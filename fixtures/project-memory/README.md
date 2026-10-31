# Project memory evaluation fixture

The agent-investigation authentication service (PRD `REQ-AUTH-001..003`, `REQ-AUDIT-001`, duplicate `REQ-DUP-001`; ADR-0001 SQLite, ADR-0002 superseded by ADR-0003; code, static callers and conventional tests) plus `docs/ARCHITECTURE.md` and `docs/notes/review-notes.md`, a note whose text imitates memory approval commands and reviewer instructions.

`cases.json` labels the memory proposals the evaluator creates (and which are approved, rejected or left pending), search queries with their relevant approved memories, source mutations with the verification state each one must produce, the expected supersession chain and the expected structured conflicts.

Run from the Repogi repository root:

```sh
python3 scripts/evaluation/project_memory.py target/release/repogi --output /tmp/repogi-memory-results
```

The evaluator copies this fixture (without this README and `cases.json`) to a temporary directory, indexes it, and never runs its TypeScript or tests. Results describe these labels only; they establish neither agent task success nor behavior on other repositories.
