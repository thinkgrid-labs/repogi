# Agent investigation evaluation fixture

A small, labeled TypeScript authentication service with a PRD (`REQ-AUTH-001..003`, `REQ-AUDIT-001`, duplicate `REQ-DUP-001`), three ADRs (one superseded), explicit implementation claims, static callers and consumers, conventional tests (`*.test.ts`, `__tests__/*.spec.ts`), an unresolved same-name member call, unresolved and ambiguous references, and a Markdown note containing prompt-injection text. `cases.json` labels the relevant and acceptable evidence, expected uncertainty and plan categories for seven tasks, plus the validated links, static edges and forbidden confirmations used as correctness oracles.

Run the evaluator from the Repogi repository root:

```sh
python3 scripts/evaluation/agent_investigation.py target/release/repogi --output /tmp/repogi-investigation-results
```

The evaluator copies this fixture to a temporary directory and never runs its TypeScript or tests. Any false confirmed relationship or fabricated citation fails the run. Results describe these labels only; they establish neither agent task success nor behavior on other repositories.
