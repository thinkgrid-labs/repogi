#!/usr/bin/env python3
"""Offline durability evaluation for Repogi project memory (v0.5.1).

Builds the labeled project-memory fixture's memories (approved, pending,
rejected, corrected, superseded) with an optional *legacy* binary (v0.5,
memories in .code-intel/index.db), migrates them with the candidate binary and
checks, with independent SQLite reads:

  - lossless migration (every row and column, event sequences, FTS rows);
  - idempotent re-migration;
  - memory surviving deletion and rebuild of .code-intel, with identical
    verification states before and after;
  - export -> restore -> export round-trip digest equality, repeated-import
    idempotence and cross-repository review import (nothing authoritative);
  - read-only behavior (byte digests of both stores);
  - crash recovery at each fault point (debug binary only: fault injection is
    compiled out of release builds);
  - timings and sizes at fixture and synthetic scale.

No project code is executed. Fixture metrics describe these labels only.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from project_memory import FIXTURE, META, selectors  # noqa: E402

ENV_DROP = ('REPOGI_MEMORY_REVIEWER', 'REPOGI_MEMORY_AUTHOR', 'REPOGI_TEST_FAULT')
TABLES = [
    "SELECT id,type,status,latest_revision,approved_revision,pending_revision,superseded_by,created_at,updated_at,version FROM memories ORDER BY id",
    "SELECT memory_id,revision,state,digest,claim_key,assertion_subject,assertion_status,data FROM memory_revisions ORDER BY memory_id,revision",
    "SELECT memory_id,revision,ordinal,role,class,subject_id,explicit_id,target_id,path,data FROM memory_evidence ORDER BY memory_id,revision,ordinal",
    "SELECT sequence,memory_id,revision,action,data FROM memory_events ORDER BY sequence",
    "SELECT memory_id,revision,title,claim,subjects FROM memory_search ORDER BY memory_id,revision",
]
FAULTS = ['migrate_before_commit', 'migrate_before_commit:abort', 'migrate_before_install',
          'migrate_before_install:abort', 'migrate_after_install:abort']


class Runner:
    def __init__(self, binary, root):
        self.binary, self.root = binary, root
        self.env = {k: v for k, v in os.environ.items() if k not in ENV_DROP}

    def run(self, *args, env=None):
        started = time.perf_counter()
        p = subprocess.run([str(self.binary), *args], cwd=self.root, capture_output=True, timeout=600,
                           env={**self.env, **(env or {})})
        return p, (time.perf_counter() - started) * 1000

    def json(self, *args):
        p, _ = self.run(*args, '--format', 'json')
        if p.returncode != 0:
            raise SystemExit(f'{args}: {p.stderr.decode()}')
        return json.loads(p.stdout)

    def code(self, *args):
        p, _ = self.run(*args)
        return None if p.returncode == 0 else json.loads(p.stderr)['error']['code']

    def timed(self, runs, *args):
        times = []
        for _ in range(runs):
            p, ms = self.run(*args)
            assert p.returncode == 0, p.stderr
            times.append(ms)
        return round(statistics.median(times), 2)


def rows(path, portable=False):
    """Every memory row; `portable` drops store-local event sequences (imports
    renumber them) while keeping event order."""
    con = sqlite3.connect(f'file:{path}?mode=ro', uri=True)
    try:
        out = []
        for sql in TABLES:
            if portable and sql.startswith('SELECT sequence'):
                sql = 'SELECT memory_id,revision,action,data FROM memory_events ORDER BY memory_id,sequence'
            out += [tuple(r) for r in con.execute(sql)]
        return out
    finally:
        con.close()


def state_digest(root):
    h = hashlib.sha256()
    for d in ['.code-intel', '.repogi']:
        base = root / d
        for p in sorted(base.glob('*')) if base.exists() else []:
            if p.is_file():
                h.update(p.name.encode() + b'\0' + p.read_bytes())
    return h.hexdigest()


def fixture(tmp, name):
    root = Path(tmp) / name
    shutil.copytree(FIXTURE, root, ignore=META)
    return root


def populate(r, root, cases):
    """The labeled lifecycle: proposals, reviews, a correction, supersession."""
    ids = {}
    for memory in cases['memories']:
        argv = ['memory', 'propose', '--type', memory['type'], '--title', memory['title'],
                '--claim', memory['claim'], '--author', 'eval']
        argv += selectors(root, memory['evidence'])
        argv += selectors(root, memory.get('context', []), '--context', '--context-source')
        if 'asserts' in memory:
            argv += ['--asserts', memory['asserts']]
        receipt = r.json(*argv)
        ids[memory['key']] = receipt['id']
        digest = receipt['digest'][:16]
        if memory['outcome'] == 'approve':
            r.json('memory', 'approve', receipt['id'], '--digest', digest, '--reviewer', 'reviewer')
        elif memory['outcome'] == 'reject':
            r.json('memory', 'reject', receipt['id'], '--digest', digest, '--reviewer', 'reviewer', '--reason', 'labelled reject')
    for link in cases['supersede']:
        r.json('memory', 'supersede', ids[link['old']], '--by', ids[link['new']], '--reason', link['reason'], '--reviewer', 'reviewer')
    # One correction of an approved, still-authoritative memory.
    superseded = {s['old'] for s in cases['supersede']}
    key = next(m['key'] for m in cases['memories'] if m['outcome'] == 'approve' and m['key'] not in superseded)
    original = next(m for m in cases['memories'] if m['key'] == key)
    receipt = r.json('memory', 'correct', ids[key], '--claim', original['claim'].rstrip('.') + ', as reviewed.',
                     '--reason', 'evaluation correction', *selectors(root, original['evidence']))
    r.json('memory', 'approve', ids[key], '--digest', receipt['digest'][:16], '--reviewer', 'reviewer')
    return ids


def verification_states(r):
    return {m['id']: m['verification'] for m in r.json('memory', 'verify', '--all', '--limit', '100')['results']}


def legacy_from_durable(root):
    """Move durable rows into the schema-5 index tables (v0.5 layout)."""
    con = sqlite3.connect(root / '.code-intel' / 'index.db')
    for (name,) in con.execute("SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'memory_moved_%'").fetchall():
        con.execute(f'DROP TRIGGER {name}')
    con.execute('ATTACH ? AS m', (str(root / '.repogi' / 'memory.db'),))
    con.executescript("""
        INSERT INTO main.memories SELECT * FROM m.memories;
        INSERT INTO main.memory_revisions SELECT * FROM m.memory_revisions;
        INSERT INTO main.memory_evidence SELECT * FROM m.memory_evidence;
        INSERT INTO main.memory_events SELECT * FROM m.memory_events;
        INSERT INTO main.memory_search(memory_id,revision,title,claim,subjects) SELECT memory_id,revision,title,claim,subjects FROM m.memory_search;
    """)
    con.commit()
    con.execute('DETACH m')
    con.close()
    shutil.rmtree(root / '.repogi')


def size(path):
    return path.stat().st_size if path.exists() else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('binary', type=Path, help='candidate (v0.5.1) binary')
    parser.add_argument('--legacy', type=Path, help='v0.5 binary that stores memory in the index')
    parser.add_argument('--debug-binary', type=Path, help='debug build of the candidate, for fault injection')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--runs', type=int, default=7)
    parser.add_argument('--scale', type=int, default=1000)
    args = parser.parse_args()
    binary = args.binary.resolve()
    cases = json.loads((FIXTURE / 'cases.json').read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    failures = []
    report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}
    if args.legacy:
        report['legacy_binary_sha256'] = hashlib.sha256(args.legacy.resolve().read_bytes()).hexdigest()

    def check(condition, message):
        if not condition:
            failures.append(message)

    with tempfile.TemporaryDirectory(prefix='repogi-durability-eval-') as tmp:
        # 1. Legacy memories, written by v0.5 itself when available.
        root = fixture(tmp, 'repo')
        writer = Runner(args.legacy.resolve() if args.legacy else binary, root)
        assert writer.run('index', '.')[0].returncode == 0
        ids = populate(writer, root, cases)
        if not args.legacy:
            legacy_from_durable(root)
        report['legacy_source'] = 'v0.5 binary' if args.legacy else 'durable rows moved into schema-5 tables'
        index_db = root / '.code-intel' / 'index.db'
        before = rows(index_db)
        report['legacy_rows'] = len(before)
        r = Runner(binary, root)

        # 2. Reads before migration are read-only and say memory is unmigrated.
        digest = state_digest(root)
        listing = r.json('memory', 'list', '--limit', '100')
        check(listing.get('storage', [{}])[0].get('code') == 'memory_not_migrated', 'unmigrated read not labelled')
        check(state_digest(root) == digest and not (root / '.repogi').exists(), 'a read before migration wrote')

        # 3. Migration: lossless, validated, idempotent.
        p, migrate_ms = r.run('memory', 'migrate', '--format', 'json')
        check(p.returncode == 0, f'migrate: {p.stderr.decode()}')
        migration = json.loads(p.stdout)
        durable = root / '.repogi' / 'memory.db'
        lossless = rows(durable) == before
        check(lossless, 'migration is not row-for-row identical')
        check(rows(index_db) == before, 'migration changed the source rows')
        again = r.json('memory', 'migrate')
        check(again['outcome'] == 'already_migrated', 're-migration not idempotent')
        check(rows(durable) == before, 're-migration changed rows')
        integrity = r.json('memory', 'status', '--check')['integrity']
        check(integrity == [], f'integrity: {integrity}')
        report['migration'] = {
            'outcome': migration['outcome'], 'counts': migration['migration']['counts'],
            'lossless_rows': lossless, 'legacy_frozen': migration['migration']['legacy_frozen'],
            'idempotent': again['outcome'] == 'already_migrated', 'integrity_problems': integrity,
            'migrate_ms': round(migrate_ms, 2),
        }

        # 4. Index deletion and rebuild.
        states = verification_states(r)
        histories = {i: r.json('memory', 'history', i)['events'] for i in ids.values()}
        shutil.rmtree(root / '.code-intel')
        without = r.json('memory', 'list', '--limit', '100')
        unverified = verification_states(r)
        check(len(without['memories']) == len(ids), 'memories missing without an index')
        check(set(unverified.values()) <= {'unverified'}, f'unverified expected without index: {unverified}')
        p, rebuild_ms = r.run('index', '.')
        check(p.returncode == 0, 'rebuild failed')
        check(rows(durable) == before, 'rebuild changed memory rows')
        rebuilt = verification_states(r)
        check(rebuilt == states, f'verification differs after rebuild: {states} vs {rebuilt}')
        check(all(r.json('memory', 'history', i)['events'] == h for i, h in histories.items()), 'history changed by rebuild')
        report['rebuild'] = {
            'memories_without_index': len(without['memories']),
            'states_without_index': sorted(set(unverified.values())),
            'verification_identical_after_rebuild': rebuilt == states,
            'history_identical': True, 'rebuild_ms': round(rebuild_ms, 2),
        }

        # 5. Export -> restore -> export, repeated import, cross-repository review.
        backup = Path(tmp) / 'backup.json'
        digest = state_digest(root)
        p, export_ms = r.run('memory', 'export', '--output', str(backup), '--format', 'json')
        check(p.returncode == 0, 'export failed')
        check(state_digest(root) == digest, 'export wrote to the stores')
        exported = json.loads(backup.read_text())
        clone = fixture(tmp, 'clone')
        c = Runner(binary, clone)
        c.run('index', '.')
        p, import_ms = c.run('memory', 'import', str(backup), '--restore', '--format', 'json')
        check(p.returncode == 0, f'restore: {p.stderr.decode()}')
        restored = json.loads(p.stdout)
        check(rows(clone / '.repogi' / 'memory.db', True) == rows(durable, True), 'restored rows differ')
        second = Path(tmp) / 'second.json'
        c.run('memory', 'export', '--output', str(second))
        round_trip = json.loads(second.read_text())['integrity']['digest'] == exported['integrity']['digest']
        check(round_trip, 'round-trip digest differs')
        repeat = c.json('memory', 'import', str(backup))
        check(repeat['outcomes'] == {'unchanged': len(ids)}, f'repeated import: {repeat["outcomes"]}')
        check(verification_states(c) == states, 'restored evidence states differ')
        foreign = fixture(tmp, 'foreign')
        f = Runner(binary, foreign)
        f.run('index', '.')
        review = f.json('memory', 'import', str(backup))
        authoritative = [m for m in f.json('memory', 'list', '--limit', '100')['memories'] if m['authoritative']]
        check(review['mode'] == 'review' and not authoritative, 'cross-repository import became authoritative')
        tampered = json.loads(backup.read_text())
        tampered['memories'][0]['revisions'][0]['claim'] += ' (edited)'
        bad = Path(tmp) / 'tampered.json'
        bad.write_text(json.dumps(tampered))
        check(f.code('memory', 'import', str(bad)) == 'integrity_mismatch', 'tampered export accepted')
        report['backup'] = {
            'export_bytes': size(backup), 'export_ms': round(export_ms, 2), 'restore_ms': round(import_ms, 2),
            'restored': restored['outcomes'], 'round_trip_digest_equal': round_trip,
            'repeated_import': repeat['outcomes'], 'cross_repository': review['outcomes'],
            'cross_repository_authoritative': len(authoritative),
        }

        # 6. Read-only guarantee across reads, export and dry-run import.
        digest = state_digest(root)
        any_id = next(iter(ids.values()))
        for argv in [('memory', 'list'), ('memory', 'show', any_id), ('memory', 'history', any_id),
                     ('memory', 'search', 'session'), ('memory', 'verify', '--all'), ('memory', 'conflicts'),
                     ('memory', 'status', '--check'), ('memory', 'import', str(backup), '--dry-run'),
                     ('investigate', 'Modify session expiration', '--include-memory')]:
            r.run(*argv)
        report['read_only'] = state_digest(root) == digest
        check(report['read_only'], 'a read changed a store')

        # 7. Crash recovery (fault injection exists in debug builds only).
        recovery = []
        if args.debug_binary:
            for fault in FAULTS:
                work = fixture(tmp, f'fault-{fault.replace(":", "-")}')
                w = Runner(binary, work)
                w.run('index', '.')
                populate(w, work, cases)
                legacy_from_durable(work)
                source = rows(work / '.code-intel' / 'index.db')
                d = Runner(args.debug_binary.resolve(), work)
                p, _ = d.run('memory', 'migrate', env={'REPOGI_TEST_FAULT': fault})
                installed = (work / '.repogi' / 'memory.db').exists()
                intact = rows(work / '.code-intel' / 'index.db') == source
                retry = Runner(binary, work).json('memory', 'migrate')
                recovered = rows(work / '.repogi' / 'memory.db') == source
                recovery.append({'fault': fault, 'failed': p.returncode != 0, 'store_installed_before_retry': installed,
                                 'source_intact': intact, 'retry': retry['outcome'], 'recovered_lossless': recovered})
                check(p.returncode != 0 and intact and recovered, f'recovery {fault}')
        report['recovery'] = recovery or 'skipped: pass --debug-binary'

        # 8. Fixture latency (durable store, with index).
        report['latency_ms_fixture'] = {
            'memory_list': r.timed(args.runs, 'memory', 'list'),
            'memory_search': r.timed(args.runs, 'memory', 'search', 'session'),
            'memory_show': r.timed(args.runs, 'memory', 'show', any_id),
            'memory_verify_all': r.timed(args.runs, 'memory', 'verify', '--all'),
            'investigate_include_memory': r.timed(args.runs, 'investigate', 'Modify session expiration', '--include-memory'),
        }
        report['store_bytes_fixture'] = size(durable)

        # 9. Synthetic scale: migration, export, restore, queries.
        if args.scale:
            scale = fixture(tmp, 'scale')
            s = Runner(binary, scale)
            s.run('index', '.')
            topics = ['session', 'token', 'login', 'storage', 'audit', 'middleware', 'routes', 'cleanup']
            started = time.perf_counter()
            for n in range(args.scale):
                receipt = s.json('memory', 'propose', '--type', 'project_convention', '--title', f'Convention {n}',
                                 '--claim', f'Convention {n} about {topics[n % len(topics)]} handling number {n}.', '--evidence', 'ADR-0003')
                if n % 10:
                    s.json('memory', 'approve', receipt['id'], '--digest', receipt['digest'][:16], '--reviewer', 'scale')
            writes = time.perf_counter() - started
            store_bytes = size(scale / '.repogi' / 'memory.db')
            legacy_from_durable(scale)
            source = rows(scale / '.code-intel' / 'index.db')
            p, migrate_ms = s.run('memory', 'migrate')
            check(p.returncode == 0 and rows(scale / '.repogi' / 'memory.db') == source, 'scale migration not lossless')
            out = Path(tmp) / 'scale.json'
            p, export_ms = s.run('memory', 'export', '--output', str(out))
            target = fixture(tmp, 'scale-restore')
            t = Runner(binary, target)
            t.run('index', '.')
            p, restore_ms = t.run('memory', 'import', str(out), '--restore', '--format', 'json')
            check(p.returncode == 0 and rows(target / '.repogi' / 'memory.db', True) == rows(scale / '.repogi' / 'memory.db', True), 'scale restore differs')
            report['scale'] = {
                'memories': args.scale, 'write_seconds_total': round(writes, 2),
                'store_bytes': store_bytes, 'store_bytes_per_memory': round(store_bytes / args.scale),
                'migrate_ms': round(migrate_ms, 2), 'export_ms': round(export_ms, 2),
                'export_bytes': size(out), 'restore_ms': round(restore_ms, 2),
                'latency_ms': {
                    'memory_search': s.timed(args.runs, 'memory', 'search', 'session handling'),
                    'memory_list_100': s.timed(args.runs, 'memory', 'list', '--limit', '100'),
                    'memory_verify_all_100': s.timed(args.runs, 'memory', 'verify', '--all', '--limit', '100'),
                    'investigate_include_memory': s.timed(args.runs, 'investigate', 'Modify session expiration', '--include-memory'),
                },
            }

    report['failures'] = failures
    (args.output / 'evaluation.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({k: report[k] for k in ['migration', 'rebuild', 'backup', 'read_only']}, indent=1))
    if isinstance(report['recovery'], list):
        print('recovery', sum(x['recovered_lossless'] for x in report['recovery']), '/', len(report['recovery']))
    if failures:
        print('FAILURES:')
        for failure in failures:
            print(' -', failure)
        sys.exit(1)
    print('pass')


if __name__ == '__main__':
    main()
