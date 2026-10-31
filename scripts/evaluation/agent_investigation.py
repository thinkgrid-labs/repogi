#!/usr/bin/env python3
"""Offline, labeled agent-investigation evaluation. No project code is executed.

Copies fixtures/agent-investigation to a temporary directory, indexes it with the
given binary, runs `investigate` for every labeled task and scores the returned
plan. False confirmed relationships and fabricated citations are hard failures.
"""
import argparse
import hashlib
import json
from pathlib import Path
import resource
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time

# Fixture documentation and labels are not part of the evaluated corpus.
META = shutil.ignore_patterns('README.md', 'cases.json')
CONFIRMED = {'verified_static_relationship', 'validated_knowledge_reference', 'advisory_test_candidate'}


def tree_digest(root):
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob('*') if p.is_file()):
        digest.update(str(path.relative_to(root)).encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()


def key(target):
    return target.get('explicit_id') or f"{target['location']['path']}::{target['name']}"


def simple(name):
    return name.rsplit('.', 1)[-1]


def score_plan(case, response):
    targets = {t['id']: t for t in response['targets']}
    labelled = set(case['relevant']) | set(case['acceptable'])
    returned, unnecessary_targets = [], []
    for step in response['steps']:
        for tid in step['targets']:
            k = key(targets[tid])
            if k not in returned:
                returned.append(k)
    unnecessary_targets = [k for k in returned if k not in labelled]
    found = [k for k in case['relevant'] if k in returned]
    codes = {u['code'] for s in response['steps'] for u in s['uncertainty']} | {u['code'] for u in response['uncertainty']}
    categories = [s['category'] for s in response['steps']]
    unnecessary_steps = 0
    for step in response['steps']:
        keys = {key(targets[t]) for t in step['targets']}
        step_codes = {u['code'] for u in step['uncertainty']}
        if step['category'] == 'uncertainty':
            needed = bool(step_codes & set(case['expected_uncertainty']))
        else:
            needed = bool(keys & labelled)
        unnecessary_steps += not needed
    implementation = [key(targets[t]) for s in response['steps'] if s['action'] == 'inspect_implementation' for t in s['targets']]
    return {
        'returned_targets': len(returned),
        'precision': round((len(returned) - len(unnecessary_targets)) / len(returned), 3) if returned else None,
        'recall': round(len(found) / len(case['relevant']), 3),
        'missed': [k for k in case['relevant'] if k not in returned],
        'unnecessary_targets': unnecessary_targets,
        'step_coverage': round(sum(c in categories for c in case['required_categories']) / len(case['required_categories']), 3),
        'steps': len(response['steps']),
        'unnecessary_steps': unnecessary_steps,
        'first_category_ok': bool(categories) and categories[0] == case['first_category'],
        'expected_uncertainty_reported': all(c in codes for c in case['expected_uncertainty']),
        'missing_uncertainty': [c for c in case['expected_uncertainty'] if c not in codes],
        'forbidden_implementation': bool(case.get('forbid_implementation')) and bool(implementation),
    }


def check_evidence(root, response, labels, entity_by_id):
    """Citation accuracy and confirmed-relationship correctness for one response."""
    validated = {tuple(link) for link in labels['validated_links']}
    static = {tuple(edge) for edge in labels['static_edges']}
    targets = {t['id']: t for t in response['targets']}
    cited = correct_citations = confirmed = false_confirmed = unlabeled_static = 0
    failures = []
    for ev in response['evidence']:
        span = ev['citation']
        source = (root / span['path']).read_bytes()
        cited += 1
        start_line = source[:span['start_byte']].count(b'\n') + 1
        # Same inclusive line convention as the v0.3 relationship evaluator.
        end_line = source[:span['end_byte']].count(b'\n') + (not source[:span['end_byte']].endswith(b'\n'))
        ok = span['end_byte'] <= len(source) and start_line == span['start_line'] and end_line == span['end_line']
        rel = ev.get('relationship')
        excerpt = source[span['start_byte']:span['end_byte']].decode('utf-8', 'replace')
        if ok and rel and rel.get('id') and ev['class'] != 'unresolved_reference':
            # Knowledge claims cite the literal declaration (link target or metadata value).
            ok = rel['target_text'].split('#')[0].split('/')[-1].replace('%20', ' ') in excerpt or rel['target_text'] in excerpt
        correct_citations += ok
        if not ok:
            failures.append(('citation', ev['id']))
        if ev['class'] not in CONFIRMED:
            continue
        confirmed += 1
        bad = False
        source_id, target_id = rel['source_id'], rel['target_id']
        if ev['class'] == 'validated_knowledge_reference':
            source_label = entity_by_id.get(source_id, source_id)
            target_label = entity_by_id.get(target_id, target_id)
            bad = (source_label, target_label) not in validated
        else:
            source_fact = targets.get(source_id)
            # Static edges: the cited site must name the target declaration.
            target_name = None
            if target_id in targets:
                target_name = simple(targets[target_id]['name'])
            else:
                target_name = entity_by_id.get(target_id)
            if target_name and target_name not in excerpt:
                bad = True
            source_name = source_fact['name'] if source_fact else None
            if source_name and target_name and (source_name, target_name) not in static:
                unlabeled_static += 1
        for forbidden_source, forbidden_target in labels['forbidden_confirmed']:
            source_key = entity_by_id.get(source_id) or (key(targets[source_id]) if source_id in targets else source_id)
            if (source_key == forbidden_source or source_key.startswith(forbidden_source)) and (
                forbidden_target == '*' or forbidden_target in (target_id or '')
            ):
                bad = True
        false_confirmed += bad
        if bad:
            failures.append(('false_confirmed', ev['id'], source_id, target_id))
    return cited, correct_citations, confirmed, false_confirmed, unlabeled_static, failures


def context_baseline(run, case, labels_entity):
    """Score the existing combined context workflow on the same labels."""
    output = run('context', case['task'], '--scope', 'code,docs', '--format', 'json', '--max-tokens', '12000')
    response = json.loads(output)
    keys = []
    for item in response['items']:
        if item['kind'] == 'document_section':
            k = labels_entity.get(item['id'], f"{item['path']}::section")
        else:
            k = f"{item['path']}::{item.get('name') or item['id']}"
        if k not in keys:
            keys.append(k)
    labelled = set(case['relevant']) | set(case['acceptable'])
    found = [k for k in case['relevant'] if k in keys]
    return {
        'returned': len(keys),
        'precision': round(sum(k in labelled for k in keys) / len(keys), 3) if keys else None,
        'recall': round(len(found) / len(case['relevant']), 3),
        'bytes': len(output),
    }


def synthetic(root, modules=240, requirements=40):
    """A modest generated repository: chained modules, requirements, decisions and tests."""
    (root / 'docs').mkdir(parents=True)
    (root / 'src').mkdir()
    prd = ['# Synthetic PRD\n']
    for m in range(modules):
        package = root / 'src' / f'pkg{m % 12}'
        package.mkdir(exist_ok=True)
        previous = f"import {{ compute{m - 1} }} from '../pkg{(m - 1) % 12}/module{m - 1}';\n" if m else ''
        body = f'compute{m - 1}(value) + 1' if m else 'value'
        (package / f'module{m}.ts').write_text(
            f"{previous}export const LIMIT_{m} = {m};\n"
            f"export function compute{m}(value: number): number {{\n  return {body} + LIMIT_{m};\n}}\n"
            f"export function render{m}(value: number): string {{\n  return String(compute{m}(value));\n}}\n")
        if m % 4 == 0:
            (package / f'module{m}.test.ts').write_text(
                f"import {{ compute{m} }} from './module{m}';\nexport function checks{m}(): boolean {{ return compute{m}(1) > 0; }}\n")
    for r in range(requirements):
        m = (r * 7) % modules
        prd.append(f"\n## REQ-SYN-{r:03d}: Compute stage {m}\nStage {m} computes values for billing reports.\n\n"
                   f"### Related implementation\n- [Stage](../src/pkg{m % 12}/module{m}.ts)\n")
    (root / 'docs/PRD.md').write_text(''.join(prd))
    for d in range(10):
        (root / f'docs/ADR-{d:04d}.md').write_text(
            f"---\nid: ADR-{d:04d}\nstatus: accepted\nrelates_to: REQ-SYN-{d:03d}\n---\n# Billing decision {d}\nCompute stages stay pure.\n")


def peak_rss_kib(binary, args, cwd):
    """Peak RSS of one command, measured in an isolated helper process."""
    helper = 'import resource,subprocess,sys;subprocess.run(sys.argv[1:],check=True,capture_output=True);print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)'
    result = subprocess.run([sys.executable, '-I', '-c', helper, str(binary), *args], cwd=cwd, capture_output=True, check=True, timeout=120)
    return int(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--baseline', type=Path, help='earlier binary for index/context comparisons')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--runs', type=int, default=9)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    fixture = repo / 'fixtures/agent-investigation'
    labels = json.loads((fixture / 'cases.json').read_text())
    binary = args.binary.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'tasks': [], 'checks': {}}
    with tempfile.TemporaryDirectory(prefix='repogi-investigation-') as temporary:
        root = Path(temporary) / 'fixture'
        shutil.copytree(fixture, root, ignore=META)

        def run(*command, cwd=root, check=True, exe=binary):
            result = subprocess.run([str(exe), *command], cwd=cwd, capture_output=True, check=check, timeout=60)
            return result.stdout

        started = time.perf_counter()
        run('index', '.')
        report['index_ms'] = round((time.perf_counter() - started) * 1000, 2)
        database = root / '.code-intel/index.db'
        report['index_bytes'] = database.stat().st_size
        with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as connection:
            entity_by_id = {row[0]: row[1] for row in connection.execute('SELECT id, explicit_id FROM knowledge_entities WHERE explicit_id IS NOT NULL')}
            schema = sorted(r[0] or '' for r in connection.execute('SELECT sql FROM sqlite_master'))
            user_version = connection.execute('PRAGMA user_version').fetchone()[0]
        before = tree_digest(root)
        totals = {'cited': 0, 'correct': 0, 'confirmed': 0, 'false_confirmed': 0, 'unlabeled_static': 0}
        failures = []
        latencies = {}
        deterministic = True
        for case in labels['tasks']:
            command = ['investigate', case['task'], '--format', 'json', '--max-bytes', '1000000']
            for selector in case['selectors']:
                command += ['--selector', selector]
            outputs, times = [], []
            for _ in range(args.runs):
                t0 = time.perf_counter()
                outputs.append(run(*command))
                times.append((time.perf_counter() - t0) * 1000)
            deterministic &= all(o == outputs[0] for o in outputs)
            latencies[case['id']] = round(statistics.median(times), 2)
            response = json.loads(outputs[0])
            assert response['budget']['used_bytes'] == len(outputs[0])
            scored = score_plan(case, response)
            cited, correct, confirmed, false_confirmed, unlabeled, bad = check_evidence(root, response, labels, entity_by_id)
            for name, value in zip(totals, (cited, correct, confirmed, false_confirmed, unlabeled)):
                totals[name] += value
            failures += [(case['id'], *f) for f in bad]
            default = json.loads(run('investigate', case['task'], '--format', 'json', *sum((['--selector', s] for s in case['selectors']), [])))
            tiered = json.loads(run('investigate', case['task'], '--format', 'json', '--max-bytes', '1000000', '--max-steps', '3', '--ranking', 'tiered'))
            balanced = json.loads(run('investigate', case['task'], '--format', 'json', '--max-bytes', '1000000', '--max-steps', '3'))
            entry = {
                'id': case['id'], 'category': case['category'], 'task': case['task'],
                'full': scored,
                'default_budget': score_plan(case, default) | {'used_bytes': default['budget']['used_bytes'], 'byte_truncated': default['completeness']['byte_truncated']},
                'three_steps_balanced': score_plan(case, balanced),
                'three_steps_tiered': score_plan(case, tiered),
                'context_code_docs': context_baseline(run, case, {e: v for e, v in entity_by_id.items()}),
                'completeness': response['completeness'],
                'classification': response['task']['classification'],
                'latency_ms_median': latencies[case['id']],
            }
            if case['selectors'] or response['task']['selectors']:
                selector = (case['selectors'] or response['task']['selectors'])[0]
                trace = json.loads(run('knowledge', 'trace', selector, '--depth', '1', '--format', 'json', '--max-bytes', '1000000'))
                ids = {e.get('explicit_id') or e['id'] for e in trace['entities']}
                knowledge = [k for k in case['relevant'] if '::' not in k]
                entry['knowledge_trace_knowledge_recall'] = round(sum(k in ids for k in knowledge) / len(knowledge), 3)
                entry['knowledge_trace_code_symbols'] = sum(1 for e in trace['entities'] if e['kind'] == 'code_symbol')
            report['tasks'].append(entry)
        # Budget compliance across budgets and formats.
        budget_ok = True
        for case in labels['tasks']:
            for budget in (1500, 4000, 8000, 12000, 32000):
                for fmt in ('json', 'text'):
                    result = subprocess.run([str(binary), 'investigate', case['task'], '--format', fmt, '--max-bytes', str(budget)], cwd=root, capture_output=True, timeout=60)
                    if result.returncode == 0:
                        budget_ok &= len(result.stdout) <= budget
                        if fmt == 'json':
                            budget_ok &= json.loads(result.stdout)['budget']['used_bytes'] == len(result.stdout)
                    else:
                        budget_ok &= b'budget' in result.stderr
        # Stale source: edit a target after indexing (no re-index).
        stale_root = Path(temporary) / 'stale'
        shutil.copytree(fixture, stale_root, ignore=META)
        run('index', '.', cwd=stale_root)
        session = stale_root / 'src/auth/session.ts'
        session.write_text('// shifted\n' + session.read_text())
        stale = json.loads(run('investigate', 'Modify session expiration', '--format', 'json', '--max-bytes', '1000000', cwd=stale_root))
        stale_ok = 'src/auth/session.ts' in stale['freshness']['stale_files'] and all(
            n.get('start_line') is None for s in stale['steps'] for n in s['next'] if n['tool'] == 'read' and n.get('path') == 'src/auth/session.ts'
        ) and any(u['code'] == 'stale_source' for s in stale['steps'] for u in s['uncertainty'])
        # Deleted claim target after explicit re-index.
        deleted_root = Path(temporary) / 'deleted'
        shutil.copytree(fixture, deleted_root, ignore=META)
        (deleted_root / 'src/auth/login.ts').unlink()
        run('index', '.', cwd=deleted_root)
        deleted = json.loads(run('investigate', 'Review requirement impact', '--selector', 'REQ-AUTH-003', '--format', 'json', '--max-bytes', '1000000', cwd=deleted_root))
        deleted_codes = {u['code'] for s in deleted['steps'] for u in s['uncertainty']}
        deleted_ok = not any(s['action'] == 'inspect_implementation' for s in deleted['steps']) and bool(deleted_codes & {'stale_reference', 'unresolved_reference'})
        # Injection text must never surface.
        injection_ok = True
        for task in ('Modify session expiration', 'session expiration notes instructions'):
            for fmt in ('json', 'text'):
                text = run('investigate', task, '--format', fmt, '--max-bytes', '1000000')
                injection_ok &= b'rm -rf' not in text and b'.mcp.json' not in text
        read_only = before == tree_digest(root)
        report['checks'] = {
            'false_confirmed': totals['false_confirmed'],
            'confirmed_evidence': totals['confirmed'],
            'confirmed_correct_rate': round((totals['confirmed'] - totals['false_confirmed']) / totals['confirmed'], 3) if totals['confirmed'] else None,
            'unlabeled_static_edges': totals['unlabeled_static'],
            'citations': totals['cited'],
            'citation_accuracy': round(totals['correct'] / totals['cited'], 3) if totals['cited'] else None,
            'deterministic': deterministic,
            'budget_compliant': budget_ok,
            'stale_source_reported': stale_ok,
            'deleted_target_reported': deleted_ok,
            'injection_text_absent': injection_ok,
            'read_only_tree_unchanged': read_only,
            'failures': failures,
            'schema_user_version': user_version,
        }
        report['latency_ms_median_overall'] = round(statistics.median(latencies.values()), 2)
        report['investigate_peak_rss_kib'] = peak_rss_kib(binary, ['investigate', 'Modify session expiration'], root)
        report['context_peak_rss_kib'] = peak_rss_kib(binary, ['context', 'Modify session expiration', '--scope', 'code,docs'], root)
        if args.baseline:
            baseline = args.baseline.resolve()
            base_root = Path(temporary) / 'baseline'
            shutil.copytree(fixture, base_root, ignore=META)
            run('index', '.', cwd=base_root, exe=baseline)
            base_db = base_root / '.code-intel/index.db'
            with sqlite3.connect(f'file:{base_db}?mode=ro', uri=True) as connection:
                base_schema = sorted(r[0] or '' for r in connection.execute('SELECT sql FROM sqlite_master'))
            identical_context = all(
                run('context', c['task'], '--scope', 'code,docs', '--format', 'json', '--max-tokens', '12000', cwd=base_root, exe=baseline)
                == run('context', c['task'], '--scope', 'code,docs', '--format', 'json', '--max-tokens', '12000')
                for c in labels['tasks']
            )
            report['baseline'] = {
                'sha256': hashlib.sha256(baseline.read_bytes()).hexdigest(),
                'index_bytes': base_db.stat().st_size,
                'schema_identical': base_schema == schema,
                'context_outputs_identical': identical_context,
            }
        # Medium synthetic repository: latency and memory only, no quality claims.
        synth = Path(temporary) / 'synthetic'
        synthetic(synth)
        t0 = time.perf_counter()
        run('index', '.', cwd=synth)
        synth_index_ms = (time.perf_counter() - t0) * 1000
        synth_tasks = ['Change compute stage 42 billing', 'What implements REQ-SYN-006?', 'What depends on compute120?', 'Find tests for render8']
        synth_latency = {}
        for task in synth_tasks:
            times, outputs = [], []
            for _ in range(args.runs):
                t0 = time.perf_counter()
                outputs.append(run('investigate', task, '--format', 'json', cwd=synth))
                times.append((time.perf_counter() - t0) * 1000)
            assert all(o == outputs[0] for o in outputs)
            parsed = json.loads(outputs[0])
            synth_latency[task] = {
                'median_ms': round(statistics.median(times), 2), 'steps': len(parsed['steps']),
                'work_used': parsed['budget']['work_used'], 'work_limit_reached': parsed['completeness']['work_limit_reached'],
                'depth_frontier': parsed['completeness']['depth_frontier'], 'used_bytes': parsed['budget']['used_bytes'],
            }
        report['synthetic'] = {
            'files': sum(1 for p in synth.rglob('*') if p.is_file() and '.code-intel' not in p.parts),
            'index_ms': round(synth_index_ms, 2),
            'index_bytes': (synth / '.code-intel/index.db').stat().st_size,
            'tasks': synth_latency,
            'investigate_peak_rss_kib': peak_rss_kib(binary, ['investigate', synth_tasks[2]], synth),
        }
    (args.output / 'evaluation.json').write_text(json.dumps(report, indent=2) + '\n')
    checks = report['checks']
    for entry in report['tasks']:
        full, ctx = entry['full'], entry['context_code_docs']
        print(f"{entry['id']:15} P={full['precision']} R={full['recall']} cov={full['step_coverage']} "
              f"unneeded_steps={full['unnecessary_steps']}/{full['steps']} unc={full['expected_uncertainty_reported']} "
              f"| 3-step balanced R={entry['three_steps_balanced']['recall']} cov={entry['three_steps_balanced']['step_coverage']} "
              f"tiered R={entry['three_steps_tiered']['recall']} cov={entry['three_steps_tiered']['step_coverage']} "
              f"| context P={ctx['precision']} R={ctx['recall']} | {entry['latency_ms_median']} ms")
    print(json.dumps({k: v for k, v in checks.items() if k != 'failures'}))
    assert checks['false_confirmed'] == 0, checks['failures']
    assert checks['citation_accuracy'] == 1.0, checks['failures']
    assert checks['deterministic'] and checks['budget_compliant'] and checks['read_only_tree_unchanged']
    assert checks['stale_source_reported'] and checks['deleted_target_reported'] and checks['injection_text_absent']
    assert all(not e['full']['forbidden_implementation'] for e in report['tasks'])


if __name__ == '__main__':
    main()
