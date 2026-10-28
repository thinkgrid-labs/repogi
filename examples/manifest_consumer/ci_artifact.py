#!/usr/bin/env python3
"""Local CI export: trusted Atlas, explicit local commits, verified atomic bundle.

This script executes only Git plumbing, the selected Atlas executable and the
independent saved-file consumer. It never runs commands found in the repository.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from consumer import Invalid, require, verify


def generate(binary, checksum, root, base, head, output, max_bytes=8000000):
    binary, root, output = Path(binary).resolve(), Path(root).resolve(), Path(output).absolute()
    require(not output.resolve().is_relative_to(root), 'output', 'output must be outside analysed root')
    require(not binary.is_relative_to(root), 'executable', 'select trusted executable outside analysed root')
    require(not output.exists() and not output.is_symlink(), 'output', 'output already exists')
    require(hashlib.sha256(binary.read_bytes()).hexdigest() == checksum, 'executable', 'trusted checksum mismatch')

    def git(*args):
        return subprocess.run(['git', '--no-optional-locks', '-c', 'core.fsmonitor=false', '-C', str(root), *args],
                              capture_output=True, check=True, text=True).stdout.strip()

    commits = {}
    for name, revision in (('base', base), ('head', head)):
        commits[name] = git('rev-parse', '--verify', '--end-of-options', revision+'^{commit}')
        git('cat-file', '-e', commits[name]+'^{commit}')
    output.parent.mkdir(parents=True, exist_ok=True)
    pending = Path(tempfile.mkdtemp(prefix='.atlas-bundle-pending-', dir=output.parent))
    try:
        path = pending/'atlas-verification-manifest.json'
        with path.open('wb') as stream:
            subprocess.run([str(binary), 'impact', str(root), '--base', commits['base'], '--head', commits['head'],
                '--manifest', '--format', 'json', '--depth', '2', '--max-results', '1000', '--max-declarations', '100',
                '--max-bytes', str(max_bytes)], stdout=stream, stderr=subprocess.PIPE, check=True)
            stream.flush()
            os.fsync(stream.fileno())
        cli = subprocess.run([str(binary), 'manifest-verify', str(path)], capture_output=True, check=True)
        with path.open('rb') as stream:
            data = stream.read(8000001)
        value, record = verify(data)
        cli_record = json.loads(cli.stdout)
        require(all(cli_record[k] == record[k] for k in record), 'identity', 'CLI/independent validation mismatch')
        # A separate process needs only the saved file and independent hash provider.
        separate = subprocess.run([sys.executable, str(Path(__file__).with_name('consumer.py')), 'verify', str(path),
                                   '--extract'], capture_output=True, check=True)
        separate_record = json.loads(separate.stdout)
        require(all(separate_record[k] == record[k] for k in record), 'identity', 'separate consumer mismatch')
        require({s: value['snapshots'][s]['commit'] for s in ('base', 'head')} == commits,
                'snapshot', 'resolved commit provenance mismatch')
        record.update(binary_sha256=checksum, commits=commits)
        (pending/'atlas-verification-integrity.json').write_text(json.dumps(record, sort_keys=True)+'\n')
        os.rename(pending, output)
        return record
    finally:
        if pending.exists():
            shutil.rmtree(pending)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--sha256', required=True)
    p.add_argument('--root', type=Path, required=True)
    p.add_argument('--base', required=True)
    p.add_argument('--head', required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--max-bytes', type=int, default=8000000)
    a = p.parse_args()
    try:
        print(json.dumps(generate(a.binary, a.sha256, a.root, a.base, a.head, a.output, a.max_bytes), sort_keys=True))
    except (Invalid, OSError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
