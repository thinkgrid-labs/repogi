"""Aggregate results into a JSON summary and a short Markdown report.

Small samples are shown raw with Wilson intervals and an exact sign test on
discordant pairs; nothing here turns a handful of sessions into a claim.
"""
from __future__ import annotations

import math
import statistics
from collections import Counter

from . import CONDITIONS, OUTCOMES

METRICS = ('input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens',
           'model_calls', 'tool_calls', 'cost_usd')


def wilson(successes: int, n: int, z: float = 1.96) -> list[float] | None:
    if n == 0:
        return None
    p = successes / n
    centre = (p + z * z / (2 * n)) / (1 + z * z / n)
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / (1 + z * z / n)
    return [round(max(0.0, centre - half), 3), round(min(1.0, centre + half), 3)]


def sign_test(wins: int, losses: int) -> float | None:
    """Exact two-sided binomial test of wins vs losses (ties excluded)."""
    n = wins + losses
    if n == 0:
        return None
    k = min(wins, losses)
    tail = sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n
    return round(min(1.0, 2 * tail), 4)


def distribution(values: list[float]) -> dict | None:
    if not values:
        return None
    ordered = sorted(values)
    p90 = ordered[min(len(ordered) - 1, math.ceil(0.9 * len(ordered)) - 1)]
    return {'n': len(values), 'min': ordered[0], 'median': statistics.median(ordered), 'p90': p90,
            'max': ordered[-1], 'mean': round(statistics.fmean(ordered), 4)}


def summarize(plan: dict, records: list[dict]) -> dict:
    latest = {}
    attempts = Counter()
    for record in records:
        latest[record['key']] = record
        attempts[record['key']] += 1
    planned = {s['key'] for s in plan['sessions']}
    runs = [latest[k] for k in sorted(latest) if k in planned]
    modes = sorted({r['mode'] for r in runs})
    conditions = {}
    for condition in CONDITIONS:
        rows = [r for r in runs if r['condition'] == condition]
        counts = Counter(r['outcome'] for r in rows)
        gradable = sum(counts[o] for o in OUTCOMES if o != 'infrastructure_failure')
        telemetry = {}
        for metric in METRICS:
            values = [(r.get('telemetry') or {}).get(metric) for r in rows]
            measured = [v for v in values if v is not None]
            telemetry[metric] = {'measured_sessions': len(measured), 'missing_sessions': len(values) - len(measured),
                                 'distribution': distribution(measured)}
        conditions[condition] = {
            'sessions': len(rows),
            'outcomes': {o: counts.get(o, 0) for o in OUTCOMES},
            'success_rate': round(counts['passed'] / gradable, 3) if gradable else None,
            'success_rate_wilson95': wilson(counts['passed'], gradable),
            'elapsed_s': distribution([r['elapsed_s'] for r in rows if r.get('elapsed_s') is not None]),
            'telemetry': telemetry,
            'tool_use': {
                'repogi_calls': distribution([r['repogi_calls'] for r in rows if r.get('repogi_calls') is not None]),
                'search_calls': distribution([r['search_calls'] for r in rows if r.get('search_calls') is not None]),
                'files_read': distribution([r['files_read'] for r in rows if r.get('files_read') is not None]),
            },
        }
    indexing = [r['indexing'] for r in runs if r.get('indexing')]
    pairs = paired(runs)
    return {
        'schema': 'repogi-agent-outcomes-summary/1',
        'plan_digest': plan['digest'],
        'suite': plan['suite'],
        'adapter': plan['adapter'],
        'repogi': plan['repogi'],
        'modes': modes,
        'live_agent_measurement': modes == ['live'],
        'planned_sessions': len(planned),
        'recorded_sessions': len(runs),
        'retried_sessions': sorted(k for k, n in attempts.items() if n > 1),
        'conditions': conditions,
        'paired': pairs,
        'indexing': {
            'cold_s': distribution([i['cold_s'] for i in indexing]),
            'warm_s': distribution([i['warm_s'] for i in indexing]),
            'index_bytes': distribution([i['index_bytes'] for i in indexing]),
        },
        'tasks': per_task(runs),
        'limitations': limitations(modes, runs, pairs),
    }


def paired(runs: list[dict]) -> dict:
    blocks = {}
    for r in runs:
        blocks.setdefault((r['task'], r['repeat']), {})[r['condition']] = r
    counts = Counter()
    cost_ratios = []
    for block in blocks.values():
        if set(block) != set(CONDITIONS) or any(r['outcome'] == 'infrastructure_failure' for r in block.values()):
            counts['excluded'] += 1
            continue
        base, repo = (block[c]['outcome'] == 'passed' for c in CONDITIONS)
        counts[{(True, True): 'both_passed', (False, False): 'both_failed',
                (True, False): 'baseline_only', (False, True): 'repogi_only'}[(base, repo)]] += 1
        costs = [(block[c].get('telemetry') or {}).get('cost_usd') for c in CONDITIONS]
        if all(costs) and costs[0] > 0:
            cost_ratios.append(round(costs[1] / costs[0], 4))
    return {
        'blocks': len(blocks),
        **{k: counts.get(k, 0) for k in ('both_passed', 'both_failed', 'baseline_only', 'repogi_only', 'excluded')},
        'sign_test_p': sign_test(counts['repogi_only'], counts['baseline_only']),
        'cost_ratio_repogi_over_baseline': distribution(cost_ratios),
    }


def per_task(runs: list[dict]) -> list[dict]:
    rows = []
    for tid in sorted({r['task'] for r in runs}):
        row = {'task': tid, 'category': next(r['category'] for r in runs if r['task'] == tid)}
        for condition in CONDITIONS:
            row[condition] = [r['outcome'] for r in sorted(runs, key=lambda r: r['repeat'])
                              if r['task'] == tid and r['condition'] == condition]
        rows.append(row)
    return rows


def limitations(modes, runs, pairs) -> list[str]:
    notes = []
    if 'live' not in modes:
        notes.append('No live agent was run: these are mock-adapter results that validate the harness and '
                     'graders. They say nothing about whether Repogi helps an agent.')
    if pairs['blocks'] - pairs['excluded'] < 20:
        notes.append(f"Only {pairs['blocks'] - pairs['excluded']} complete pairs: treat differences as anecdotal; "
                     'the Wilson intervals and sign test show the uncertainty.')
    if any(r['outcome'] == 'infrastructure_failure' for r in runs):
        notes.append('Infrastructure failures are excluded from success rates and paired comparisons.')
    if any(r.get('telemetry_source', '').startswith('claude-code') for r in runs):
        notes.append('Token counts and cost are what Claude Code reports; with subscription (OAuth) auth the '
                     'cost is a list-price estimate, not billing.')
    notes.append('One small fixture repository and one agent: results do not generalize to other repositories, '
                 'models or agents without further runs.')
    return notes


def markdown(summary: dict) -> str:
    lines = ['# Agent outcome benchmark', '']
    if not summary['live_agent_measurement']:
        lines += ['> **Not a live-agent measurement.** Modes: ' + ', '.join(summary['modes'] or ['none']) + '.', '']
    adapter = summary['adapter']
    lines += [
        f"- Suite: `{summary['suite']['id']}` (fixture tree `{summary['suite']['fixture_tree'][:12]}`)",
        f"- Adapter: `{adapter['name']}`" + (f" {adapter.get('version')}, model `{adapter.get('model')}`"
                                              if adapter.get('live') else ''),
        f"- Repogi binary sha256 `{summary['repogi']['sha256'][:16]}…`, tools `{summary['repogi']['tools']}`",
        f"- Plan digest `{summary['plan_digest']}`",
        f"- Sessions recorded: {summary['recorded_sessions']} of {summary['planned_sessions']}",
        '', '## Outcomes', '',
        '| Condition | Passed | Failed | Incomplete | Infra | Indeterminate | Success (95% CI) |',
        '| --- | ---: | ---: | ---: | ---: | ---: | --- |',
    ]
    for condition, data in summary['conditions'].items():
        o = data['outcomes']
        rate = data['success_rate']
        ci = data['success_rate_wilson95']
        lines.append(f"| {condition} | {o['passed']} | {o['failed']} | {o['incomplete']} | "
                     f"{o['infrastructure_failure']} | {o['indeterminate']} | "
                     + (f'{rate:.2f} [{ci[0]:.2f}, {ci[1]:.2f}]' if rate is not None else 'n/a') + ' |')
    p = summary['paired']
    lines += ['', f"Paired blocks: {p['blocks']} (both passed {p['both_passed']}, both failed {p['both_failed']}, "
              f"baseline only {p['baseline_only']}, Repogi only {p['repogi_only']}, excluded {p['excluded']}); "
              f"sign test p = {p['sign_test_p'] if p['sign_test_p'] is not None else 'n/a'}.", '',
              '## Efficiency (medians; only sessions with reported telemetry)', '',
              '| Metric | Baseline | Repogi | Missing (B/R) |', '| --- | ---: | ---: | --- |']
    for metric in METRICS + ('elapsed_s',):
        cells, missing = [], []
        for condition in summary['conditions'].values():
            if metric == 'elapsed_s':
                d, miss = condition['elapsed_s'], 0
            else:
                t = condition['telemetry'][metric]
                d, miss = t['distribution'], t['missing_sessions']
            cells.append(f"{d['median']}" if d else '—')
            missing.append(str(miss))
        lines.append(f"| {metric} | {cells[0]} | {cells[1]} | {'/'.join(missing)} |")
    idx = summary['indexing']
    if idx['cold_s']:
        lines += ['', f"Indexing (Repogi condition, outside agent time): cold median {idx['cold_s']['median']} s, "
                  f"warm median {idx['warm_s']['median']} s, index size median {idx['index_bytes']['median']} bytes."]
    lines += ['', '## Per task', '', '| Task | Category | Baseline | Repogi |', '| --- | --- | --- | --- |']
    for row in summary['tasks']:
        lines.append(f"| {row['task']} | {row['category']} | {', '.join(row['baseline'])} | {', '.join(row['repogi'])} |")
    lines += ['', '## Limitations', ''] + [f'- {note}' for note in summary['limitations']]
    return '\n'.join(lines) + '\n'
