#!/usr/bin/env python3
"""Assemble and verify a complete release directory.

Takes the per-target archives produced by package.py, requires exactly one
archive per contract target, re-validates every archive member against the
payload allowlist, checks that every BUILD-INFO.json names the same version and
source commit, copies the installers, and writes SHA256SUMS. With --verify,
checks an existing release directory (asset set, checksums, members) instead.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tarfile
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_members(path: Path) -> dict[str, tuple[str, bytes | None]]:
    """Map member name -> (kind, contents for small metadata files)."""
    result = {}
    if path.suffix == '.zip':
        with zipfile.ZipFile(path) as zf:
            for info in zf.infolist():
                kind = 'dir' if info.is_dir() else 'file'
                if (info.external_attr >> 16) & 0o170000 == 0o120000:
                    kind = 'symlink'
                result[info.filename] = (kind, zf.read(info) if info.filename.endswith('BUILD-INFO.json') else None)
    else:
        with tarfile.open(path, 'r:gz') as tar:
            for info in tar.getmembers():
                kind = 'dir' if info.isdir() else 'file' if info.isreg() else 'other'
                name = info.name + ('/' if info.isdir() else '')
                data = tar.extractfile(info).read() if info.isreg() and name.endswith('BUILD-INFO.json') else None
                result[name] = (kind, data)
    return result


def check_archive(path: Path, version: str, target: str, licenses: set[str]) -> dict:
    stem = contract.stem(version, target)
    members = read_members(path)
    for name, (kind, _) in members.items():
        if kind not in ('file', 'dir'):
            raise ValueError(f'{path.name}: {name!r} is not a regular file or directory')
        contract.check_member(name, version, target, licenses)
    required = {f'{stem}/{n}' for n in (contract.executable(target), *contract.ROOT_MEMBERS,
                                        *contract.GENERATED_MEMBERS)} | {f'{stem}/{n}' for n in licenses}
    missing = sorted(required - members.keys())
    if missing:
        raise ValueError(f'{path.name}: missing members {missing[:5]}')
    info = json.loads(members[f'{stem}/BUILD-INFO.json'][1])
    if info['version'] != version or info['target'] != target:
        raise ValueError(f'{path.name}: BUILD-INFO names {info["version"]} {info["target"]}')
    return info


def assemble(args: argparse.Namespace) -> None:
    version = contract.check_version(args.version)
    licenses = set(contract.license_files(ROOT))
    found = {p.name for p in args.artifacts.rglob('*') if p.is_file() and not p.name.endswith('.BUILD-INFO.json')}
    expected = {contract.archive_name(version, t) for t in contract.TARGETS}
    if found != expected:
        raise SystemExit(f'artifact set mismatch; missing {sorted(expected - found)}, unexpected {sorted(found - expected)}')
    args.out.mkdir(parents=True, exist_ok=False)
    commits, report = set(), {}
    for target in sorted(contract.TARGETS):
        name = contract.archive_name(version, target)
        src = next(args.artifacts.rglob(name))
        info = check_archive(src, version, target, licenses)
        commits.add(info['source_commit'])
        shutil.copyfile(src, args.out / name)
        report[name] = {'executable_sha256': info['executable_sha256'], 'rustc': info['rustc'][0]}
    if args.source_commit and commits != {args.source_commit}:
        raise SystemExit(f'archives built from {sorted(commits)}, expected {args.source_commit}')
    if len(commits) != 1:
        raise SystemExit(f'archives come from different commits: {sorted(commits)}')
    for installer in contract.INSTALLERS:
        shutil.copyfile(ROOT / installer, args.out / installer)
    lines = [f'{sha256(args.out / n)}  {n}\n' for n in contract.release_assets(version)[:-1]]
    (args.out / contract.CHECKSUMS).write_text(''.join(lines))
    print(json.dumps({'version': version, 'source_commit': commits.pop(), 'archives': report,
                      'checksums': ''.join(lines).splitlines()}, indent=2))


def verify(args: argparse.Namespace) -> None:
    version = contract.check_version(args.version)
    licenses = set(contract.license_files(ROOT))
    present = sorted(p.name for p in args.out.iterdir())
    if present != sorted(contract.release_assets(version)):
        raise SystemExit(f'release directory holds {present}')
    sums = {}
    for line in (args.out / contract.CHECKSUMS).read_text().splitlines():
        digest, name = line.split('  ', 1)
        sums[name] = digest
    if sorted(sums) != contract.release_assets(version)[:-1]:
        raise SystemExit(f'{contract.CHECKSUMS} lists {sorted(sums)}')
    for name, digest in sums.items():
        if sha256(args.out / name) != digest:
            raise SystemExit(f'checksum mismatch: {name}')
    for target in contract.TARGETS:
        check_archive(args.out / contract.archive_name(version, target), version, target, licenses)
    print(f'Verified {len(sums)} assets and {contract.CHECKSUMS} for {version}.')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--artifacts', type=Path, help='directory holding the downloaded per-target archives')
    parser.add_argument('--source-commit')
    parser.add_argument('--out', required=True, type=Path, help='release directory to create (or verify)')
    parser.add_argument('--verify', action='store_true')
    args = parser.parse_args()
    if args.verify:
        verify(args)
    elif args.artifacts:
        assemble(args)
    else:
        parser.error('--artifacts is required unless --verify is given')


if __name__ == '__main__':
    main()
