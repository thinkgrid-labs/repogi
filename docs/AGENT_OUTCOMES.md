# Agent outcome benchmark

Does giving a coding agent Repogi improve the outcome of real development tasks, compared with the same agent using only its normal tools? The harness in [`scripts/evaluation/agent_outcomes/`](../scripts/evaluation/agent_outcomes/) measures that with paired, independently graded sessions.

> **Status:** the harness, task suite and graders are implemented and tested offline. **No live agent results exist yet**, so Repogi makes no claim about task success, tokens, time or cost. A live run needs an agent account and explicit approval of a reviewed plan.

## Design

| Element | What the harness does |
| --- | --- |
| Conditions | **baseline**: the agent with Read, Edit, Write, Glob, Grep and a few Bash commands (`node --test`, `git diff`, `git status`). **repogi**: the same tools plus a Repogi MCP server (navigation tools, `atlas_knowledge_trace`, `atlas_investigate`). Normal search stays available; Repogi is an extra evidence source. |
| Controls | Same model, agent version, task text, permitted tools, time limit and per-session budget. No extra prompt in the Repogi condition: the tool descriptions are the only difference. `--strict-mcp-config` and project-only setting sources keep user-level MCP servers, memories and instructions out of both conditions. |
| Isolation | Every session gets a fresh copy of the task repository, committed as a disposable Git snapshot. Nothing carries over between sessions or conditions. |
| Order | Sessions run in (task, repeat) blocks. Blocks are shuffled with a recorded seed, and the condition that goes first alternates. |
| Indexing | Measured outside agent time: a cold `repogi index` and an unchanged warm re-index, with index size, per Repogi session. End-to-end Repogi cost is agent time plus cold indexing. |
| Grading | Independent of what the agent says. Hidden tests are copied into the workspace only after the session ends, then run with Node.js; scope checks compare the Git diff with an allowed set; question tasks must end with a structured line whose paths and line numbers are checked against the original snapshot. |
| Outcomes | `passed`, `failed`, `incomplete` (timeout, budget or turn limit), `infrastructure_failure` (agent failed to start, MCP server did not connect, grader tool missing) and `indeterminate` (grader error). Infrastructure failures are excluded from success rates and pairs. |
| Telemetry | Only what the agent reports. For Claude Code: input, output and cache tokens, model calls, tool calls by name, files read, Repogi calls and the reported cost. Missing values stay `null`; nothing is estimated from text length. Wall-clock time is measured by the harness. |
| Reports | `results.jsonl` (one record per attempt, append-only), a JSON summary and Markdown: per-condition outcomes with Wilson 95% intervals, paired wins and losses with an exact sign test, medians of reported telemetry, missing-telemetry counts, indexing cost, per-task raw outcomes and limitations. |

## Tasks

The bundled suite, `ledgerlite-v1`, uses a small mixed JavaScript/TypeScript repository with an ADR and a requirement document ([`fixtures/agent-outcomes/repo`](../fixtures/agent-outcomes/repo)):

| Task | Category | Graded by |
| --- | --- | --- |
| `bugfix-negative-price` | bug fix with a same-named deprecated decoy | hidden tests, project tests, allowed files, decoy untouched |
| `feature-free-shipping` | requirements-driven (REQ-12) | hidden tests, allowed files |
| `refactor-rename-session` | rename across JS, TSX and tests | hidden tests, project tests, old name absent, exact file set |
| `feature-discount` | multi-module feature | hidden tests, project tests, allowed files |
| `architecture-integer-cents` | architecture question with citations | no changes; cited `path:line` must exist and cover the ADR and implementation |
| `tests-for-apply-tax` | test selection | no changes; listed tests must equal the direct and transitive users |

Prompts never mention graders or hidden material, and the suite validator refuses a task without graders or with graders inside the agent-visible repository.

## Running it

Everything except a live run works offline and costs nothing:

```sh
python3 scripts/evaluation/agent_outcomes/run.py validate          # every reference solution passes, every no-op fails
python3 scripts/evaluation/agent_outcomes/tests/test_harness.py    # harness tests (a fake agent executable, no model)
cargo build --release --locked -p code-intel-cli
python3 scripts/evaluation/agent_outcomes/run.py plan --adapter mock-reference --output /tmp/plan.json
python3 scripts/evaluation/agent_outcomes/run.py run --plan /tmp/plan.json --out /tmp/run
python3 scripts/evaluation/agent_outcomes/run.py report --out /tmp/run
```

A live run starts from a reviewed plan. The plan records the suite, fixture, hidden-test and harness digests, the Repogi binary hash, the agent executable, version and model, limits, seed and the exact session order, and gets its own digest:

```sh
python3 scripts/evaluation/agent_outcomes/run.py plan --adapter claude-code --model <model-id> \
  --repeats 3 --max-budget-usd-per-session 0.50 --total-budget-usd 6 --output plan.json
# review plan.json, then:
python3 scripts/evaluation/agent_outcomes/run.py run --plan plan.json --out results/ --authorize-live <plan digest>
python3 scripts/evaluation/agent_outcomes/run.py report --out results/ --json summary.json --markdown report.md
```

`run` refuses a live adapter without the exact digest, a plan whose contents or inputs changed since planning, a different agent version or model, and an output directory that holds another plan's results. It stops starting sessions once reported spend reaches the plan's total budget. Re-running resumes: recorded sessions are never re-run, and `--retry-infrastructure` retries only infrastructure failures, keeping the earlier attempt in the history.

Requirements: Python 3.10+, Git, Node.js 20+ (graders), a Repogi build and, for live runs, an authenticated Claude Code CLI.

## Adapters

`mock-reference` applies the task's reference solution and `mock-noop` changes nothing; neither calls a model and both report no telemetry. They validate graders and the pipeline, and their results say nothing about agents. `claude-code` runs `claude -p` with `stream-json` output. Adding another agent means implementing one `run(session) -> transcript` method that records only what that agent reports.

## Sources of bias

- One small repository and six tasks: results describe this suite, not repositories in general.
- The fixture is part of the Repogi source tree, so a model may have seen similar code; both conditions share that exposure.
- Repogi's tool descriptions add a fixed amount of context to every request in the Repogi condition.
- Agents and models change between versions; results are tied to the recorded version and model.
- With subscription authentication, Claude Code's reported cost is a list-price estimate, not billing.
- A few sessions per task cannot establish a reliable difference; reports show raw outcomes and intervals instead.
- Graders were validated against reference solutions written by the benchmark author.
