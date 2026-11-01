"""Agent adapters. One interface; telemetry is whatever the agent actually reports.

`mock-reference` and `mock-noop` never call a model: they apply the task's
reference solution or nothing, to validate graders and the pipeline. Their
telemetry is null by construction. `claude-code` runs the Claude Code CLI in
print mode and parses its stream-json events; it is the only live adapter.
"""
from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

TELEMETRY_FIELDS = (
    'input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens',
    'model_calls', 'tool_calls', 'cost_usd',
)


@dataclass
class Session:
    task: dict
    condition: str
    workspace: Path
    prompt: str
    mcp_config: dict
    allowed_tools: list[str]
    limits: dict
    raw_log: Path


@dataclass
class Transcript:
    answer: str = ''
    stop_reason: str = 'completed'   # completed | timeout | budget | turn_limit | agent_error | infrastructure
    error: str | None = None
    telemetry: dict = field(default_factory=lambda: dict.fromkeys(TELEMETRY_FIELDS))
    telemetry_source: str = 'none'
    tools: dict = field(default_factory=dict)     # name -> count
    files_read: list[str] = field(default_factory=list)
    repogi_calls: int | None = None
    search_calls: int | None = None
    agent: dict = field(default_factory=dict)     # reported model and version
    mcp_connected: bool | None = None


class Adapter:
    name = 'abstract'
    live = False

    def describe(self) -> dict:
        return {'name': self.name, 'live': self.live}

    def preflight(self) -> list[str]:
        return []

    def run(self, session: Session) -> Transcript:
        raise NotImplementedError


class MockReference(Adapter):
    """Applies tasks/reference/<id> over the workspace and answers from ANSWER.md."""
    name = 'mock-reference'

    def __init__(self, suite_dir: Path):
        self.reference = suite_dir / 'reference'

    def run(self, session: Session) -> Transcript:
        source = self.reference / session.task['id']
        answer = ''
        for path in sorted(p for p in source.rglob('*') if p.is_file()):
            rel = path.relative_to(source)
            if rel.as_posix() == 'ANSWER.md':
                answer = path.read_text()
                continue
            target = session.workspace / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
        session.raw_log.write_text(json.dumps({'adapter': self.name, 'applied': str(source)}) + '\n')
        return Transcript(answer=answer, telemetry_source='none (mock adapter; no model)')


class MockNoop(Adapter):
    """Changes nothing and answers nothing: every task must fail its graders."""
    name = 'mock-noop'

    def run(self, session: Session) -> Transcript:
        session.raw_log.write_text(json.dumps({'adapter': self.name}) + '\n')
        return Transcript(telemetry_source='none (mock adapter; no model)')


class ClaudeCode(Adapter):
    """Claude Code CLI in print mode with stream-json output.

    Both conditions get `--strict-mcp-config` (so user-level MCP servers never
    leak in), the same allowed tools apart from the Repogi server, project-only
    setting sources and no session persistence.
    """
    name = 'claude-code'
    live = True

    def __init__(self, executable: str, model: str, permission_mode: str = 'acceptEdits'):
        self.executable = executable
        self.model = model
        self.permission_mode = permission_mode

    def version(self) -> str | None:
        try:
            out = subprocess.run([self.executable, '--version'], capture_output=True, text=True, timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            return None
        return out.stdout.strip() or None

    def describe(self) -> dict:
        return {'name': self.name, 'live': True, 'executable': self.executable,
                'version': self.version(), 'model': self.model, 'permission_mode': self.permission_mode}

    def preflight(self) -> list[str]:
        problems = []
        if not shutil.which(self.executable) and not Path(self.executable).is_file():
            problems.append(f'claude executable not found: {self.executable}')
        if not self.model or self.model.startswith('<'):
            problems.append('a concrete model id is required')
        return problems

    def command(self, session: Session, mcp_path: Path) -> list[str]:
        command = [
            self.executable, '-p', session.prompt,
            '--output-format', 'stream-json', '--verbose',
            '--model', self.model,
            '--no-session-persistence',
            '--setting-sources', 'project',
            '--strict-mcp-config', '--mcp-config', str(mcp_path),
            '--permission-mode', self.permission_mode,
            '--allowedTools', ','.join(session.allowed_tools),
        ]
        if session.limits.get('max_budget_usd_per_session'):
            command += ['--max-budget-usd', str(session.limits['max_budget_usd_per_session'])]
        return command

    def run(self, session: Session) -> Transcript:
        mcp_path = session.raw_log.with_suffix('.mcp.json')
        mcp_path.write_text(json.dumps(session.mcp_config, indent=2))
        command = self.command(session, mcp_path)
        timeout = session.limits.get('timeout_s', 600)
        with session.raw_log.open('w') as log:
            try:
                process = subprocess.Popen(command, cwd=session.workspace, stdout=log,
                                           stderr=subprocess.PIPE, text=True, start_new_session=True)
            except OSError as error:
                return Transcript(stop_reason='infrastructure', error=str(error))
            try:
                _, stderr = process.communicate(timeout=timeout)
                timed_out = False
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                _, stderr = process.communicate()
                timed_out = True
        transcript = parse_stream(session.raw_log.read_text().splitlines())
        if timed_out:
            transcript.stop_reason = 'timeout'
        elif process.returncode != 0 and transcript.stop_reason == 'completed':
            transcript.stop_reason = 'agent_error'
            transcript.error = (stderr or '')[-2000:]
        return transcript


STOP_REASONS = {
    'success': 'completed',
    'error_max_turns': 'turn_limit',
    'error_max_budget_usd': 'budget',
    'error_during_execution': 'agent_error',
}


def parse_stream(lines: list[str]) -> Transcript:
    """Parse Claude Code stream-json. Only reported values are recorded."""
    transcript = Transcript(telemetry_source='claude-code stream-json (provider-reported usage)')
    message_ids, tools, read = set(), {}, []
    result = None
    for line in lines:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = event.get('type')
        if kind == 'system' and event.get('subtype') == 'init':
            transcript.agent = {'model': event.get('model'), 'version': event.get('claude_code_version'),
                                'tools': event.get('tools')}
            servers = {s.get('name'): s.get('status') for s in event.get('mcp_servers') or []}
            transcript.mcp_connected = servers.get('repogi') == 'connected' if 'repogi' in servers else None
        elif kind == 'assistant':
            message = event.get('message') or {}
            if message.get('id'):
                message_ids.add(message['id'])
            for item in message.get('content') or []:
                if item.get('type') == 'tool_use':
                    name = item.get('name', '?')
                    tools[name] = tools.get(name, 0) + 1
                    if name == 'Read' and isinstance(item.get('input'), dict):
                        read.append(item['input'].get('file_path', ''))
        elif kind == 'result':
            result = event
    transcript.tools = tools
    transcript.files_read = sorted(set(read))
    transcript.repogi_calls = sum(n for name, n in tools.items() if name.startswith('mcp__repogi__'))
    transcript.search_calls = sum(tools.get(name, 0) for name in ('Grep', 'Glob'))
    if result is None:
        transcript.stop_reason = 'agent_error'
        transcript.error = 'no result event in stream'
        return transcript
    usage = result.get('usage') or {}
    transcript.answer = result.get('result') or ''
    transcript.stop_reason = STOP_REASONS.get(result.get('subtype'), 'agent_error')
    if result.get('is_error') and transcript.stop_reason == 'completed':
        transcript.stop_reason = 'agent_error'
    transcript.telemetry = {
        'input_tokens': usage.get('input_tokens'),
        'output_tokens': usage.get('output_tokens'),
        'cache_creation_input_tokens': usage.get('cache_creation_input_tokens'),
        'cache_read_input_tokens': usage.get('cache_read_input_tokens'),
        'model_calls': len(message_ids) or None,
        'tool_calls': sum(tools.values()),
        'cost_usd': result.get('total_cost_usd'),
        'num_turns': result.get('num_turns'),
        'agent_duration_ms': result.get('duration_ms'),
    }
    return transcript


def make(name: str, suite_dir: Path, *, executable: str | None = None, model: str | None = None) -> Adapter:
    if name == 'mock-reference':
        return MockReference(suite_dir)
    if name == 'mock-noop':
        return MockNoop()
    if name == 'claude-code':
        return ClaudeCode(executable or shutil.which('claude') or 'claude', model or '')
    raise ValueError(f'unknown adapter {name!r}')


def wall_clock(function, *args):
    started = time.monotonic()
    value = function(*args)
    return value, round(time.monotonic() - started, 3)
