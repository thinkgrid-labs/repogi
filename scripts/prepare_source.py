#!/usr/bin/env python3
"""Validate public source membership and optionally copy it to a fresh directory.

This prepares a local source tree; it does not approve, archive or publish a release.
"""
from __future__ import annotations

import argparse
from pathlib import Path, PurePosixPath
import shutil

ROOT = Path(__file__).resolve().parents[1]
ROOT_FILES = {
    '.gitignore', '.gitattributes', 'Cargo.toml', 'Cargo.lock', 'LICENSE',
    'README.md', 'THIRD_PARTY_NOTICES.md', 'licenses/THIRD_PARTY_NOTICES.json',
    'install.sh', 'install.ps1',
}
PUBLIC_ROOTS = {'assets', 'crates', 'docs', 'examples', 'fixtures', 'licenses', 'dist', 'scripts'}
PUBLIC_CONFIG_FILES = {'.github/workflows/ci.yml', '.github/workflows/release.yml'}
EXCLUDED = {
    'LICENSE-APACHE', 'AGENTS.md', 'docs/PRD.md',
    'docs/DEVELOPER_PREVIEW_RELEASE_CHECKLIST.md',
    'examples/ci/deployment_preflight.py', 'examples/ci/hosted-pilot-plan.json',
    'examples/ci/reviewed-action-pins.json',
    'examples/ci/verification-manifest-hosted.yml.example',
}
PRIVATE_PARTS = {
    '.git', 'target', 'benchmarks', '.code-intel', '__pycache__',
    'internal', 'sessions', 'recordings', 'evaluations',
}


def members() -> list[Path]:
    lines = (ROOT / 'dist/source-files.txt').read_text().splitlines()
    if not lines or lines != sorted(set(lines)):
        raise ValueError('source manifest must contain sorted unique paths')
    missing = ROOT_FILES.difference(lines)
    if missing:
        raise ValueError(f'missing required public assets: {sorted(missing)}')
    result = []
    for name in lines:
        rel = PurePosixPath(name)
        if rel.is_absolute() or '..' in rel.parts or str(rel) != name:
            raise ValueError(f'invalid relative path: {name}')
        if (name in EXCLUDED or PRIVATE_PARTS.intersection(rel.parts)
                or rel.name.startswith(('PHASE3', 'PHASE4', 'PHASE5'))
                or 'BENCHMARK' in rel.name or 'EVALUATION' in rel.name):
            raise ValueError(f'internal path in manifest: {name}')
        if name not in ROOT_FILES | PUBLIC_CONFIG_FILES and rel.parts[0] not in PUBLIC_ROOTS:
            raise ValueError(f'unreviewed source root: {name}')
        src = ROOT / name
        if any(parent.is_symlink() for parent in [src, *src.parents]):
            raise ValueError(f'symlink source is not supported: {name}')
        if not src.is_file():
            raise ValueError(f'missing source file: {name}')
        result.append(src)
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, help='fresh local copy directory')
    args = parser.parse_args()
    files = members()
    if args.output:
        output = args.output.resolve()
        if output == ROOT or ROOT in output.parents or output in ROOT.parents:
            raise ValueError('output must be outside the source tree')
        output.mkdir(parents=True, exist_ok=False)
        for src in files:
            dst = output / src.relative_to(ROOT)
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
    print(f'Validated {len(files)} public source files. License review is a separate release requirement.')


if __name__ == '__main__':
    main()
