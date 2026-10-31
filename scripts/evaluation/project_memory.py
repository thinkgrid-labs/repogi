#!/usr/bin/env python3
"""Offline, labeled project-memory evaluation. No project code is executed.

Copies fixtures/project-memory to a temporary directory, indexes it with the
given binary, then drives the memory CLI through the labeled lifecycle in
cases.json: proposals, explicit reviews, refusals, supersession, searches,
investigations, source mutations and conflicts. Incorrect authoritative claims,
broken history, a read that writes, or a wrong verification state fail the run.
Scores describe these fixture labels only, never agent task outcomes.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time

META = shutil.ignore_patterns('README.md', 'cases.json')
FIXTURE = Path(__file__).resolve().parents[2] / 'fixtures' / 'project-memory'
ENV_DROP = ('REPOGI_MEMORY_REVIEWER', 'REPOGI_MEMORY_AUTHOR')


class Runner:
    def __init__(self, binary, root):
        self.binary, self.root = binary, root
        import os
        self.env = {k: v for k, v in os.environ.items() if k not in ENV_DROP}

    def run(self, *args):
        started = time.perf_counter()
        p = subprocess.run([str(self.binary), *args], cwd=self.root, capture_output=True, timeout=60, env=self.env)
        return p, (time.perf_counter() - started) * 1000

    def json(self, *args):
        p, _ = self.run(*args, '--format', 'json')
        if p.returncode != 0:
            raise SystemExit(f'{args}: {p.stderr.decode()}')
        return json.loads(p.stdout)

    def code(self, *args):
        p, _ = self.run(*args)
        if p.returncode == 0:
            return None
        return json.loads(p.stderr)['error']['code']

    def timed(self, runs, *args):
        times = []
        for _ in range(runs):
            p, ms = self.run(*args)
            assert p.returncode == 0, p.stderr
            times.append(ms)
        return round(statistics.median(times), 2)


def db(root):
    return sqlite3.connect(root / '.code-intel' / 'index.db')


def selectors(root, specs, flag_id='--evidence', flag_source='--source'):
    args = []
    con = db(root)
    for spec in specs:
        if 'id' in spec:
            args += [flag_id, spec['id']]
        elif 'source' in spec:
            args += [flag_source, spec['source']]
        elif 'relationship' in spec:
            r = spec['relationship']
            row = con.execute("SELECT r.id FROM knowledge_relationships r JOIN knowledge_entities e ON e.id=r.source_id WHERE e.explicit_id=? AND json_extract(r.data,'$.kind')=? ORDER BY r.id LIMIT 1", (r['author'], r['kind'])).fetchone()
            args += [flag_id, row[0]]
        elif 'fact' in spec:
            args += [flag_id, fact(con, *spec['fact'])]
        elif 'edge' in spec:
            args += ['--edge', fact(con, *spec['edge']['source']), fact(con, *spec['edge']['target'])]
    return args


def fact(con, path, name):
    return con.execute("SELECT id FROM facts WHERE path=? AND json_extract(data,'$.name')=? AND json_extract(data,'$.kind') NOT IN ('file','import','export')", (path, name)).fetchone()[0]


def memory_db(root):
    """The durable memory store (v0.5.1+), else the schema-5 index (v0.5)."""
    durable = root / '.repogi' / 'memory.db'
    return sqlite3.connect(durable if durable.exists() else root / '.code-intel' / 'index.db')


def db_digest(root):
    """Every file of both stores, so a read that writes anything is detected."""
    h = hashlib.sha256()
    for d in ['.code-intel', '.repogi']:
        for p in sorted((root / d).glob('*')) if (root / d).exists() else []:
            if p.is_file():
                h.update(p.name.encode() + b'\0' + p.read_bytes())
    return h.hexdigest()


def db_bytes(root):
    total = 0
    for path in [root / '.code-intel' / 'index.db', root / '.repogi' / 'memory.db']:
        if path.exists():
            con = sqlite3.connect(path)
            total += con.execute('PRAGMA page_count').fetchone()[0] * con.execute('PRAGMA page_size').fetchone()[0]
    return total


def memory_rows(root):
    con = memory_db(root)
    return {t: con.execute(f'SELECT count(*) FROM {t}').fetchone()[0] for t in ['memories', 'memory_revisions', 'memory_evidence', 'memory_events']}


def apply_edit(root, edit):
    path = root / edit['path']
    if 'write' in edit:
        path.write_text(edit['write'])
    elif 'replace' in edit:
        old, new = edit['replace']
        text = path.read_text()
        assert old in text, (edit, 'replace target missing')
        path.write_text(text.replace(old, new, 1))
    elif 'prepend' in edit:
        path.write_text(edit['prepend'] + path.read_text())
    elif 'delete_between' in edit:
        start, end = edit['delete_between']
        text = path.read_text()
        path.write_text(text[:text.index(start)] + text[text.index(end):])


def citation_correct(root, evidence):
    """Independent check that the snapshot cites the bytes it names."""
    citation = evidence['citation']
    data = (root / citation['path']).read_bytes()
    if citation['end_byte'] > len(data):
        return False
    cited = data[citation['start_byte']:citation['end_byte']].decode('utf-8', 'replace')
    rel = evidence.get('relationship') or {}
    if evidence['kind'] == 'static_relationship':
        return rel.get('target_name', '') in cited
    if evidence['kind'] == 'knowledge_relationship':
        return rel.get('target_text', '').split('#')[0].split('/')[-1].split('.')[0] in cited or rel.get('target_text', '') in cited
    if evidence['kind'] == 'document':
        return citation['start_byte'] == 0 and citation['end_byte'] == len(data)
    return (evidence.get('explicit_id') or evidence['title']) in cited


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--runs', type=int, default=9)
    parser.add_argument('--scale', type=int, default=300, help='synthetic approved memories for latency/size')
    args = parser.parse_args()
    binary = args.binary.resolve()
    cases = json.loads((FIXTURE / 'cases.json').read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    failures, report = [], {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}

    def check(condition, message):
        if not condition:
            failures.append(message)

    with tempfile.TemporaryDirectory(prefix='repogi-memory-eval-') as tmp:
        root = Path(tmp) / 'repo'
        shutil.copytree(FIXTURE, root, ignore=META)
        r = Runner(binary, root)
        p, index_ms = r.run('index', '.')
        assert p.returncode == 0, p.stderr
        report['index_ms'] = round(index_ms, 2)
        report['db_bytes_before_memories'] = db_bytes(root)
        tree_before = sorted((str(p.relative_to(root)), p.read_bytes()) for p in root.rglob('*') if p.is_file() and '.code-intel' not in p.parts and '.repogi' not in p.parts)

        # Proposals and explicit reviews.
        ids, write_ms = {}, []
        for memory in cases['memories']:
            argv = ['memory', 'propose', '--type', memory['type'], '--title', memory['title'], '--claim', memory['claim'], '--author', 'eval']
            argv += selectors(root, memory['evidence'])
            argv += selectors(root, memory.get('context', []), '--context', '--context-source')
            if 'asserts' in memory:
                argv += ['--asserts', memory['asserts']]
            p, ms = r.run(*argv, '--format', 'json')
            write_ms.append(ms)
            check(p.returncode == 0, f"propose {memory['key']}: {p.stderr.decode()}")
            receipt = json.loads(p.stdout)
            ids[memory['key']] = receipt['id']
            check(receipt['status'] == 'pending_review', f"{memory['key']} not pending after propose")
            digest = receipt['digest'][:16]
            if memory['outcome'] == 'approve':
                p, ms = r.run('memory', 'approve', receipt['id'], '--digest', digest, '--reviewer', 'reviewer', '--format', 'json')
                write_ms.append(ms)
                check(p.returncode == 0, f"approve {memory['key']}: {p.stderr.decode()}")
            elif memory['outcome'] == 'reject':
                p, ms = r.run('memory', 'reject', receipt['id'], '--digest', digest, '--reviewer', 'reviewer', '--reason', 'labelled reject', '--format', 'json')
                write_ms.append(ms)
                check(p.returncode == 0, f"reject {memory['key']}")
        key_of = {v: k for k, v in ids.items()}
        refusals = []
        for refusal in cases['refused']:
            argv = ['memory', 'propose', '--type', refusal.get('type', 'requirement'), '--title', 'Refused', '--claim', refusal.get('claim', f"Refused case {refusal['name']}.")]
            argv += selectors(root, refusal['evidence'])
            code = r.code(*argv)
            refusals.append({'name': refusal['name'], 'expected': refusal['code'], 'actual': code})
            check(code == refusal['code'], f"refusal {refusal['name']}: {code}")
        report['refusals'] = refusals
        for link in cases['supersede']:
            p, _ = r.run('memory', 'supersede', ids[link['old']], '--by', ids[link['new']], '--reason', link['reason'], '--reviewer', 'reviewer')
            check(p.returncode == 0, f'supersede {link}')
        report['memory_write_ms_median'] = round(statistics.median(write_ms), 2)
        report['db_bytes_with_fixture_memories'] = db_bytes(root)
        report['memory_rows'] = memory_rows(root)

        authoritative = {m['key'] for m in cases['memories'] if m['outcome'] == 'approve'} - {s['old'] for s in cases['supersede']}

        # Citation correctness on every stored snapshot, before any mutation.
        citations = correct = 0
        for key, mid in ids.items():
            shown = r.json('memory', 'show', mid)
            for revision in [shown.get('approved'), shown.get('pending'), shown.get('reviewed')]:
                for evidence in (revision or {}).get('evidence', []):
                    citations += 1
                    correct += citation_correct(root, evidence)
        report['citations'] = {'checked': citations, 'correct': correct}
        check(citations > 0 and correct == citations, f'citation correctness {correct}/{citations}')

        # Read-only guarantee across every read path.
        digest_before = db_digest(root)
        rows_before = memory_rows(root)
        searches, incorrect_authoritative, excluded_violations = [], 0, 0
        for case in cases['searches']:
            response = r.json('memory', 'search', case['query'], '--limit', '20')
            exact = [key_of[h['id']] for h in response['results'] if h['match_mode'] == 'all_terms']
            returned = [key_of[h['id']] for h in response['results']]
            for hit in response['results']:
                if hit['authoritative'] and key_of[hit['id']] not in authoritative:
                    incorrect_authoritative += 1
            excluded_violations += sum(1 for k in case['excluded'] if k in returned)
            relevant = set(case['relevant'])
            tp = len(relevant & set(exact))
            searches.append({'query': case['query'], 'all_terms': exact, 'partial': [k for k in returned if k not in exact],
                             'precision': round(tp / len(exact), 3) if exact else (1.0 if not relevant else 0.0),
                             'recall': round(tp / len(relevant), 3) if relevant else 1.0})
        report['searches'] = searches
        report['search_precision_mean'] = round(statistics.mean(s['precision'] for s in searches), 3)
        report['search_recall_mean'] = round(statistics.mean(s['recall'] for s in searches), 3)
        report['incorrect_authoritative_results'] = incorrect_authoritative
        report['excluded_returned'] = excluded_violations
        check(incorrect_authoritative == 0, 'non-approved memory returned as authoritative')
        check(excluded_violations == 0, 'rejected/pending/superseded memory returned by default search')
        investigations = []
        for case in cases['investigations']:
            plain = r.json('investigate', case['task'])
            with_memory = r.json('investigate', case['task'], '--include-memory', '--max-steps', '16')
            surfaced = [key_of[t] for s in with_memory['steps'] if s['action'] == 'review_memory' for t in s['targets']]
            relevant = set(case['relevant'])
            tp = len(relevant & set(surfaced))
            strip = lambda plan: [(s['action'], s['targets']) for s in plan['steps'] if s['category'] != 'memory']
            plain16 = r.json('investigate', case['task'], '--max-steps', '16')
            investigations.append({
                'task': case['task'], 'surfaced': surfaced,
                'precision': round(tp / len(surfaced), 3) if surfaced else (1.0 if not relevant else 0.0),
                'recall': round(tp / len(relevant), 3) if relevant else 1.0,
                # Memory never adds or alters other steps; a step limit may drop some.
                'non_memory_steps_subset': all(step in strip(plain16) for step in strip(with_memory)),
                'default_has_memory': any(e['class'] == 'approved_project_memory' for e in plain['evidence']),
            })
            check(not investigations[-1]['default_has_memory'], f"default plan used memory: {case['task']}")
            check(investigations[-1]['non_memory_steps_subset'], f"memory altered other steps: {case['task']}")
            check(all(k in authoritative for k in surfaced), f"non-authoritative memory surfaced: {surfaced}")
        report['investigations'] = investigations
        for mid in ids.values():
            r.json('memory', 'show', mid)
            r.json('memory', 'history', mid)
        r.json('memory', 'list')
        r.json('memory', 'verify', '--all')
        r.json('memory', 'conflicts')
        check(db_digest(root) == digest_before and memory_rows(root) == rows_before, 'a read changed the database')
        report['read_only'] = db_digest(root) == digest_before

        # History preservation.
        histories = {}
        for key, expected in cases['history'].items():
            actions = [e['action'] for e in r.json('memory', 'history', ids[key])['events']]
            histories[key] = actions
            check(actions == expected, f'history {key}: {actions}')
        superseded = r.json('memory', 'show', ids['no_server'])['memory']
        check(superseded['superseded_by'] == ids['one_file'] and not superseded['authoritative'], 'supersession lineage')
        report['history'] = histories

        # Source mutations and staleness detection.
        staleness, labelled, right = [], 0, 0
        for mutation in cases['mutations']:
            for edit in mutation['edits']:
                apply_edit(root, edit)
            if mutation['reindex']:
                p, _ = r.run('index', '.')
                assert p.returncode == 0, p.stderr
            states = {}
            for key, expected in mutation['expect'].items():
                actual = r.json('memory', 'verify', ids[key])['results'][0]['verification']
                states[key] = actual
                labelled += 1
                right += actual == expected
                check(actual == expected, f"{mutation['name']}: {key} is {actual}, expected {expected}")
            staleness.append({'mutation': mutation['name'], 'states': states})
        report['staleness'] = {'labelled': labelled, 'correct': right, 'steps': staleness}
        for key in authoritative:
            check(r.json('memory', 'show', ids[key])['memory']['status'] == 'approved', f'{key} status changed by a source change')

        # Structured conflicts.
        found = {(c['kind'], tuple(key_of[m] for m in c['memories'])) for c in r.json('memory', 'conflicts')['conflicts']}
        expected = {(c['kind'], tuple(c['memories'])) for c in cases['conflicts']}
        report['conflicts'] = {'expected': len(expected), 'found': len(found), 'correct': len(found & expected),
                               'unexpected': sorted(map(list, found - expected)), 'missed': sorted(map(list, expected - found))}
        check(found == expected, f'conflicts differ: {report["conflicts"]}')

        # Latency at fixture size.
        report['latency_ms_fixture'] = {
            'memory_search': r.timed(args.runs, 'memory', 'search', 'session'),
            'memory_show': r.timed(args.runs, 'memory', 'show', ids['sqlite']),
            'memory_verify_all': r.timed(args.runs, 'memory', 'verify', '--all'),
            'memory_conflicts': r.timed(args.runs, 'memory', 'conflicts'),
            'investigate': r.timed(args.runs, 'investigate', 'Modify session expiration'),
            'investigate_include_memory': r.timed(args.runs, 'investigate', 'Modify session expiration', '--include-memory'),
        }
        tree_after = sorted((str(p.relative_to(root)), p.read_bytes()) for p in root.rglob('*') if p.is_file() and '.code-intel' not in p.parts and '.repogi' not in p.parts)
        report['only_labelled_edits_changed_the_tree'] = len(tree_after) == len(tree_before) + 1

        # Scale: many approved memories, bounded queries.
        if args.scale:
            scale_root = Path(tmp) / 'scale'
            shutil.copytree(FIXTURE, scale_root, ignore=META)
            s = Runner(binary, scale_root)
            s.run('index', '.')
            before = db_bytes(scale_root)
            topics = ['session', 'token', 'login', 'storage', 'audit', 'middleware', 'routes', 'cleanup']
            started = time.perf_counter()
            for n in range(args.scale):
                receipt = s.json('memory', 'propose', '--type', 'project_convention', '--title', f'Convention {n}',
                                 '--claim', f'Convention {n} about {topics[n % len(topics)]} handling number {n}.', '--evidence', 'ADR-0003')
                s.json('memory', 'approve', receipt['id'], '--digest', receipt['digest'][:16], '--reviewer', 'scale')
            report['scale'] = {
                'memories': args.scale,
                'write_seconds_total': round(time.perf_counter() - started, 2),
                'db_bytes_delta': db_bytes(scale_root) - before,
                'db_bytes_per_memory': round((db_bytes(scale_root) - before) / args.scale),
                'latency_ms': {
                    'memory_search': s.timed(args.runs, 'memory', 'search', 'session handling'),
                    'memory_list_100': s.timed(args.runs, 'memory', 'list', '--limit', '100'),
                    'memory_verify_all_100': s.timed(args.runs, 'memory', 'verify', '--all', '--limit', '100'),
                    'memory_conflicts_200': s.timed(args.runs, 'memory', 'conflicts'),
                    'investigate_include_memory': s.timed(args.runs, 'investigate', 'Modify session expiration', '--include-memory'),
                },
                'search_results_bounded': len(s.json('memory', 'search', 'session handling')['results']) <= 10,
                'conflict_scan_bounded': s.json('memory', 'conflicts')['scanned_memories'] <= 200,
            }
            check(report['scale']['search_results_bounded'] and report['scale']['conflict_scan_bounded'], 'unbounded scale query')

    report['failures'] = failures
    (args.output / 'evaluation.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({k: report[k] for k in ['search_precision_mean', 'search_recall_mean', 'incorrect_authoritative_results',
                                             'excluded_returned', 'citations', 'read_only', 'conflicts']}, indent=1))
    print('staleness', report['staleness']['correct'], '/', report['staleness']['labelled'])
    for case in report['investigations']:
        print(f"investigate {case['task']!r}: surfaced {case['surfaced']} P={case['precision']} R={case['recall']} subset={case['non_memory_steps_subset']}")
    if failures:
        print('FAILURES:')
        for failure in failures:
            print(' -', failure)
        sys.exit(1)
    print('pass')


if __name__ == '__main__':
    main()
