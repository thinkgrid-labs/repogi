"""Plans and runs paired sessions; results are append-only JSON lines."""
from __future__ import annotations

import hashlib
import json
import random
import shutil
import subprocess
import time
from pathlib import Path

from . import CONDITIONS, ROOT, adapters, grading, suite as suites, workspace as ws

PLAN_SCHEMA = 'repogi-agent-outcomes-plan/1'
RESULT_SCHEMA = 'repogi-agent-outcomes-result/1'
BASE_TOOLS = ['Read', 'Edit', 'Write', 'Glob', 'Grep', 'Bash(node --test:*)', 'Bash(git diff:*)', 'Bash(git status:*)']
DEFAULT_REPOGI_TOOLS = 'atlas_context,atlas_references,atlas_dependencies,atlas_knowledge_trace,atlas_investigate'


class PlanError(RuntimeError):
    pass


def canonical(value) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def plan_digest(plan: dict) -> str:
    return hashlib.sha256(canonical({k: v for k, v in plan.items() if k != 'digest'})).hexdigest()


def file_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_plan(suite: dict, task_ids, adapter: adapters.Adapter, *, repogi: Path, repeats: int, seed: int,
               limits: dict, repogi_tools: str = DEFAULT_REPOGI_TOOLS) -> dict:
    tasks = suites.select(suite, task_ids)
    rng = random.Random(seed)
    sessions = []
    # One block per (task, repeat); condition order alternates and is shuffled
    # within the run so neither condition systematically goes first.
    blocks = [(task['id'], r) for r in range(1, repeats + 1) for task in tasks]
    rng.shuffle(blocks)
    for index, (tid, repeat) in enumerate(blocks):
        order = list(CONDITIONS) if index % 2 == 0 else list(reversed(CONDITIONS))
        for position, condition in enumerate(order):
            sessions.append({'key': f'{tid}#r{repeat}#{condition}', 'task': tid, 'repeat': repeat,
                             'condition': condition, 'block': index, 'position': position})
    plan = {
        'schema': PLAN_SCHEMA,
        'suite': {'id': suite['id'], **suites.digest(suite)},
        'adapter': adapter.describe(),
        'conditions': list(CONDITIONS),
        'repogi': {'binary': str(repogi), 'sha256': file_sha256(repogi), 'tools': repogi_tools},
        'allowed_tools': BASE_TOOLS,
        'repeats': repeats,
        'seed': seed,
        'limits': limits,
        'sessions': sessions,
    }
    plan['digest'] = plan_digest(plan)
    return plan


def verify_plan(plan: dict, suite: dict) -> list[str]:
    problems = []
    if plan.get('schema') != PLAN_SCHEMA:
        problems.append('unknown plan schema')
    if plan.get('digest') != plan_digest(plan):
        problems.append('plan digest does not match its contents')
    current = suites.digest(suite)
    for key, value in current.items():
        if plan['suite'].get(key) != value:
            problems.append(f'stale plan: {key} changed since planning')
    binary = Path(plan['repogi']['binary'])
    if not binary.is_file() or file_sha256(binary) != plan['repogi']['sha256']:
        problems.append('stale plan: Repogi binary missing or changed')
    return problems


def mcp_config(plan: dict, condition: str, workspace: Path) -> dict:
    if condition == 'baseline':
        return {'mcpServers': {}}
    return {'mcpServers': {'repogi': {
        'type': 'stdio', 'command': plan['repogi']['binary'],
        'args': ['mcp', '--root', str(workspace), '--profile', 'compact', '--tools', plan['repogi']['tools']],
    }}}


def allowed_tools(plan: dict, condition: str) -> list[str]:
    return plan['allowed_tools'] + (['mcp__repogi'] if condition == 'repogi' else [])


def index(binary: str, workspace: Path) -> dict:
    """Cold index, then an unchanged re-index (warm). Timed separately from the agent."""
    timings = {}
    for label in ('cold', 'warm'):
        started = time.monotonic()
        result = subprocess.run([binary, 'index', str(workspace)], capture_output=True, text=True, timeout=600)
        timings[f'{label}_s'] = round(time.monotonic() - started, 3)
        if result.returncode != 0:
            raise RuntimeError(f'repogi index failed: {result.stderr[-500:]}')
        if label == 'cold':
            report = json.loads(result.stdout)
            timings['files'] = report.get('files_scanned')
            timings['documents'] = (report.get('documents') or {}).get('scanned')
    timings['index_bytes'] = (workspace / '.code-intel/index.db').stat().st_size
    return timings


def load_results(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def latest(records: list[dict]) -> dict[str, dict]:
    by_key = {}
    for record in records:
        by_key[record['key']] = record
    return by_key


def run(plan: dict, suite: dict, adapter: adapters.Adapter, out: Path, *, authorize_live: str | None = None,
        retry_infrastructure: bool = False, keep_workspaces: bool = False, log=print) -> Path:
    problems = verify_plan(plan, suite)
    if adapter.describe()['name'] != plan['adapter']['name']:
        problems.append('adapter differs from the plan')
    if adapter.live:
        if authorize_live != plan['digest']:
            problems.append('live adapter: pass --authorize-live with the exact plan digest after review')
        problems += adapter.preflight()
        if adapter.describe() != plan['adapter']:
            problems.append('live adapter version or model differs from the plan')
    if problems:
        raise PlanError('; '.join(problems))
    out.mkdir(parents=True, exist_ok=True)
    (out / 'raw').mkdir(exist_ok=True)
    plan_file = out / 'plan.json'
    if plan_file.exists() and json.loads(plan_file.read_text()).get('digest') != plan['digest']:
        raise PlanError(f'{out} holds results for a different plan; use a new output directory')
    plan_file.write_text(json.dumps(plan, indent=2) + '\n')
    results = out / 'results.jsonl'
    previous = latest(load_results(results))
    tasks = {t['id']: t for t in suite['tasks']}
    spent = sum((r.get('telemetry') or {}).get('cost_usd') or 0 for r in load_results(results))
    budget = plan['limits'].get('total_budget_usd')
    for session in plan['sessions']:
        prior = previous.get(session['key'])
        if prior and not (retry_infrastructure and prior['outcome'] == 'infrastructure_failure'):
            continue
        if adapter.live and budget is not None and spent >= budget:
            log(f'stopping: measured spend ${spent:.4f} reached the plan budget ${budget}')
            break
        record = run_session(plan, suite, tasks[session['task']], session, adapter, out, keep_workspaces)
        record['attempt'] = (prior or {}).get('attempt', 0) + 1
        spent += (record.get('telemetry') or {}).get('cost_usd') or 0
        with results.open('a') as handle:
            handle.write(json.dumps(record, sort_keys=True) + '\n')
        log(f"{session['key']}: {record['outcome']}")
    return results


def run_session(plan, suite, task, session, adapter, out, keep_workspaces) -> dict:
    workspace = ws.create(Path(suite['_fixture']))
    record = {'schema': RESULT_SCHEMA, 'plan_digest': plan['digest'], **session,
              'category': task['category'], 'adapter': adapter.name, 'mode': 'live' if adapter.live else 'mock',
              'started_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}
    try:
        if session['condition'] == 'repogi':
            try:
                record['indexing'] = index(plan['repogi']['binary'], workspace)
            except (RuntimeError, OSError, subprocess.TimeoutExpired) as error:
                record.update(outcome='infrastructure_failure', error=f'indexing: {error}')
                return record
        raw = out / 'raw' / f"{session['key'].replace('#', '__')}.jsonl"
        transcript, elapsed = adapters.wall_clock(adapter.run, adapters.Session(
            task=task, condition=session['condition'], workspace=workspace, prompt=task['prompt'],
            mcp_config=mcp_config(plan, session['condition'], workspace),
            allowed_tools=allowed_tools(plan, session['condition']), limits=plan['limits'], raw_log=raw))
        record.update(
            elapsed_s=elapsed, stop_reason=transcript.stop_reason, telemetry=transcript.telemetry,
            telemetry_source=transcript.telemetry_source, tools=transcript.tools,
            files_read=len(transcript.files_read), repogi_calls=transcript.repogi_calls,
            search_calls=transcript.search_calls, agent=transcript.agent,
            raw_log=raw.relative_to(out).as_posix(), answer=transcript.answer[-4000:])
        record['diff_sha256'] = hashlib.sha256(ws.diff(workspace).encode()).hexdigest()
        if transcript.stop_reason == 'infrastructure' or (
                session['condition'] == 'repogi' and transcript.mcp_connected is False):
            record.update(outcome='infrastructure_failure',
                          error=transcript.error or 'Repogi MCP server did not connect')
            return record
        graded = grading.grade(task, Path(suite['_dir']), workspace, transcript.answer)
        record['grading'] = graded
        outcome = graded['outcome']
        if outcome == 'failed' and transcript.stop_reason in ('timeout', 'budget', 'turn_limit'):
            outcome = 'incomplete'
        record['outcome'] = outcome
        if transcript.error:
            record['error'] = transcript.error
        return record
    finally:
        if keep_workspaces:
            record['workspace'] = str(workspace)
        else:
            ws.remove(workspace)


def validate_graders(suite: dict, log=print) -> list[str]:
    """Reference solutions must pass every grader; an untouched workspace must fail."""
    problems = []
    suite_dir = Path(suite['_dir'])
    for task in suite['tasks']:
        for adapter, expected in ((adapters.MockReference(suite_dir), 'passed'), (adapters.MockNoop(), 'failed')):
            workspace = ws.create(Path(suite['_fixture']))
            raw = workspace.parent / f'{workspace.name}.log'
            try:
                transcript = adapter.run(adapters.Session(task, 'baseline', workspace, task['prompt'], {}, [], {}, raw))
                graded = grading.grade(task, suite_dir, workspace, transcript.answer)
            finally:
                ws.remove(workspace)
                raw.unlink(missing_ok=True)
            status = 'ok' if graded['outcome'] == expected else 'MISMATCH'
            log(f"{task['id']:<28} {adapter.name:<15} {graded['outcome']:<10} {status}")
            if graded['outcome'] != expected:
                problems.append(f"{task['id']}: {adapter.name} graded {graded['outcome']}, expected {expected}: "
                                + json.dumps(graded['checks']))
    return problems


def default_binary() -> Path:
    return ROOT / 'target/release/repogi'


