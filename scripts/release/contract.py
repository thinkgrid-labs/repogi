"""Release asset naming and payload contract shared by the release scripts.

Archive: repogi-<version>-<target>.tar.gz (Windows: .zip), one top-level
directory with the same stem. Checksums: SHA256SUMS, `sha256sum -c` format.
The installers implement the same naming independently; see docs/RELEASES.md.
"""
from __future__ import annotations

from pathlib import Path, PurePosixPath
import re

REPOSITORY = 'thinkgrid-labs/repogi'
CHECKSUMS = 'SHA256SUMS'
INSTALLERS = ('install.sh', 'install.ps1')

# Every published target. A target is listed here only if the release workflow
# builds, tests and runs it on a native runner of that platform.
TARGETS = {
    'x86_64-unknown-linux-gnu': {'os': 'linux', 'arch': 'x86_64', 'format': 'tar.gz'},
    'aarch64-unknown-linux-gnu': {'os': 'linux', 'arch': 'aarch64', 'format': 'tar.gz'},
    'x86_64-apple-darwin': {'os': 'macos', 'arch': 'x86_64', 'format': 'tar.gz'},
    'aarch64-apple-darwin': {'os': 'macos', 'arch': 'aarch64', 'format': 'tar.gz'},
    'x86_64-pc-windows-msvc': {'os': 'windows', 'arch': 'x86_64', 'format': 'zip'},
}

# Highest glibc symbol version a Linux binary may require. The installer
# refuses older glibc with the same floor.
GLIBC_FLOOR = (2, 34)

VERSION_RE = re.compile(r'^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?$')

# Files copied from the source tree into every archive, besides licenses/**.
ROOT_MEMBERS = ('LICENSE', 'THIRD_PARTY_NOTICES.md')
GENERATED_MEMBERS = ('BUILD-INFO.json', 'README.txt')

# Defence in depth on top of the allowlist: nothing internal ever ships.
FORBIDDEN_PARTS = {
    '.git', 'target', 'benchmarks', '.code-intel', '__pycache__', 'internal',
    'sessions', 'recordings', 'evaluations', 'preserved', 'docs', 'fixtures',
}
FORBIDDEN_NAME = re.compile(r'PHASE\d|BENCHMARK|EVALUATION|OWNER-REVIEW|RELEASE-CHECKLIST|PRD\.md|AGENTS\.md', re.I)


def check_version(version: str) -> str:
    if not VERSION_RE.match(version):
        raise ValueError(f'not a release version (expected MAJOR.MINOR.PATCH[-PRE]): {version!r}')
    return version


def is_prerelease(version: str) -> bool:
    return '-' in check_version(version)


def executable(target: str) -> str:
    return 'repogi.exe' if TARGETS[target]['os'] == 'windows' else 'repogi'


def stem(version: str, target: str) -> str:
    if target not in TARGETS:
        raise ValueError(f'unsupported target: {target}')
    return f'repogi-{check_version(version)}-{target}'


def archive_name(version: str, target: str) -> str:
    return f'{stem(version, target)}.{TARGETS[target]["format"]}'


def release_assets(version: str) -> list[str]:
    """Every asset a complete release carries, checksum manifest last."""
    return sorted([archive_name(version, t) for t in TARGETS] + list(INSTALLERS)) + [CHECKSUMS]


def license_files(root: Path) -> list[str]:
    """Public notice files under licenses/, as listed in the source manifest."""
    listed = (root / 'dist/source-files.txt').read_text().splitlines()
    return [p for p in listed if p.startswith('licenses/')]


def check_member(name: str, version: str, target: str, licenses: set[str]) -> None:
    """Reject any archive member outside the payload allowlist."""
    top = stem(version, target)
    path = PurePosixPath(name.rstrip('/'))
    if (name.startswith('/') or '\\' in name or '..' in path.parts
            or not path.parts or path.parts[0] != top):
        raise ValueError(f'unsafe or foreign archive member: {name!r}')
    rel = PurePosixPath(*path.parts[1:]) if len(path.parts) > 1 else None
    if rel is None:
        return
    text = str(rel)
    if FORBIDDEN_PARTS.intersection(rel.parts) or FORBIDDEN_NAME.search(text):
        raise ValueError(f'internal material in archive: {name!r}')
    if name.endswith('/'):
        if text != 'licenses' and not any(f.startswith(text + '/') for f in licenses):
            raise ValueError(f'unexpected directory in archive: {name!r}')
        return
    allowed = {executable(target), *ROOT_MEMBERS, *GENERATED_MEMBERS}
    if text not in allowed and text not in licenses:
        raise ValueError(f'unexpected file in archive: {name!r}')
