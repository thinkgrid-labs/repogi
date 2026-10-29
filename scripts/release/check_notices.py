#!/usr/bin/env python3
"""Refuse a release whose shipped dependencies lack their license notices.

For one target triple, walks the normal (linked) dependency closure of the
`repogi` CLI from `cargo metadata --filter-platform`, then requires each
package's own grant in licenses/THIRD_PARTY_NOTICES.json: at least one license
file at the top of its notice directory. A notice that only covers an embedded
component (such as a Unicode table) does not count as the package's grant.
Also checks the SQLite notice and that the Rust standard library notice matches
the active toolchain. Prints a JSON report; exits non-zero on any gap.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from contract import TARGETS  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
CLI = 'code-intel-cli'
RUST_NOTICE = 'licenses/rust/COPYRIGHT-library.html'
NATIVE_NOTICES = {'libsqlite3-sys': 'licenses/native/SQLITE_HEADER.txt'}


def closure(target: str) -> list[dict]:
    meta = json.loads(subprocess.run(
        ['cargo', 'metadata', '--format-version', '1', '--locked', '--filter-platform', target],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    packages = {p['id']: p for p in meta['packages']}
    nodes = {n['id']: n for n in meta['resolve']['nodes']}
    members = set(meta['workspace_members'])
    start = next(i for i in members if packages[i]['name'] == CLI)
    seen, stack = set(), [start]
    while stack:
        node = nodes[stack.pop()]
        for dep in node['deps']:
            if any(k['kind'] is None for k in dep['dep_kinds']) and dep['pkg'] not in seen:
                seen.add(dep['pkg'])
                stack.append(dep['pkg'])
    return sorted((packages[i] for i in seen - members), key=lambda p: (p['name'], p['version']))


def rust_notice_status() -> dict:
    sysroot = subprocess.run(['rustc', '--print', 'sysroot'], check=True, capture_output=True, text=True).stdout.strip()
    toolchain = Path(sysroot) / 'share/doc/rust/COPYRIGHT-library.html'
    shipped = hashlib.sha256((ROOT / RUST_NOTICE).read_bytes()).hexdigest()
    found = hashlib.sha256(toolchain.read_bytes()).hexdigest() if toolchain.is_file() else None
    return {'toolchain_notice': str(toolchain), 'toolchain_sha256': found, 'shipped_sha256': shipped,
            'matches': found == shipped}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True, choices=sorted(TARGETS))
    parser.add_argument('--skip-toolchain-notice', action='store_true',
                        help='do not compare the Rust notice with the active toolchain (inventory review only)')
    args = parser.parse_args()
    inventory = json.loads((ROOT / 'licenses/THIRD_PARTY_NOTICES.json').read_text())
    entries = {(p['name'], p['version']): p for p in inventory['packages']}
    problems, rows = [], []
    for pkg in closure(args.target):
        key = (pkg['name'], pkg['version'])
        entry = entries.get(key)
        row = {'name': pkg['name'], 'version': pkg['version'], 'license': pkg.get('license')}
        if entry is None:
            problems.append(f'{pkg["name"]} {pkg["version"]}: not in the notice inventory')
        else:
            notice_dir = PurePosixPath(f'licenses/dependencies/{pkg["name"]}-{pkg["version"]}')
            files = entry.get('license_files') or []
            missing = [f for f in files if not (ROOT / f).is_file()]
            grants = [f for f in files if PurePosixPath(f).parent == notice_dir]
            row.update(license_files=files, package_grants=grants)
            if missing:
                problems.append(f'{pkg["name"]} {pkg["version"]}: listed notice files missing: {missing}')
            if not grants:
                detail = f' (only component notices: {files})' if files else ''
                problems.append(f'{pkg["name"]} {pkg["version"]}: no upstream license grant on file{detail}')
            if entry.get('declared_license') != pkg.get('license'):
                problems.append(f'{pkg["name"]} {pkg["version"]}: inventory says {entry.get("declared_license")!r}, '
                                f'Cargo says {pkg.get("license")!r}')
        native = NATIVE_NOTICES.get(pkg['name'])
        if native and not (ROOT / native).is_file():
            problems.append(f'{pkg["name"]}: native notice {native} missing')
        rows.append(row)
    rust = None
    if not args.skip_toolchain_notice:
        rust = rust_notice_status()
        if not rust['matches']:
            problems.append(f'{RUST_NOTICE} does not match the active toolchain notice {rust["toolchain_notice"]}')
    print(json.dumps({'target': args.target, 'packages': rows, 'rust_notice': rust, 'problems': problems}, indent=2))
    if problems:
        sys.exit(f'{len(problems)} license notice problem(s) for {args.target}:\n  ' + '\n  '.join(problems))


if __name__ == '__main__':
    main()
