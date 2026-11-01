#!/usr/bin/env python3
"""Paired agent-outcome benchmark: the same coding agent with and without Repogi.

  validate   check the suite and prove every grader (reference passes, no-op fails)
  plan       write a reviewable plan with a content digest; starts no agent
  run        execute a plan; a live adapter needs --authorize-live <digest>
  report     summarize results.jsonl into JSON and Markdown

Mock adapters never call a model and report no telemetry.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import HARNESS, adapters, report, runner, suite as suites  # noqa: E402

DEFAULT_SUITE = HARNESS / 'tasks/suite.json'


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--suite', type=Path, default=DEFAULT_SUITE)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('validate')

    plan = sub.add_parser('plan')
    plan.add_argument('--adapter', required=True, choices=['mock-reference', 'mock-noop', 'claude-code'])
    plan.add_argument('--model', help='concrete model id (claude-code)')
    plan.add_argument('--agent-executable', help='path to the claude executable')
    plan.add_argument('--repogi', type=Path, default=runner.default_binary())
    plan.add_argument('--repogi-tools', default=runner.DEFAULT_REPOGI_TOOLS)
    plan.add_argument('--tasks', nargs='*')
    plan.add_argument('--repeats', type=int, default=1)
    plan.add_argument('--seed', type=int, default=20261031)
    plan.add_argument('--timeout-s', type=int, default=600)
    plan.add_argument('--max-budget-usd-per-session', type=float)
    plan.add_argument('--total-budget-usd', type=float)
    plan.add_argument('--output', type=Path, required=True)

    run = sub.add_parser('run')
    run.add_argument('--plan', type=Path, required=True)
    run.add_argument('--out', type=Path, required=True)
    run.add_argument('--authorize-live', metavar='DIGEST')
    run.add_argument('--retry-infrastructure', action='store_true',
                     help='re-run sessions whose latest attempt was an infrastructure failure')
    run.add_argument('--keep-workspaces', action='store_true')

    rep = sub.add_parser('report')
    rep.add_argument('--out', type=Path, required=True, help='directory holding plan.json and results.jsonl')
    rep.add_argument('--json', type=Path)
    rep.add_argument('--markdown', type=Path)
    args = parser.parse_args(argv)

    try:
        suite = suites.load(args.suite)
    except (suites.SuiteError, OSError, json.JSONDecodeError) as error:
        print(f'suite invalid: {error}', file=sys.stderr)
        return 2

    if args.command == 'validate':
        problems = runner.validate_graders(suite)
        print('graders valid' if not problems else '\n'.join(problems))
        return 1 if problems else 0

    if args.command == 'plan':
        if not args.repogi.is_file():
            print(f'Repogi binary not found: {args.repogi} (build with cargo build --release)', file=sys.stderr)
            return 2
        if args.adapter == 'claude-code' and (args.max_budget_usd_per_session is None or args.total_budget_usd is None):
            print('claude-code plans need --max-budget-usd-per-session and --total-budget-usd', file=sys.stderr)
            return 2
        adapter = adapters.make(args.adapter, Path(suite['_dir']), executable=args.agent_executable, model=args.model)
        problems = adapter.preflight()
        if problems:
            print('; '.join(problems), file=sys.stderr)
            return 2
        limits = {'timeout_s': args.timeout_s, 'max_budget_usd_per_session': args.max_budget_usd_per_session,
                  'total_budget_usd': args.total_budget_usd}
        built = runner.build_plan(suite, args.tasks, adapter, repogi=args.repogi.resolve(), repeats=args.repeats,
                                  seed=args.seed, limits=limits, repogi_tools=args.repogi_tools)
        args.output.write_text(json.dumps(built, indent=2) + '\n')
        print(f"plan {built['digest']} ({len(built['sessions'])} sessions) -> {args.output}")
        return 0

    if args.command == 'run':
        plan = json.loads(args.plan.read_text())
        spec = plan['adapter']
        adapter = adapters.make(spec['name'], Path(suite['_dir']), executable=spec.get('executable'),
                                model=spec.get('model'))
        try:
            results = runner.run(plan, suite, adapter, args.out, authorize_live=args.authorize_live,
                                 retry_infrastructure=args.retry_infrastructure, keep_workspaces=args.keep_workspaces)
        except runner.PlanError as error:
            print(f'refused: {error}', file=sys.stderr)
            return 2
        print(f'results -> {results}')
        return 0

    plan = json.loads((args.out / 'plan.json').read_text())
    summary = report.summarize(plan, runner.load_results(args.out / 'results.jsonl'))
    if args.json:
        args.json.write_text(json.dumps(summary, indent=2) + '\n')
    text = report.markdown(summary)
    if args.markdown:
        args.markdown.write_text(text)
    else:
        print(text)
    return 0


if __name__ == '__main__':
    sys.exit(main())
