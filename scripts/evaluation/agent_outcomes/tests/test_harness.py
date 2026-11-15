#!/usr/bin/env python3
"""Offline tests for the agent-outcome harness. No model is called: the live
adapter runs against tests/fake_claude.py."""
from __future__ import annotations

import copy
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from harness import adapters, grading, report, runner, suite as suites, workspace as ws  # noqa: E402

SUITE = suites.load(HERE.parent / 'tasks/suite.json')
FAKE = HERE / 'fake_claude.py'
NODE = shutil.which('node')
# Any real Repogi build works; the repogi condition indexes the workspace with it.
REPOGI = next((p for p in (os.environ.get('REPOGI_BINARY'), str(runner.default_binary())) if p and Path(p).is_file()),
              None)


def task(tid):
    return next(t for t in SUITE['tasks'] if t['id'] == tid)


class SuiteValidation(unittest.TestCase):
    def test_bundled_suite_is_valid(self):
        self.assertEqual(suites.validate({k: v for k, v in SUITE.items() if not k.startswith('_')},
                                         Path(SUITE['_dir'])), [])

    def test_rejects_ungraded_tasks_and_leaky_prompts(self):
        bad = copy.deepcopy({k: v for k, v in SUITE.items() if not k.startswith('_')})
        bad['tasks'][0]['grading'] = []
        bad['tasks'][1]['prompt'] += ' See the hidden tests.'
        bad['tasks'][2]['grading'].append({'type': 'agent_says_done'})
        problems = ' '.join(suites.validate(bad, Path(SUITE['_dir'])))
        self.assertIn('no graders', problems)
        self.assertIn('grader material', problems)
        self.assertIn('unknown grader', problems)

    def test_digest_tracks_harness_and_grader_sources(self):
        digest = suites.digest(SUITE)
        self.assertEqual(set(digest), {'suite_json', 'fixture_tree', 'hidden_tree', 'reference_tree',
                                       'harness_sources'})


class Graders(unittest.TestCase):
    def setUp(self):
        self.workspace = ws.create(Path(SUITE['_fixture']))

    def tearDown(self):
        ws.remove(self.workspace)

    def test_answer_paths_parses_formatting_and_rejects_extras(self):
        spec = task('tests-for-apply-tax')['grading'][1]
        good = 'text\n**TESTS:** `tests/tax.test.js`, ./tests/checkout.test.js, tests/orders.test.js'
        self.assertTrue(grading.answer_paths(spec, None, self.workspace, good, [])[0])
        self.assertFalse(grading.answer_paths(spec, None, self.workspace, good + ', tests/cart.test.js', [])[0])
        self.assertFalse(grading.answer_paths(spec, None, self.workspace, good + ', tests/nope.test.js', [])[0])
        self.assertFalse(grading.answer_paths(spec, None, self.workspace, 'no line', [])[0])

    def test_citations_must_exist_in_the_snapshot(self):
        spec = task('architecture-integer-cents')['grading'][1]
        good = 'CITATIONS: docs/adr/ADR-001-integer-cents.md:13, src/money/cents.js:3-8'
        self.assertTrue(grading.answer_citations(spec, None, self.workspace, good, [])[0])
        self.assertFalse(grading.answer_citations(spec, None, self.workspace, good + ', src/money/cents.js:900', [])[0])
        self.assertFalse(grading.answer_citations(spec, None, self.workspace, 'CITATIONS: src/money/cents.js:3', [])[0])

    def test_untracked_and_deleted_files_count_as_changes(self):
        (self.workspace / 'notes.txt').write_text('x')
        (self.workspace / 'src/legacy/format.js').unlink()
        (self.workspace / '.code-intel').mkdir()
        (self.workspace / '.code-intel/index.db').write_text('state')
        self.assertEqual(ws.changed_files(self.workspace), ['notes.txt', 'src/legacy/format.js'])
        graded = grading.grade(task('bugfix-negative-price'), Path(SUITE['_dir']), self.workspace, '')
        self.assertEqual(graded['outcome'], 'failed' if NODE else 'infrastructure_failure')

    def test_missing_node_is_infrastructure_not_task_failure(self):
        with mock.patch.object(grading.shutil, 'which', return_value=None):
            graded = grading.grade(task('bugfix-negative-price'), Path(SUITE['_dir']), self.workspace, '')
        self.assertEqual(graded['outcome'], 'infrastructure_failure')


class StreamParsing(unittest.TestCase):
    def test_reported_usage_only(self):
        lines = [
            json.dumps({'type': 'system', 'subtype': 'init', 'model': 'm', 'claude_code_version': '1',
                        'mcp_servers': [{'name': 'repogi', 'status': 'failed'}]}),
            json.dumps({'type': 'assistant', 'message': {'id': 'a', 'content': [
                {'type': 'tool_use', 'name': 'mcp__repogi__atlas_context', 'input': {}},
                {'type': 'tool_use', 'name': 'Read', 'input': {'file_path': '/w/x.js'}}]}}),
            json.dumps({'type': 'assistant', 'message': {'id': 'a', 'content': [
                {'type': 'tool_use', 'name': 'Read', 'input': {'file_path': '/w/x.js'}}]}}),
            'not json',
            json.dumps({'type': 'result', 'subtype': 'error_max_turns', 'is_error': True, 'result': '',
                        'usage': {'input_tokens': 5}}),
        ]
        transcript = adapters.parse_stream(lines)
        self.assertEqual(transcript.stop_reason, 'turn_limit')
        self.assertFalse(transcript.mcp_connected)
        self.assertEqual(transcript.repogi_calls, 1)
        self.assertEqual(transcript.files_read, ['/w/x.js'])
        self.assertEqual(transcript.telemetry['model_calls'], 1)
        self.assertEqual(transcript.telemetry['tool_calls'], 3)
        self.assertEqual(transcript.telemetry['input_tokens'], 5)
        self.assertIsNone(transcript.telemetry['output_tokens'])
        self.assertIsNone(transcript.telemetry['cost_usd'])

    def test_missing_result_is_an_agent_error(self):
        self.assertEqual(adapters.parse_stream([]).stop_reason, 'agent_error')


class Statistics(unittest.TestCase):
    def test_wilson_and_sign_test(self):
        self.assertEqual(report.wilson(0, 0), None)
        self.assertEqual(report.wilson(5, 10), [0.237, 0.763])
        self.assertEqual(report.sign_test(0, 0), None)
        self.assertEqual(report.sign_test(5, 0), 0.0625)
        self.assertEqual(report.sign_test(3, 3), 1.0)

    def test_infrastructure_failures_leave_the_pairs(self):
        def record(tid, condition, outcome, cost=None):
            return {'key': f'{tid}#r1#{condition}', 'task': tid, 'repeat': 1, 'condition': condition,
                    'category': 'bug_fix', 'outcome': outcome, 'mode': 'live', 'telemetry': {'cost_usd': cost},
                    'elapsed_s': 1.0}
        records = [record('a', 'baseline', 'failed', 0.2), record('a', 'repogi', 'passed', 0.1),
                   record('b', 'baseline', 'passed'), record('b', 'repogi', 'infrastructure_failure')]
        plan = {'digest': 'x', 'suite': {}, 'adapter': {}, 'repogi': {},
                'sessions': [{'key': r['key']} for r in records]}
        summary = report.summarize(plan, records)
        self.assertEqual(summary['paired']['repogi_only'], 1)
        self.assertEqual(summary['paired']['excluded'], 1)
        self.assertEqual(summary['paired']['cost_ratio_repogi_over_baseline']['median'], 0.5)
        self.assertEqual(summary['conditions']['repogi']['success_rate'], 1.0)
        self.assertTrue(summary['live_agent_measurement'])


@unittest.skipUnless(NODE, 'node is required to grade')
class Runs(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.argv = self.tmp / 'argv.json'
        os.environ['FAKE_CLAUDE_ARGV'] = str(self.argv)
        os.environ['FAKE_CLAUDE_MODE'] = 'fix'
        self.binary = Path(REPOGI).resolve() if REPOGI else self.tmp / 'not-repogi'
        if not REPOGI:
            self.binary.write_text('placeholder; repogi condition tests skipped')

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def plan(self, adapter, tasks=('bugfix-negative-price',), conditions=None, **limits):
        plan = runner.build_plan(SUITE, list(tasks), adapter, repogi=self.binary, repeats=1, seed=1,
                                 limits={'timeout_s': limits.get('timeout_s', 60), 'total_budget_usd': 1.0})
        if conditions:
            plan['sessions'] = [s for s in plan['sessions'] if s['condition'] in conditions]
            plan['digest'] = runner.plan_digest(plan)
        return plan

    def fake(self):
        return adapters.ClaudeCode(str(FAKE), 'fake-model')

    def test_live_adapter_needs_the_exact_digest(self):
        plan = self.plan(self.fake(), conditions=['baseline'])
        for digest in (None, '0' * 64):
            with self.assertRaises(runner.PlanError):
                runner.run(plan, SUITE, self.fake(), self.tmp / 'out', authorize_live=digest, log=lambda _: None)
        self.assertFalse(self.argv.exists(), 'the agent must not start without authorization')

    def test_stale_or_edited_plans_are_refused(self):
        plan = self.plan(adapters.MockReference(Path(SUITE['_dir'])), conditions=['baseline'])
        edited = copy.deepcopy(plan)
        edited['limits']['timeout_s'] = 5
        with self.assertRaisesRegex(runner.PlanError, 'digest'):
            runner.run(edited, SUITE, adapters.MockReference(Path(SUITE['_dir'])), self.tmp / 'o1', log=lambda _: None)
        stale = copy.deepcopy(plan)
        stale['suite']['hidden_tree'] = '0' * 64
        stale['digest'] = runner.plan_digest(stale)
        with self.assertRaisesRegex(runner.PlanError, 'hidden_tree'):
            runner.run(stale, SUITE, adapters.MockReference(Path(SUITE['_dir'])), self.tmp / 'o2', log=lambda _: None)

    def test_fake_live_session_is_graded_independently(self):
        plan = self.plan(self.fake(), conditions=['baseline'])
        out = self.tmp / 'out'
        results = runner.run(plan, SUITE, self.fake(), out, authorize_live=plan['digest'], log=lambda _: None)
        record = runner.load_results(results)[0]
        self.assertEqual(record['outcome'], 'passed', record.get('grading'))
        self.assertEqual(record['telemetry']['cost_usd'], 0.0123)
        self.assertEqual(record['tools'], {'Read': 1, 'Grep': 1})
        argv = json.loads(self.argv.read_text())
        self.assertIn('--strict-mcp-config', argv)
        self.assertNotIn('mcp__repogi', argv[argv.index('--allowedTools') + 1])
        config = json.loads((out / record['raw_log']).with_suffix('.mcp.json').read_text())
        self.assertEqual(config, {'mcpServers': {}})

    @unittest.skipUnless(REPOGI, 'set REPOGI_BINARY or build target/release/repogi')
    def test_repogi_condition_indexes_and_connects(self):
        plan = self.plan(self.fake(), conditions=['repogi'])
        out = self.tmp / 'out'
        runner.run(plan, SUITE, self.fake(), out, authorize_live=plan['digest'], log=lambda _: None)
        record = runner.load_results(out / 'results.jsonl')[0]
        self.assertEqual(record['outcome'], 'passed', record.get('grading'))
        self.assertEqual(record['repogi_calls'], 1)
        self.assertGreater(record['indexing']['index_bytes'], 0)
        argv = json.loads(self.argv.read_text())
        self.assertIn('mcp__repogi', argv[argv.index('--allowedTools') + 1])
        server = json.loads((out / record['raw_log']).with_suffix('.mcp.json').read_text())['mcpServers']['repogi']
        self.assertEqual(server['command'], str(self.binary))
        self.assertEqual(server['args'][:2], ['mcp', '--root'])

    def test_timeout_is_incomplete_and_crash_is_not_success(self):
        os.environ['FAKE_CLAUDE_MODE'] = 'sleep'
        plan = self.plan(self.fake(), conditions=['baseline'], timeout_s=2)
        out = self.tmp / 'timeout'
        runner.run(plan, SUITE, self.fake(), out, authorize_live=plan['digest'], log=lambda _: None)
        self.assertEqual(runner.load_results(out / 'results.jsonl')[0]['outcome'], 'incomplete')
        os.environ['FAKE_CLAUDE_MODE'] = 'crash'
        plan = self.plan(self.fake(), conditions=['baseline'])
        out = self.tmp / 'crash'
        runner.run(plan, SUITE, self.fake(), out, authorize_live=plan['digest'], log=lambda _: None)
        record = runner.load_results(out / 'results.jsonl')[0]
        self.assertEqual(record['stop_reason'], 'agent_error')
        self.assertEqual(record['outcome'], 'failed')

    def test_resume_never_reruns_graded_sessions(self):
        mock = adapters.MockNoop()
        plan = self.plan(mock, conditions=['baseline'])
        out = self.tmp / 'out'
        runner.run(plan, SUITE, mock, out, log=lambda _: None)
        results = out / 'results.jsonl'
        record = runner.load_results(results)[0]
        self.assertEqual(record['outcome'], 'failed')
        runner.run(plan, SUITE, mock, out, retry_infrastructure=True, log=lambda _: None)
        self.assertEqual(len(runner.load_results(results)), 1)
        record['outcome'] = 'infrastructure_failure'
        results.write_text(json.dumps(record) + '\n')
        runner.run(plan, SUITE, mock, out, log=lambda _: None)
        self.assertEqual(len(runner.load_results(results)), 1, 'retries are opt-in')
        runner.run(plan, SUITE, mock, out, retry_infrastructure=True, log=lambda _: None)
        history = runner.load_results(results)
        self.assertEqual([r['attempt'] for r in history], [1, 2])
        other = self.plan(mock, tasks=('feature-discount',), conditions=['baseline'])
        with self.assertRaisesRegex(runner.PlanError, 'different plan'):
            runner.run(other, SUITE, mock, out, log=lambda _: None)


def _record(tid, condition, outcome, cost=None, **extra):
    return {'key': f'{tid}#r1#{condition}', 'task': tid, 'repeat': 1, 'condition': condition,
            'category': 'bug_fix', 'outcome': outcome, 'mode': 'live', 'telemetry': {'cost_usd': cost},
            'elapsed_s': 1.0, **extra}


class PairedAccounting(unittest.TestCase):
    def test_indeterminate_is_excluded_not_counted_as_failure(self):
        records = [_record('a', 'baseline', 'passed'), _record('a', 'repogi', 'indeterminate'),
                   _record('b', 'baseline', 'failed'), _record('b', 'repogi', 'passed')]
        plan = {'digest': 'x', 'suite': {}, 'adapter': {}, 'repogi': {}, 'sessions': [{'key': r['key']} for r in records]}
        summary = report.summarize(plan, records)
        self.assertEqual(summary['paired']['excluded_indeterminate'], 1)
        self.assertEqual(summary['paired']['baseline_only'], 0)
        self.assertEqual(summary['paired']['repogi_only'], 1)
        self.assertEqual(summary['conditions']['repogi']['graded_sessions'], 1)
        self.assertEqual(summary['conditions']['repogi']['success_rate'], 1.0)
        self.assertEqual(summary['conditions']['repogi']['outcomes']['indeterminate'], 1)

    def test_unrun_planned_blocks_are_counted(self):
        records = [_record('a', 'baseline', 'passed'), _record('a', 'repogi', 'passed'), _record('b', 'baseline', 'failed')]
        keys = [r['key'] for r in records] + ['b#r1#repogi', 'c#r1#baseline', 'c#r1#repogi']
        plan = {'digest': 'x', 'suite': {'id': 's', 'fixture_tree': 'f'}, 'adapter': {'name': 'claude-code'},
                'repogi': {'sha256': 'r', 'tools': 't'}, 'sessions': [{'key': k} for k in keys]}
        paired = report.summarize(plan, records)['paired']
        self.assertEqual((paired['blocks'], paired['not_run'], paired['both_passed']), (3, 2, 1))
        self.assertIn('not run 2', report.markdown(report.summarize(plan, records)))

    def test_end_to_end_time_includes_cold_index(self):
        records = [_record('a', 'baseline', 'passed'),
                   _record('a', 'repogi', 'passed', indexing={'cold_s': 2.5, 'warm_s': 0.5, 'index_bytes': 10})]
        plan = {'digest': 'x', 'suite': {}, 'adapter': {}, 'repogi': {}, 'sessions': [{'key': r['key']} for r in records]}
        summary = report.summarize(plan, records)
        self.assertEqual(summary['conditions']['repogi']['end_to_end_s']['median'], 3.5)
        self.assertEqual(summary['conditions']['repogi']['elapsed_s']['median'], 1.0)
        self.assertEqual(summary['conditions']['baseline']['end_to_end_s']['median'], 1.0)
        self.assertEqual(summary['indexing']['warm_s']['median'], 0.5)


class Planning(unittest.TestCase):
    def test_order_is_counterbalanced_per_task_and_overall(self):
        mock = adapters.MockNoop()
        binary = Path(sys.executable)
        plan = runner.build_plan(SUITE, None, mock, repogi=binary, repeats=2, seed=7, limits={})
        first = {(s['task'], s['repeat']): s['condition'] for s in plan['sessions'] if s['position'] == 0}
        for t in SUITE['tasks']:
            self.assertEqual({first[(t['id'], 1)], first[(t['id'], 2)]}, set(runner.CONDITIONS), t['id'])
        single = runner.build_plan(SUITE, None, mock, repogi=binary, repeats=1, seed=7, limits={})
        leading = [s['condition'] for s in single['sessions'] if s['position'] == 0]
        self.assertEqual(leading.count('baseline'), leading.count('repogi'))

    def test_plan_records_provenance_inputs(self):
        plan = runner.build_plan(SUITE, None, adapters.MockNoop(), repogi=Path(sys.executable), repeats=1, seed=1,
                                 limits={})
        self.assertIn('commit', plan['source'])
        self.assertIn('node', plan['grader_environment'])
        self.assertIn('reference_tree', plan['suite'])
        self.assertIn('error', plan['repogi']['mcp_surface'], 'python is not a Repogi MCP server')
        self.assertIn('WebFetch', plan['disallowed_tools']['both_conditions'])


@unittest.skipUnless(NODE, 'node is required to grade')
class Hardening(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.env = mock.patch.dict(os.environ, {'FAKE_CLAUDE_ARGV': str(self.tmp / 'argv.json'),
                                                'FAKE_CLAUDE_MODE': 'fix'})
        self.env.start()
        self.argv = self.tmp / 'argv.json'
        self.binary = Path(REPOGI).resolve() if REPOGI else Path(sys.executable)

    def tearDown(self):
        self.env.stop()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def fake(self):
        return adapters.ClaudeCode(str(FAKE), 'fake-model')

    def plan(self, adapter, tasks=('bugfix-negative-price',), conditions=('baseline',), binary=None, **limits):
        limits = {'timeout_s': 60, 'total_budget_usd': 1.0, **limits}
        plan = runner.build_plan(SUITE, list(tasks), adapter, repogi=binary or self.binary, repeats=1, seed=1,
                                 limits=limits)
        plan['sessions'] = [s for s in plan['sessions'] if s['condition'] in conditions]
        plan['digest'] = runner.plan_digest(plan)
        return plan

    def go(self, plan, adapter=None, out='out', **kwargs):
        adapter = adapter or self.fake()
        authorize = plan['digest'] if adapter.live else None
        runner.run(plan, SUITE, adapter, self.tmp / out, authorize_live=authorize, log=lambda _: None, **kwargs)
        return runner.load_results(self.tmp / out / 'results.jsonl')

    def test_authorization_covers_every_plan_input(self):
        plan = self.plan(self.fake())
        mutations = {
            'model': lambda p: p['adapter'].__setitem__('model', 'other-model'),
            'agent version': lambda p: p['adapter'].__setitem__('version', '9.9.9'),
            'agent executable': lambda p: p['adapter'].__setitem__('executable_sha256', '0' * 64),
            'repogi binary': lambda p: p['repogi'].__setitem__('sha256', '0' * 64),
            'repogi tools': lambda p: p['repogi'].__setitem__('tools', 'atlas_context'),
            'task set': lambda p: p.__setitem__('sessions', p['sessions'][:0]),
            'repetitions': lambda p: p.__setitem__('repeats', 5),
            'session budget': lambda p: p['limits'].__setitem__('max_budget_usd_per_session', 9.0),
            'total budget': lambda p: p['limits'].__setitem__('total_budget_usd', 99.0),
            'timeout': lambda p: p['limits'].__setitem__('timeout_s', 3600),
            'allowed tools': lambda p: p['allowed_tools'].append('Bash'),
            'grader node': lambda p: p['grader_environment'].__setitem__('node', 'v0.0.0'),
            **{f'suite {key}': (lambda key: lambda p: p['suite'].__setitem__(key, '0' * 64))(key)
               for key in suites.digest(SUITE)},
        }
        for name, mutate in mutations.items():
            edited = copy.deepcopy(plan)
            mutate(edited)
            self.assertNotEqual(runner.plan_digest(edited), plan['digest'], name)
            for redigest in (False, True):
                if redigest:
                    edited['digest'] = runner.plan_digest(edited)
                with self.assertRaises(runner.PlanError, msg=name):
                    runner.run(edited, SUITE, self.fake(), self.tmp / 'refused', authorize_live=plan['digest'],
                               log=lambda _: None)
        self.assertFalse(self.argv.exists(), 'no mutated plan may start the agent')

    def test_draft_plans_never_run(self):
        plan = runner.build_plan(SUITE, ['bugfix-negative-price'], adapters.MockNoop(), repogi=self.binary,
                                 repeats=1, seed=1, limits={}, draft=['agent version'])
        with self.assertRaisesRegex(runner.PlanError, 'draft'):
            runner.run(plan, SUITE, adapters.MockNoop(), self.tmp / 'out', log=lambda _: None)

    def test_unknown_cost_is_charged_its_cap_and_never_reported(self):
        os.environ['FAKE_CLAUDE_MODE'] = 'nocost'
        plan = self.plan(self.fake(), tasks=('bugfix-negative-price', 'feature-discount'),
                         max_budget_usd_per_session=0.5, total_budget_usd=0.9)
        records = self.go(plan)
        self.assertEqual(len(records), 1, 'the second session could push spend past the total')
        self.assertIsNone(records[0]['telemetry']['cost_usd'])
        self.assertEqual(runner.budget_charge(records[0], 0.5), 0.5)

    def test_budget_reserves_one_session_cap(self):
        plan = self.plan(self.fake(), tasks=('bugfix-negative-price', 'feature-discount'),
                         max_budget_usd_per_session=0.5, total_budget_usd=1.0)
        self.assertEqual(len(self.go(plan)), 2)
        self.argv.unlink()
        plan = self.plan(self.fake(), tasks=('bugfix-negative-price', 'feature-discount'),
                         max_budget_usd_per_session=0.5, total_budget_usd=0.4)
        self.assertEqual(self.go(plan, out='tight'), [])
        self.assertFalse(self.argv.exists(), 'a cap larger than the total budget starts nothing')

    def test_stop_file_and_duration_limit(self):
        plan = self.plan(adapters.MockNoop())
        (self.tmp / 'out').mkdir()
        (self.tmp / 'out' / 'STOP').write_text('')
        self.assertEqual(self.go(plan, adapters.MockNoop()), [])
        (self.tmp / 'out' / 'STOP').unlink()
        self.assertEqual(len(self.go(plan, adapters.MockNoop())), 1)
        short = self.plan(adapters.MockNoop(), max_total_duration_s=1)
        self.assertEqual(self.go(short, adapters.MockNoop(), out='short'), [])

    def test_reported_model_mismatch_is_infrastructure_and_halts(self):
        os.environ['FAKE_CLAUDE_MODEL'] = 'some-other-model'
        plan = self.plan(self.fake(), tasks=('bugfix-negative-price', 'feature-discount'))
        records = self.go(plan)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]['outcome'], 'infrastructure_failure')
        self.assertIn('some-other-model', records[0]['halt'])

    def test_baseline_with_mcp_servers_is_not_a_baseline(self):
        os.environ['FAKE_CLAUDE_MODE'] = 'extramcp'
        record = self.go(self.plan(self.fake()))[0]
        self.assertEqual(record['outcome'], 'infrastructure_failure')
        self.assertIn('user-server', record['error'])

    @unittest.skipUnless(REPOGI, 'set REPOGI_BINARY or build target/release/repogi')
    def test_repogi_session_without_a_connected_server_is_not_graded(self):
        os.environ['FAKE_CLAUDE_MODE'] = 'nomcp'
        record = self.go(self.plan(self.fake(), conditions=('repogi',)))[0]
        self.assertEqual(record['outcome'], 'infrastructure_failure')
        self.assertNotIn('grading', record)
        self.assertNotIn('halt', record)

    def test_agent_is_denied_web_and_harness_and_peeking_is_indeterminate(self):
        os.environ['FAKE_CLAUDE_MODE'] = 'peek'
        os.environ['FAKE_CLAUDE_PEEK'] = str(HERE.parent / 'tasks/hidden/bugfix-negative-price/format.test.mjs')
        record = self.go(self.plan(self.fake()))[0]
        self.assertEqual(record['outcome'], 'indeterminate')
        self.assertTrue(record['contamination'])
        argv = json.loads(self.argv.read_text())
        denied = argv[argv.index('--disallowedTools') + 1].split(',')
        for rule in ('WebFetch', 'WebSearch', f'Read(/{runner.ROOT}/**)', f'Edit(/{runner.ROOT}/**)',
                     f'Read(/{(self.tmp / "out").resolve()}/**)'):
            self.assertIn(rule, denied)
        self.assertTrue(denied[2].startswith('Read(//'), 'absolute-path rules use the // form')

    def test_live_runs_refuse_workspaces_inside_denied_trees(self):
        plan = self.plan(self.fake())
        with mock.patch.object(runner.tempfile, 'gettempdir', return_value=str(runner.ROOT / 'tmp')):
            with self.assertRaisesRegex(runner.PlanError, 'TMPDIR'):
                runner.run(plan, SUITE, self.fake(), self.tmp / 'out', authorize_live=plan['digest'],
                           log=lambda _: None)

    def test_results_carry_provenance(self):
        plan = self.plan(self.fake())
        record = self.go(plan)[0]
        prov = record['provenance']
        self.assertEqual(prov['repogi_sha256'], plan['repogi']['sha256'])
        self.assertEqual(prov['agent']['model'], 'fake-model')
        self.assertEqual(prov['agent']['executable_sha256'], runner.file_sha256(FAKE))
        self.assertEqual(prov['agent']['version'], '0.0.0-fake (Claude Code)')
        self.assertEqual(prov['source_commit'], plan['source']['commit'])
        self.assertEqual(prov['grader_node'], plan['grader_environment']['node'])
        self.assertEqual(len(prov['suite_digest']), 64)

    def test_kept_workspaces_leave_the_shared_temp_directory(self):
        records = self.go(self.plan(adapters.MockReference(Path(SUITE['_dir']))),
                          adapters.MockReference(Path(SUITE['_dir'])), keep_workspaces=True)
        kept = Path(records[0]['workspace'])
        self.assertTrue(kept.is_relative_to((self.tmp / 'out').resolve()))
        self.assertTrue((kept / '.grader').is_dir())

    def test_interrupt_kills_the_agent_process_group(self):
        os.environ['FAKE_CLAUDE_MODE'] = 'sleep'
        workspace = ws.create(Path(SUITE['_fixture']))
        self.addCleanup(ws.remove, workspace)
        session = adapters.Session(task('bugfix-negative-price'), 'baseline', workspace, 'p', {'mcpServers': {}},
                                   [], {'timeout_s': 60}, self.tmp / 'raw.jsonl')
        with mock.patch.object(adapters.subprocess.Popen, 'communicate', side_effect=KeyboardInterrupt), \
                mock.patch.object(adapters.os, 'killpg', wraps=os.killpg) as killpg:
            with self.assertRaises(KeyboardInterrupt):
                self.fake().run(session)
        killpg.assert_called_once()

    def test_index_without_json_report_is_infrastructure(self):
        script = self.tmp / 'not-repogi.sh'
        script.write_text('#!/bin/sh\necho not json\n')
        script.chmod(0o755)
        mock_ref = adapters.MockReference(Path(SUITE['_dir']))
        record = self.go(self.plan(mock_ref, conditions=('repogi',), binary=script), mock_ref)[0]
        self.assertEqual(record['outcome'], 'infrastructure_failure')
        self.assertIn('no JSON', record['error'])


if __name__ == '__main__':
    unittest.main()
