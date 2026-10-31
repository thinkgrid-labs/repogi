#!/usr/bin/env python3
"""Offline, labeled source-relationship evaluation. No project code is executed."""
import argparse
from collections import deque
import hashlib
import json
from pathlib import Path
import shutil
import sqlite3
import statistics
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--runs', type=int, default=9)
    args = parser.parse_args()
    assert args.runs >= 1
    repo = Path(__file__).resolve().parents[2]
    fixture = repo / 'fixtures/knowledge-relationships'
    labels = json.loads((fixture / 'cases.json').read_text())
    binary = args.binary.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='repogi-relationships-') as temporary:
        root = Path(temporary) / 'fixture'
        shutil.copytree(fixture, root)

        def run(*command):
            result = subprocess.run([str(binary), *command], cwd=root, capture_output=True, check=True, timeout=30)
            return result.stdout

        cold_start = time.perf_counter()
        cold = json.loads(run('index', '.'))
        cold_ms = (time.perf_counter() - cold_start) * 1000
        database = root / '.code-intel/index.db'
        with sqlite3.connect(database) as connection:
            entities = [json.loads(r[0]) for r in connection.execute('SELECT data FROM knowledge_entities')]
            links = [json.loads(r[0]) for r in connection.execute('SELECT data FROM knowledge_relationships')]
        by_id = {e['id']: e for e in entities}
        actual_req = {(e['explicit_id'], e['source']['path'], e['source']['start_line']) for e in entities if e['kind'] == 'requirement'}
        expected_req = {tuple(v) for v in labels['requirements']}
        labeled = {(v[0], v[1]): (v[2], v[3]) for v in labels['links']}
        actual = {(by_id[e['source_id']]['explicit_id'], e['target']): e for e in links if e['kind'] != 'CONTAINS'}
        validated = {k for k, e in actual.items() if e['status'] == 'validated'}
        expected_valid = {k for k, (status, _) in labeled.items() if status == 'validated'}
        correct_valid = {k for k in validated & expected_valid if actual[k]['target_path'] == labeled[k][1]}
        false_confirmed = len(validated - correct_valid)
        status_correct = sum(k in actual and actual[k]['status'] == status for k, (status, _) in labeled.items())
        citation_correct = 0
        for edge in actual.values():
            span = edge['evidence']
            source = (root / span['path']).read_bytes()
            excerpt = source[span['start_byte']:span['end_byte']].decode()
            line = source[:span['start_byte']].count(b'\n') + 1
            end_line = source[:span['end_byte']].count(b'\n') + (not source[:span['end_byte']].endswith(b'\n'))
            citation_correct += edge['target'] in excerpt and line == span['start_line'] and end_line == span['end_line']
        before = database.read_bytes()
        latencies, outputs = [], []
        for _ in range(args.runs):
            started = time.perf_counter()
            outputs.append(run('knowledge', 'trace', 'REQ-AUTH-001', '--depth', '2', '--format', 'json', '--max-bytes', '1000000'))
            latencies.append((time.perf_counter() - started) * 1000)
        response = json.loads(outputs[0])
        assert all(o == outputs[0] for o in outputs)
        assert response['used_bytes'] == len(outputs[0]) <= 1_000_000
        assert before == database.read_bytes()
        # Independent fixture BFS oracle against every persisted incident edge.
        selected = response['selected_ids'][0]
        pending = deque([(selected, 0)])
        seen, expected_edges = {selected}, set()
        while pending:
            entity_id, level = pending.popleft()
            if level == 2:
                continue
            for edge in links:
                if entity_id in (edge['source_id'], edge['target_id']):
                    expected_edges.add(edge['id'])
                    for next_id in (edge['source_id'], edge['target_id']):
                        if next_id and next_id not in seen:
                            seen.add(next_id)
                            pending.append((next_id, level + 1))
        returned_edges = {e['id'] for e in response['relationships']}
        warm = json.loads(run('index', '.'))
        warm_correct = warm['index_revision'] == cold['index_revision'] and warm['documents']['changed'] == 0
        # A changed target is stale at read, then rebound without document parsing.
        target = root / 'src/session.ts'
        target.write_text(target.read_text() + '\nexport const changed = true;\n')
        stale = json.loads(run('knowledge', 'trace', 'REQ-AUTH-001', '--depth', '1', '--format', 'json', '--max-bytes', '100000'))
        stale_detected = any(e['target'] == '../src/session.ts' and e['freshness'] == 'stale' for e in stale['relationships'])
        changed = json.loads(run('index', '.'))
        incremental_correct = changed['files_changed'] == 1 and changed['documents']['changed'] == 0 and stale_detected
        target.unlink()
        deleted = json.loads(run('index', '.'))
        missing = json.loads(run('knowledge', 'trace', 'REQ-AUTH-001', '--depth', '1', '--format', 'json', '--max-bytes', '100000'))
        deletion_correct = deleted['documents']['changed'] == 0 and any(e['target'] == '../src/session.ts' and e['status'] == 'stale' and e['target_id'] is None for e in missing['relationships'])
        # Restore target and add an intent with no lexical overlap with source names.
        shutil.copyfile(fixture / 'src/session.ts', target)
        (root / 'docs/context.md').write_text('# REQ-QUASAR-001: Quasar handling\n### Related implementation\n[Code](../src/session.ts)\n')
        run('index', '.')
        context = json.loads(run('context', 'quasar', '--scope', 'code,docs', '--max-tokens', '16000'))
        paths = {i['path'] for i in context['items']}
        relevant = {'docs/context.md', 'src/session.ts'}
        report = {
            'scope': 'small labeled fixture, Linux host CLI subprocess timings including selected-file checks; no agent or tokenizer claims',
            'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
            'requirement_precision': len(actual_req & expected_req) / max(len(actual_req), 1),
            'requirement_recall': len(actual_req & expected_req) / len(expected_req),
            'requirement_occurrences': len(actual_req),
            'link_resolution_precision': len(correct_valid) / max(len(validated), 1),
            'link_resolution_recall': len(correct_valid) / len(expected_valid),
            'false_confirmed_links': false_confirmed,
            'validated_links': len(validated),
            'link_status_correct': status_correct, 'labeled_links': len(labeled),
            'missing_reporting_correct': all(actual[k]['status'] == 'unresolved' for k, (s, _) in labeled.items() if s == 'unresolved'),
            'ambiguous_reporting_correct': all(actual[k]['status'] == 'ambiguous' and actual[k]['target_id'] is None for k, (s, _) in labeled.items() if s == 'ambiguous'),
            'correct_source_citations': citation_correct, 'source_citations': len(actual),
            'traversal_edges': len(returned_edges), 'oracle_edges': len(expected_edges),
            'traversal_complete_within_depth': returned_edges == expected_edges,
            'depth_frontier_reported': response['depth_limit_reached'],
            'warm_revision_preserved': warm_correct,
            'target_change_without_document_reparse': incremental_correct,
            'target_deletion_invalidated': deletion_correct,
            'read_only_database_bytes_preserved': True,
            'trace_deterministic': True, 'trace_runs': args.runs,
            'trace_median_ms': statistics.median(latencies), 'trace_min_ms': min(latencies),
            'fresh_index_ms': cold_ms, 'initial_index_bytes': len(before),
            'context_path_precision': len(paths & relevant) / max(len(paths), 1),
            'context_path_recall': len(paths & relevant) / len(relevant),
            'context_paths': sorted(paths),
        }
        (args.output / 'evaluation.json').write_text(json.dumps(report, indent=2) + '\n')
        (args.output / 'trace.json').write_bytes(outputs[0])
        (args.output / 'trace.txt').write_bytes(run('knowledge', 'trace', 'REQ-AUTH-001', '--depth', '1', '--max-bytes', '8000'))
        assert false_confirmed == 0, 'release-blocking false confirmed link'
        assert actual_req == expected_req
        assert set(actual) == set(labeled) and status_correct == len(labeled)
        assert citation_correct == len(actual)
        assert returned_edges == expected_edges
        assert warm_correct and incremental_correct and deletion_correct
        assert relevant <= paths
        print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
