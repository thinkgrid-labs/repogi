#!/usr/bin/env python3
"""Check an installed `repogi` with the official MCP Python SDK (`pip install mcp`).

Complements smoke.py (raw JSON-RPC) with a real SDK client: stdio
initialization, exactly three Navigation tools, answers confined to each
server's --root across two disposable repositories, and a two-root server that
requires an explicit root. Starts no coding agent and makes no model calls.
"""
from __future__ import annotations

import argparse
import asyncio
from importlib.metadata import version as package_version
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from mcp import Client, StdioServerParameters
from mcp.client.stdio import stdio_client

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from smoke import CANARY, NAVIGATION, write_repo  # noqa: E402


def text_of(result) -> str:
    return '\n'.join(getattr(c, 'text', '') for c in result.content)


def session(exe: Path, roots: list[Path], cwd: Path, log: Path):
    args = ['mcp', '--profile', 'compact', '--tools', ','.join(NAVIGATION)]
    for root in roots:
        args += ['--root', str(root)]
    params = StdioServerParameters(command=str(exe), args=args, cwd=str(cwd))
    return params, log.open('a')


async def check(exe: Path, work: Path) -> dict:
    canary = work / 'canary.txt'
    alpha, beta = work / 'repo alpha', work / 'repo beta'
    write_repo(alpha, 'alpha', canary)
    write_repo(beta, 'beta', canary)
    for repo in (alpha, beta):
        subprocess.run([str(exe), 'index', str(repo)], check=True, capture_output=True)
    results = {'sdk': package_version('mcp')}
    for repo, own, other in ((alpha, 'alpha', 'beta'), (beta, 'beta', 'alpha')):
        params, errlog = session(exe, [repo], work, work / 'server.log')
        with errlog:
            async with Client(stdio_client(params, errlog=errlog), mode='legacy', read_timeout_seconds=60) as client:
                tools = sorted(t.name for t in (await client.list_tools()).tools)
                assert tools == NAVIGATION, tools
                found = await client.call_tool('atlas_context', {'task': f'{own}Handler'})
                assert not found.is_error and f'{own}Handler' in text_of(found), text_of(found)
                leaked = await client.call_tool('atlas_context', {'task': f'{other}Handler'})
                assert f'{other}Handler' not in text_of(leaked), text_of(leaked)
                refs = await client.call_tool('atlas_references', {'name': f'{own}Handler'})
                assert not refs.is_error and 'main.ts' in text_of(refs), text_of(refs)
                deps = await client.call_tool('atlas_dependencies', {'name': f'{own}Handler', 'direction': 'upstream'})
                assert other not in text_of(deps).replace(str(work), ''), text_of(deps)
        results[repo.name] = {'tools': tools, 'context_bytes': len(text_of(found).encode())}
    params, errlog = session(exe, [alpha, beta], work, work / 'server.log')
    with errlog:
        async with Client(stdio_client(params, errlog=errlog), mode='legacy', read_timeout_seconds=60) as client:
            missing = await client.call_tool('atlas_context', {'task': 'alphaHandler'})
            assert missing.is_error and 'root' in text_of(missing), text_of(missing)
            for repo, own, other in ((alpha, 'alpha', 'beta'), (beta, 'beta', 'alpha')):
                hit = await client.call_tool('atlas_context', {'task': f'{own}Handler', 'root': str(repo)})
                assert not hit.is_error and f'{own}Handler' in text_of(hit) and f'{other}Handler' not in text_of(hit)
    results['two-root server requires root'] = True
    assert not canary.exists(), f'{CANARY}: project code ran'
    results['no project code executed'] = True
    return results


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    args = parser.parse_args()
    work = Path(tempfile.mkdtemp(prefix='repogi sdk '))
    try:
        results = asyncio.run(check(args.binary.resolve(), work))
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print(json.dumps({'binary': str(args.binary), 'passed': True, **results}, indent=2))


if __name__ == '__main__':
    main()
