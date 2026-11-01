# Agent outcome harness

Paired sessions of one coding agent with and without Repogi, graded independently. Method, controls and limitations: [docs/AGENT_OUTCOMES.md](../../../docs/AGENT_OUTCOMES.md). Standard library Python only.

```text
run.py                  validate | plan | run | report
harness/suite.py        suite schema, validation and content digests
harness/workspace.py    disposable Git workspaces and change detection
harness/adapters.py     mock-reference, mock-noop, claude-code (stream-json parser)
harness/grading.py      hidden tests, scope checks, answer-line checks
harness/runner.py       plans, digest gating, resume/retry, indexing measurement
harness/report.py       Wilson intervals, sign test, telemetry distributions, Markdown
tasks/suite.json        ledgerlite-v1 task definitions (agent-visible prompts + grader specs)
tasks/hidden/<task>/    hidden tests, copied in only after a session ends
tasks/reference/<task>/ reference solutions, used only by validation and mock-reference
tests/                  offline tests; fake_claude.py stands in for the agent CLI
```

The task repository is [`fixtures/agent-outcomes/repo`](../../../fixtures/agent-outcomes/repo).

## Adding a task

1. Add the prompt and graders to `tasks/suite.json`. Prompts state the goal, not the solution, and never mention graders.
2. Put hidden tests in `tasks/hidden/<id>/*.test.mjs` (they run from `<workspace>/.grader/<id>/`).
3. Put a reference solution in `tasks/reference/<id>/` (changed files, or `ANSWER.md` for question tasks).
4. Run `python3 run.py validate`: the reference must pass and an untouched workspace must fail.

Changing any task, grader, fixture or harness file changes the suite digest, so existing plans become stale and must be re-planned.
