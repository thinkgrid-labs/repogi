#!/usr/bin/env python3
"""Consumer smoke test for one built `repogi` executable (standard library only).

Creates two disposable TypeScript repositories, indexes each, and talks to
`repogi mcp` over stdio. Checks: the reported version, exactly three Navigation
tools, that each server answers only from its own --root, that each index holds
only its own files, that a two-root server requires an explicit root, and that
indexing and queries never run project code (a canary script must not fire).
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile

NAVIGATION = ['atlas_context', 'atlas_dependencies', 'atlas_references']
CANARY = 'CANARY-EXECUTED'


def write_repo(root: Path, word: str, canary: Path) -> None:
    src = root / 'src'
    src.mkdir(parents=True)
    hook = f"echo {CANARY} > '{canary}'"
    (root / 'package.json').write_text(json.dumps({
        'name': f'{word}-fixture', 'private': True,
        'scripts': {'preinstall': hook, 'postinstall': hook, 'prepare': hook, 'build': hook}}))
    (root / 'tsconfig.json').write_text('{"compilerOptions":{"strict":true}}\n')
    (src / f'{word}.ts').write_text(
        f"import {{ writeFileSync }} from 'fs';\n"
        f"writeFileSync({json.dumps(str(canary))}, '{CANARY}');\n"
        f"export function {word}Handler(input: string): string {{\n  return input.trim();\n}}\n")
    (src / 'main.ts').write_text(
        f"import {{ {word}Handler }} from './{word}';\n"
        f"export function run{word.capitalize()}(): string {{\n  return {word}Handler(' {word} ');\n}}\n")
    if shutil.which('git'):
        subprocess.run(['git', 'init', '-q', str(root)], check=True)


class Server:
    def __init__(self, exe: Path, roots: list[Path], cwd: Path):
        args = [str(exe), 'mcp', '--profile', 'compact', '--tools', ','.join(NAVIGATION)]
        for root in roots:
            args += ['--root', str(root)]
        self.proc = subprocess.Popen(args, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True, encoding='utf-8')
        self.next_id = 0

    def request(self, method: str, params: dict | None = None) -> dict:
        self.next_id += 1
        msg = {'jsonrpc': '2.0', 'id': self.next_id, 'method': method}
        if params is not None:
            msg['params'] = params
        self.proc.stdin.write(json.dumps(msg) + '\n')
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise AssertionError(f'server closed stdout; stderr: {self.proc.stderr.read()}')
        reply = json.loads(line)
        assert reply.get('id') == self.next_id, reply
        return reply

    def notify(self, method: str) -> None:
        self.proc.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': method}) + '\n')
        self.proc.stdin.flush()

    def call(self, tool: str, arguments: dict) -> tuple[bool, str]:
        result = self.request('tools/call', {'name': tool, 'arguments': arguments})
        if 'error' in result:
            return True, json.dumps(result['error'])
        body = result['result']
        return bool(body.get('isError')), '\n'.join(c.get('text', '') for c in body.get('content', []))

    def close(self) -> None:
        self.proc.stdin.close()
        self.proc.wait(timeout=30)
        assert self.proc.returncode == 0, f'server exited {self.proc.returncode}: {self.proc.stderr.read()}'


def initialize(server: Server) -> dict:
    reply = server.request('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                          'clientInfo': {'name': 'repogi-release-smoke', 'version': '1'}})
    server.notify('notifications/initialized')
    return reply['result']


def indexed_paths(root: Path) -> list[str]:
    db = root / '.code-intel' / 'index.db'
    with sqlite3.connect(f'{db.as_uri()}?mode=ro', uri=True) as conn:
        tables = [r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")]
        table = next(t for t in ('files', 'file') if t in tables)
        column = next(c[1] for c in conn.execute(f'PRAGMA table_info({table})') if c[1] in ('path', 'rel_path'))
        return sorted(r[0] for r in conn.execute(f'SELECT {column} FROM {table}'))


def run(exe: Path, version: str | None, work: Path) -> dict:
    checks = {}
    out = subprocess.run([str(exe), '--version'], check=True, capture_output=True, text=True).stdout.strip()
    if version:
        assert out == f'repogi {version}', f'--version printed {out!r}, expected repogi {version}'
    checks['version'] = out
    canary = work / 'canary.txt'
    alpha, beta = work / 'repo alpha', work / 'repo beta'
    write_repo(alpha, 'alpha', canary)
    write_repo(beta, 'beta', canary)
    for repo in (alpha, beta):
        report = json.loads(subprocess.run([str(exe), 'index', str(repo)], check=True,
                                           capture_output=True, text=True).stdout)
        checks[f'index {repo.name}'] = {k: report[k] for k in report if isinstance(report[k], (int, str))}
    for repo, own, other in ((alpha, 'alpha', 'beta'), (beta, 'beta', 'alpha')):
        paths = indexed_paths(repo)
        assert paths and all(other not in p for p in paths), f'{repo.name} index holds {paths}'
        assert any(own in p for p in paths), f'{repo.name} index misses its own files: {paths}'
        checks[f'index paths {repo.name}'] = paths

    for repo, own, other in ((alpha, 'alpha', 'beta'), (beta, 'beta', 'alpha')):
        server = Server(exe, [repo], cwd=work)
        info = initialize(server)
        assert info['serverInfo']['name'] == 'repogi', info
        tools = sorted(t['name'] for t in server.request('tools/list')['result']['tools'])
        assert tools == NAVIGATION, f'navigation tools: {tools}'
        err, text = server.call('atlas_context', {'task': f'{own}Handler'})
        assert not err and f'{own}Handler' in text, text
        assert other not in text, f'{repo.name} answered with {other} data: {text}'
        err, text = server.call('atlas_context', {'task': f'{other}Handler'})
        assert f'{other}Handler' not in text, f'{repo.name} leaked {other}: {text}'
        err, text = server.call('atlas_references', {'name': f'{own}Handler'})
        assert not err and 'main.ts' in text, text
        err, text = server.call('atlas_context', {'task': f'{other}Handler', 'root': str(beta if own == 'alpha' else alpha)})
        assert err and f'{other}Handler' not in text, f'unconfigured root accepted: {text}'
        server.close()
        checks[f'mcp {repo.name}'] = {'tools': tools, 'serverInfo': info['serverInfo']}

    server = Server(exe, [alpha, beta], cwd=work)
    initialize(server)
    err, text = server.call('atlas_context', {'task': 'alphaHandler'})
    assert err and 'root' in text, f'two-root server answered without a root: {text}'
    for repo, own, other in ((alpha, 'alpha', 'beta'), (beta, 'beta', 'alpha')):
        err, text = server.call('atlas_context', {'task': f'{own}Handler', 'root': str(repo)})
        assert not err and f'{own}Handler' in text and f'{other}Handler' not in text, text
    server.close()
    checks['two-root server requires root'] = True

    assert not canary.exists(), 'project code ran during indexing or queries'
    checks['no project code executed'] = True
    return checks


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--version', help='expected version, e.g. 0.1.0')
    parser.add_argument('--keep', action='store_true', help='keep the disposable repositories')
    args = parser.parse_args()
    work = Path(tempfile.mkdtemp(prefix='repogi smoke '))
    try:
        checks = run(args.binary.resolve(), args.version, work)
    finally:
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)
    print(json.dumps({'binary': str(args.binary), 'passed': True, 'checks': checks}, indent=2))


if __name__ == '__main__':
    main()
