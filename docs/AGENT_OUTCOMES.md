# Agent outcome benchmark

Does giving a coding agent Repogi improve the outcome of real development tasks, compared with the same agent using only its normal tools? The harness in [`scripts/evaluation/agent_outcomes/`](../scripts/evaluation/agent_outcomes/) measures that with paired, independently graded sessions.

> **Status:** the harness, task suite and graders are implemented and tested offline. **Live outcomes are pending authorization: no live agent results exist yet**, so Repogi makes no claim about task success, tokens, time or cost. A live run needs an agent account and explicit approval of a reviewed, fully pinned plan.

## Design

| Element | What the harness does |
| --- | --- |
| Conditions | **baseline**: the agent with Read, Edit, Write, Glob, Grep and a few Bash commands (`node --test`, `git diff`, `git status`). **repogi**: the same tools plus a Repogi MCP server (navigation tools, `atlas_knowledge_trace`, `atlas_investigate`). Normal search stays available; Repogi is an extra evidence source. |
| Controls | Same model, agent version, task text, permitted tools, time limit and per-session budget. No extra prompt in the Repogi condition and Repogi is never required: the Repogi server's instructions and tool schemas are the only difference (the plan records their digests and size). `--strict-mcp-config` and project-only setting sources keep user-level MCP servers, memories and instructions out of both conditions. Both conditions deny `WebFetch`, `WebSearch` and reads or edits of the Repogi source tree, the suite directory and the output directory. A live session counts only if the agent's init event confirms its condition (baseline: no MCP servers; repogi: `repogi` connected) and reports the planned model and version; a model, version or baseline-server mismatch also stops the run. |
| Isolation | Every session gets a fresh copy of the task repository, committed as a disposable Git snapshot with no history. Nothing carries over between sessions or conditions; kept workspaces (`--keep-workspaces`) are moved into the denied output directory. Live runs refuse a temp directory inside the source tree or output directory. Tool inputs that name grader material or a denied path make the session `indeterminate`. These controls are best effort: `node --test` runs code the agent writes, so a determined agent could still read files on the host. For an authorized run, use a machine or container where the harness tree and network are not reachable from the agent. |
| Order | Sessions run in (task, repeat) blocks. Blocks are shuffled with a recorded seed. Which condition goes first alternates across repeats of each task and across tasks within a repeat. |
| Indexing | Measured outside agent time: a cold `repogi index` and an unchanged warm re-index, with index size, per Repogi session, reported separately. Reports also give `end_to_end_s`: agent time plus that session's cold index. |
| Grading | Independent of what the agent says. Hidden tests are copied into the workspace only after the session ends, then run with Node.js; scope checks compare the Git diff with an allowed set; question tasks must end with a structured line whose paths and line numbers are checked against the original snapshot. |
| Outcomes | `passed`, `failed`, `incomplete` (timeout, budget or turn limit), `infrastructure_failure` (agent failed to start or gave no init event, condition not confirmed, model or version mismatch, indexing failed, grader tool missing) and `indeterminate` (grader error or attempted access to grader material). Infrastructure failures and indeterminate sessions are counted but excluded from success rates and pairs. Every planned block is reported as a pair or as excluded for a stated reason (`not_run`, `excluded_infrastructure`, `excluded_indeterminate`). |
| Telemetry | Only what the agent reports. For Claude Code: input, output and cache tokens, model calls, tool calls by name, files read, Repogi calls and the reported cost. Missing values stay `null`; nothing is estimated from text length. Wall-clock time is measured by the harness. |
| Budget | The run never starts a session that could take spend past the total budget if it used its whole per-session cap. A live session with no reported cost counts as its full cap for this check only; reports keep its cost missing. The per-session cap is enforced by Claude Code between turns, so one session can slightly exceed it. |
| Provenance | Every result record carries the planned and run-time source commit, suite digest, Repogi binary SHA-256, agent name, version, executable SHA-256 and model, grader Node.js and Python versions, plus the plan digest. |
| Reports | `results.jsonl` (one record per attempt, append-only), a JSON summary and Markdown: per-condition outcomes with Wilson 95% intervals, paired wins and losses with an exact sign test, medians of reported telemetry, missing-telemetry counts, indexing and end-to-end cost, per-task raw outcomes and limitations. |

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

A live run starts from a reviewed plan. The plan's digest covers the suite JSON (prompts and graders), the fixture, hidden-test, reference and harness digests, the Repogi binary hash, tool list and MCP surface, the agent executable, its SHA-256, version, model and permission mode, allowed and denied tools, the grader Node.js version, source commit, limits (timeouts, budgets, duration), seed, repeats and the exact session order. Changing any of them changes the digest, so an earlier authorization no longer applies:

```sh
python3 scripts/evaluation/agent_outcomes/run.py plan --adapter claude-code --model <model-id> \
  --agent-executable <path to the pinned claude> --repeats 3 --max-budget-usd-per-session 1.00 \
  --total-budget-usd 36 --max-total-duration-s 25200 --output plan.json
# review plan.json, then:
python3 scripts/evaluation/agent_outcomes/run.py run --plan plan.json --out results/ --authorize-live <plan digest>
python3 scripts/evaluation/agent_outcomes/run.py report --out results/ --json summary.json --markdown report.md
```

`plan --draft` writes a reviewable plan when the agent executable or model is not available yet. Draft plans list what is still unpinned and can never be run. `plan` warns when the total budget is smaller than sessions times the per-session cap.

`run` refuses a live adapter without the exact digest, a draft or a plan whose contents or inputs changed since planning (including the grader Node.js version), a different agent executable, version or model, and an output directory that holds another plan's results. To stop a run, create `<out>/STOP` (no new session starts) or press Ctrl-C (the running agent's process group is killed and that session is not recorded). Re-running resumes: recorded sessions are never re-run, and `--retry-infrastructure` retries only infrastructure failures, keeping the earlier attempt in the history. `--max-total-duration-s` applies to each invocation.

Requirements: Python 3.10+, Git, Node.js 20+ (graders), a Repogi build and, for live runs, an authenticated Claude Code CLI.

## Adapters

`mock-reference` applies the task's reference solution and `mock-noop` changes nothing; neither calls a model and both report no telemetry. They validate graders and the pipeline, and their results say nothing about agents. `claude-code` runs `claude -p` with `stream-json` output. Adding another agent means implementing one `run(session) -> transcript` method that records only what that agent reports.

## Sources of bias

- One small repository and six tasks: results describe this suite, not repositories in general.
- The fixture is part of the Repogi source tree, so a model may have seen similar code; both conditions share that exposure.
- Repogi's server instructions and tool descriptions add a fixed amount of context (`repogi.mcp_surface.context_chars` in the plan) to every request in the Repogi condition.
- Retrying infrastructure failures, or excluding them, can bias results if failures cluster in one condition; reports show both counts per condition.
- Provider prompt caching can carry over between sessions of the same condition; cache tokens are reported separately.
- Agents and models change between versions; results are tied to the recorded version and model.
- With subscription authentication, Claude Code's reported cost is a list-price estimate, not billing.
- A few sessions per task cannot establish a reliable difference; reports show raw outcomes and intervals instead.
- Graders were validated against reference solutions written by the benchmark author.
