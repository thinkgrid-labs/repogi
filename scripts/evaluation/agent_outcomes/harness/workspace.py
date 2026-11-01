"""Disposable Git workspaces. Each session gets a fresh copy of the fixture."""
from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

# Local state that is neither the agent's change nor part of the task.
STATE = ('.code-intel', '.repogi', '.grader', '.claude', '.git')
GIT_ENV = {
    'GIT_AUTHOR_NAME': 'benchmark', 'GIT_AUTHOR_EMAIL': 'benchmark@localhost',
    'GIT_COMMITTER_NAME': 'benchmark', 'GIT_COMMITTER_EMAIL': 'benchmark@localhost',
    'GIT_AUTHOR_DATE': '2026-01-01T00:00:00Z', 'GIT_COMMITTER_DATE': '2026-01-01T00:00:00Z',
    'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
}


def git(workspace: Path, *args: str) -> str:
    return subprocess.run(['git', *args], cwd=workspace, check=True, capture_output=True, text=True,
                          env={**os.environ, **GIT_ENV}).stdout


def create(fixture: Path, parent: Path | None = None) -> Path:
    """Copy the fixture into a new directory and commit it as the task snapshot."""
    workspace = Path(tempfile.mkdtemp(prefix='repogi-outcome-', dir=parent)).resolve()
    shutil.copytree(fixture, workspace, dirs_exist_ok=True, ignore=shutil.ignore_patterns(*STATE))
    git(workspace, 'init', '--quiet', '--initial-branch=main')
    exclude = workspace / '.git/info/exclude'
    exclude.write_text(exclude.read_text() + ''.join(f'/{name}/\n' for name in STATE[:-1]))
    git(workspace, 'add', '--all')
    git(workspace, 'commit', '--quiet', '--no-gpg-sign', '-m', 'task snapshot')
    return workspace


def changed_files(workspace: Path) -> list[str]:
    """Tracked modifications, deletions and untracked files since the snapshot."""
    out = git(workspace, 'status', '--porcelain=v1', '-z', '--untracked-files=all')
    paths = set()
    entries = out.split('\0')
    i = 0
    while i < len(entries):
        entry = entries[i]
        i += 1
        if len(entry) < 4:
            continue
        status, path = entry[:2], entry[3:]
        paths.add(path)
        if 'R' in status or 'C' in status:
            paths.add(entries[i])
            i += 1
    return sorted(paths)


def snapshot_lines(workspace: Path, path: str) -> int | None:
    """Line count of `path` in the committed task snapshot, or None if absent."""
    try:
        return len(git(workspace, 'show', f'HEAD:{path}').splitlines())
    except subprocess.CalledProcessError:
        return None


def diff(workspace: Path) -> str:
    git(workspace, 'add', '--intent-to-add', '--all')
    return git(workspace, 'diff', '--no-color', 'HEAD')


def remove(workspace: Path) -> None:
    shutil.rmtree(workspace, ignore_errors=True)
