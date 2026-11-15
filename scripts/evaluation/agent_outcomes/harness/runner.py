"""Plans and runs paired sessions; results are append-only JSON lines."""
from __future__ import annotations

import hashlib
import json
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from . import CONDITIONS, HARNESS, ROOT, adapters, grading, suite as suites, workspace as ws

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


def _command(*args: str) -> str | None:
    try:
        out = subprocess.run(list(args), capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return out.stdout.strip() or None if out.returncode == 0 else None


def source_state() -> dict:
    """Repogi source commit at planning or run time (provenance; content is pinned by digests)."""
    commit = _command('git', '-C', str(ROOT), 'rev-parse', 'HEAD')
    status = _command('git', '-C', str(ROOT), 'status', '--porcelain', '--', str(HARNESS),
                      str(ROOT / 'fixtures/agent-outcomes'))
    return {'commit': commit, 'harness_or_fixture_dirty': bool(status)}


def grader_environment() -> dict:
    """Tools the graders run; a different Node.js can change hidden-test outcomes."""
    node = shutil.which('node')
    return {'node': _command(node, '--version') if node else None}


def mcp_surface(binary: Path, tools: str, fixture: Path) -> dict:
    """What the Repogi condition adds to the agent's context: server instructions and tool schemas."""
    messages = [
        {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {
            'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'plan', 'version': '0'}}},
        {'jsonrpc': '2.0', 'method': 'notifications/initialized'},
        {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list', 'params': {}},
    ]
    workspace = ws.create(fixture)
    try:
        out = subprocess.run([str(binary), 'mcp', '--root', str(workspace), '--profile', 'compact', '--tools', tools],
                             input=''.join(json.dumps(m) + '\n' for m in messages), capture_output=True,
                             text=True, timeout=60)
        replies = {}
        for line in out.stdout.splitlines():
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(message, dict) and message.get('id') in (1, 2):
                replies[message['id']] = message.get('result')
        if not replies.get(1) or not replies.get(2):
            return {'error': f'no initialize/tools/list reply (exit {out.returncode}): {out.stderr[-300:]}'}
        instructions = replies[1].get('instructions') or ''
        listed = replies[2].get('tools') or []
        return {'tools': [t.get('name') for t in listed],
                'instructions_sha256': hashlib.sha256(instructions.encode()).hexdigest(),
                'tools_sha256': hashlib.sha256(canonical(listed)).hexdigest(),
                'context_chars': len(instructions) + len(json.dumps(listed))}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {'error': str(error)}
    finally:
        ws.remove(workspace)


def build_plan(suite: dict, task_ids, adapter: adapters.Adapter, *, repogi: Path, repeats: int, seed: int,
               limits: dict, repogi_tools: str = DEFAULT_REPOGI_TOOLS, draft: list[str] | None = None) -> dict:
    tasks = suites.select(suite, task_ids)
    rng = random.Random(seed)
    sessions = []
    # One block per (task, repeat). Counterbalanced: each task alternates which
    # condition goes first across repeats, and tasks alternate within a repeat,
    # so neither condition systematically goes first. Block order is shuffled.
    order_tasks = [task['id'] for task in tasks]
    rng.shuffle(order_tasks)
    offset = {tid: index % 2 for index, tid in enumerate(order_tasks)}
    blocks = [(task['id'], r) for r in range(1, repeats + 1) for task in tasks]
    rng.shuffle(blocks)
    for index, (tid, repeat) in enumerate(blocks):
        first = (offset[tid] + repeat - 1) % 2
        order = list(CONDITIONS) if first == 0 else list(reversed(CONDITIONS))
        for position, condition in enumerate(order):
            sessions.append({'key': f'{tid}#r{repeat}#{condition}', 'task': tid, 'repeat': repeat,
                             'condition': condition, 'block': index, 'position': position})
    plan = {
        'schema': PLAN_SCHEMA,
        'suite': {'id': suite['id'], **suites.digest(suite)},
        'adapter': adapter.describe(),
        'conditions': list(CONDITIONS),
        'repogi': {'binary': str(repogi), 'sha256': file_sha256(repogi), 'tools': repogi_tools,
                   'mcp_surface': mcp_surface(repogi, repogi_tools, Path(suite['_fixture']))},
        'allowed_tools': BASE_TOOLS,
        'disallowed_tools': {'both_conditions': list(adapters.NO_NETWORK_TOOLS),
                             'read_edit_denied': ['<repogi source tree>', '<suite directory>', '<output directory>']},
        'source': source_state(),
        'grader_environment': grader_environment(),
        'repeats': repeats,
        'seed': seed,
        'limits': limits,
        'sessions': sessions,
    }
    if draft:
        plan['draft'] = {'pending': draft, 'note': 'not runnable; re-plan with every value pinned to authorize'}
    plan['digest'] = plan_digest(plan)
    return plan


def verify_plan(plan: dict, suite: dict) -> list[str]:
    problems = []
    if plan.get('schema') != PLAN_SCHEMA:
        problems.append('unknown plan schema')
    if plan.get('digest') != plan_digest(plan):
        problems.append('plan digest does not match its contents')
    if plan.get('draft'):
        problems.append(f"draft plan: pin {plan['draft'].get('pending')} and re-plan before running")
    if plan.get('grader_environment', {}).get('node') != grader_environment()['node']:
        problems.append('stale plan: grader Node.js version changed since planning')
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


def deny_paths(suite: dict, out: Path) -> list[str]:
    return sorted({str(ROOT), str(Path(suite['_dir']).resolve()), str(out.resolve())})


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
            try:
                report = json.loads(result.stdout)
            except json.JSONDecodeError as error:
                raise RuntimeError(f'repogi index printed no JSON report: {error}') from None
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
    if adapter.live:
        temp = Path(tempfile.gettempdir()).resolve()
        for denied in (ROOT, out.resolve()):
            if temp.is_relative_to(denied):
                problems.append(f'workspaces would be created under {denied}, which the agent may not read; '
                                'set TMPDIR elsewhere')
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
    limits = plan['limits']
    budget, cap = limits.get('total_budget_usd'), limits.get('max_budget_usd_per_session')
    committed = sum(budget_charge(r, cap) for r in load_results(results))
    max_duration, started = limits.get('max_total_duration_s'), time.monotonic()
    for session in plan['sessions']:
        prior = previous.get(session['key'])
        if prior and not (retry_infrastructure and prior['outcome'] == 'infrastructure_failure'):
            continue
        if (out / 'STOP').exists():
            log(f'stopping: {out / "STOP"} exists; remove it and re-run to resume')
            break
        # Never start a session that could take spend past the total if it used its whole cap.
        if adapter.live and budget is not None and committed + (cap or 0) > budget:
            log(f'stopping: committed spend ${committed:.4f} + session cap ${cap} would exceed the plan budget ${budget}')
            break
        if max_duration is not None and time.monotonic() - started + limits.get('timeout_s', 600) > max_duration:
            log(f'stopping: another session could exceed max_total_duration_s={max_duration}')
            break
        record = run_session(plan, suite, tasks[session['task']], session, adapter, out, keep_workspaces)
        record['attempt'] = (prior or {}).get('attempt', 0) + 1
        committed += budget_charge(record, cap)
        with results.open('a') as handle:
            handle.write(json.dumps(record, sort_keys=True) + '\n')
        log(f"{session['key']}: {record['outcome']}")
        if record.get('halt'):
            log(f"stopping: {record['halt']}")
            break
    return results


def budget_charge(record: dict, cap: float | None) -> float:
    """Spend counted against the total budget. Reported cost when present; a live session
    that ran the agent but reported no cost is charged its full cap. This bound is only
    for stopping; reports keep the missing cost missing."""
    cost = (record.get('telemetry') or {}).get('cost_usd')
    if cost is not None:
        return cost
    if record.get('mode') == 'live' and 'elapsed_s' in record:
        return cap or 0
    return 0


def provenance(plan: dict) -> dict:
    agent = plan['adapter']
    return {
        'source_commit': plan.get('source', {}).get('commit'),
        'run_source_commit': source_state()['commit'],
        'suite_digest': hashlib.sha256(canonical(plan['suite'])).hexdigest(),
        'repogi_sha256': plan['repogi']['sha256'],
        'agent': {k: agent.get(k) for k in ('name', 'version', 'executable_sha256', 'model')},
        'grader_node': plan.get('grader_environment', {}).get('node'),
        'python': sys.version.split()[0],
    }


def condition_problem(plan: dict, condition: str, transcript: adapters.Transcript) -> tuple[str | None, bool]:
    """Why a live session does not count as its condition (and whether to halt the run)."""
    servers = transcript.mcp_servers
    if servers is None:
        return 'agent reported no init event; condition and model unverified', False
    if condition == 'baseline' and servers:
        return f'baseline session had MCP servers {sorted(servers)}', True
    if condition == 'repogi' and servers.get('repogi') != 'connected':
        return f"Repogi MCP server did not connect (status {servers.get('repogi')!r})", False
    reported, planned = transcript.agent.get('model'), plan['adapter'].get('model')
    if reported != planned:
        return f'agent reported model {reported!r}, plan pins {planned!r}', True
    version = transcript.agent.get('version')
    if not version or version not in (plan['adapter'].get('version') or ''):
        return f"agent reported version {version!r}, plan pins {plan['adapter'].get('version')!r}", True
    return None, False


def run_session(plan, suite, task, session, adapter, out, keep_workspaces) -> dict:
    workspace = ws.create(Path(suite['_fixture']))
    record = {'schema': RESULT_SCHEMA, 'plan_digest': plan['digest'], **session,
              'category': task['category'], 'adapter': adapter.name, 'mode': 'live' if adapter.live else 'mock',
              'started_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'provenance': provenance(plan)}
    denied = deny_paths(suite, out)
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
            allowed_tools=allowed_tools(plan, session['condition']), limits=plan['limits'], raw_log=raw,
            deny_paths=denied))
        record.update(
            elapsed_s=elapsed, stop_reason=transcript.stop_reason, telemetry=transcript.telemetry,
            telemetry_source=transcript.telemetry_source, tools=transcript.tools,
            files_read=len(transcript.files_read), repogi_calls=transcript.repogi_calls,
            search_calls=transcript.search_calls, agent=transcript.agent,
            raw_log=raw.relative_to(out).as_posix(), answer=transcript.answer[-4000:])
        record['diff_sha256'] = hashlib.sha256(ws.diff(workspace).encode()).hexdigest()
        if transcript.stop_reason == 'infrastructure':
            record.update(outcome='infrastructure_failure', error=transcript.error or 'agent did not start')
            return record
        if adapter.live:
            problem, halt = condition_problem(plan, session['condition'], transcript)
            if problem:
                record.update(outcome='infrastructure_failure', error=problem)
                if halt:
                    record['halt'] = problem
                return record
        graded = grading.grade(task, Path(suite['_dir']), workspace, transcript.answer)
        record['grading'] = graded
        outcome = graded['outcome']
        if outcome == 'failed' and transcript.stop_reason in ('timeout', 'budget', 'turn_limit'):
            outcome = 'incomplete'
        leaks = adapters.forbidden_access(transcript.tool_inputs, denied)
        if leaks:
            # The agent tried to reach grader material or another session's results: not gradable.
            record['contamination'] = leaks
            outcome = 'indeterminate'
        record['outcome'] = outcome
        if transcript.error:
            record['error'] = transcript.error
        return record
    finally:
        if keep_workspaces:
            # Kept workspaces hold copied hidden tests: move them under the output directory,
            # which later sessions may not read.
            kept = out / 'workspaces' / session['key'].replace('#', '__')
            kept.parent.mkdir(exist_ok=True)
            shutil.rmtree(kept, ignore_errors=True)
            shutil.move(str(workspace), kept)
            record['workspace'] = str(kept)
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


