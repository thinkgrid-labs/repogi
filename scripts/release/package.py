#!/usr/bin/env python3
"""Package one built `repogi` executable into its release archive.

The archive holds exactly the executable, LICENSE, THIRD_PARTY_NOTICES.md, the
public licenses/ tree, README.txt and BUILD-INFO.json under one top-level
directory. Members are sorted with fixed timestamps, owners and modes, so
packaging the same inputs twice gives the same bytes. Never call this on an
executable that has not passed inspect_binary.py and the smoke test.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import time
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract  # noqa: E402
from inspect_binary import inspect  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]

README = """Repogi {version} ({target})

Local-first code intelligence for TypeScript/TSX repositories: a CLI and a
read-only MCP server. Source and documentation:
https://github.com/{repository}

Install: copy `{exe}` to a directory on your PATH, or use the release
installers (install.sh / install.ps1) which verify SHA256SUMS first.

    {exe} --version
    {exe} index /path/to/project

Repogi is MIT licensed (LICENSE). Bundled dependencies keep their own
licenses: see THIRD_PARTY_NOTICES.md and licenses/. BUILD-INFO.json records
the source commit, compiler and target this executable was built from.
"""


def command(*args: str) -> str:
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout.strip()


def build_info(args: argparse.Namespace, binary: bytes, report: dict) -> dict:
    env = os.environ
    run = (f'{env["GITHUB_SERVER_URL"]}/{env["GITHUB_REPOSITORY"]}/actions/runs/{env["GITHUB_RUN_ID"]}'
           if env.get('GITHUB_RUN_ID') else None)
    info = {
        'name': 'repogi',
        'version': args.version,
        'target': args.target,
        'source_repository': f'https://github.com/{contract.REPOSITORY}',
        'source_commit': args.source_commit,
        'source_date_epoch': args.source_date_epoch,
        'rustc': command('rustc', '-vV').splitlines(),
        'cargo': command('cargo', '-V'),
        'cargo_profile': 'release',
        'rustflags': env.get('RUSTFLAGS', ''),
        'executable': contract.executable(args.target),
        'executable_sha256': hashlib.sha256(binary).hexdigest(),
        'build_environment': args.build_environment or f'{platform.system()} {platform.machine()}',
        'workflow_run': run,
    }
    for key in ('glibc_max', 'glibc_floor', 'macos_min', 'needed'):
        if report.get(key) is not None:
            info[key] = report[key]
    return info


def members(args: argparse.Namespace, binary: bytes, info: dict) -> list[tuple[str, bytes, int]]:
    exe = contract.executable(args.target)
    readme = README.format(version=args.version, target=args.target, exe=exe, repository=contract.REPOSITORY)
    files = [(exe, binary, 0o755),
             ('BUILD-INFO.json', (json.dumps(info, indent=2) + '\n').encode(), 0o644),
             ('README.txt', readme.encode(), 0o644)]
    for name in (*contract.ROOT_MEMBERS, *contract.license_files(ROOT)):
        path = ROOT / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f'payload source is not a regular file: {name}')
        files.append((name, path.read_bytes(), 0o644))
    return sorted(files)


def directories(stem: str, names: list[str]) -> list[str]:
    dirs = {stem}
    for name in names:
        parts = name.split('/')[:-1]
        dirs.update(stem + '/' + '/'.join(parts[:i]) for i in range(1, len(parts) + 1))
    return sorted(dirs)


def write_tar(path: Path, stem: str, files: list, epoch: int) -> None:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode='w', format=tarfile.PAX_FORMAT) as tar:
        def add(name: str, mode: int, data: bytes | None) -> None:
            info = tarfile.TarInfo(name)
            info.mtime, info.mode, info.uid, info.gid, info.uname, info.gname = epoch, mode, 0, 0, '', ''
            if data is None:
                info.type = tarfile.DIRTYPE
                tar.addfile(info)
            else:
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        for d in directories(stem, [f[0] for f in files]):
            add(d, 0o755, None)
        for name, data, mode in files:
            add(f'{stem}/{name}', mode, data)
    with open(path, 'wb') as out, gzip.GzipFile(filename='', mode='wb', fileobj=out, mtime=0, compresslevel=9) as gz:
        gz.write(raw.getvalue())


def write_zip(path: Path, stem: str, files: list, epoch: int) -> None:
    stamp = time.gmtime(max(epoch, 315532800))[:6]
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
        for d in directories(stem, [f[0] for f in files]):
            info = zipfile.ZipInfo(d + '/', stamp)
            info.external_attr = (0o40755 << 16) | 0x10
            zf.writestr(info, b'')
        for name, data, mode in files:
            info = zipfile.ZipInfo(f'{stem}/{name}', stamp)
            info.external_attr = (0o100000 | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(info, data)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True, choices=sorted(contract.TARGETS))
    parser.add_argument('--version', required=True)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--source-commit', required=True)
    parser.add_argument('--source-date-epoch', required=True, type=int)
    parser.add_argument('--out-dir', required=True, type=Path)
    parser.add_argument('--build-environment', help='runner image or container that built the executable')
    args = parser.parse_args()
    contract.check_version(args.version)
    if len(args.source_commit) != 40 or any(c not in '0123456789abcdef' for c in args.source_commit):
        raise SystemExit('--source-commit must be a full lowercase commit SHA')
    report = inspect(args.binary, args.target)
    if report['problems']:
        raise SystemExit('binary check failed: ' + '; '.join(report['problems']))
    binary = args.binary.read_bytes()
    info = build_info(args, binary, report)
    files = members(args, binary, info)
    stem = contract.stem(args.version, args.target)
    licenses = set(contract.license_files(ROOT))
    for name in directories(stem, [f[0] for f in files]):
        contract.check_member(name + '/', args.version, args.target, licenses)
    for name, _, _ in files:
        contract.check_member(f'{stem}/{name}', args.version, args.target, licenses)
    args.out_dir.mkdir(parents=True, exist_ok=True)
    out = args.out_dir / contract.archive_name(args.version, args.target)
    if out.exists():
        raise SystemExit(f'refusing to overwrite {out}')
    writer = write_zip if contract.TARGETS[args.target]['format'] == 'zip' else write_tar
    writer(out, stem, files, args.source_date_epoch)
    (args.out_dir / f'{stem}.BUILD-INFO.json').write_text(json.dumps(info, indent=2) + '\n')
    print(json.dumps({'archive': out.name, 'sha256': hashlib.sha256(out.read_bytes()).hexdigest(),
                      'executable_sha256': info['executable_sha256'], 'members': len(files)}, indent=2))


if __name__ == '__main__':
    main()
