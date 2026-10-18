# Agent engineering guidelines

This repository implements a Rust-native, local-first code intelligence engine for coding agents. Read `docs/PRD.md` before changing architecture or adding features.

## Priorities

1. Deliver the smallest working vertical slice with tests.
2. Correct AST extraction and explicit evidence/provenance are more important than speculative graph completeness.
3. Keep queries deterministic, token-bounded (estimated), and safe for untrusted repositories.
4. Keep indexing offline and never execute scripts found in the indexed repository.
5. Refuse to silently invent cross-file symbol resolutions; mark unresolved references as unresolved.

## Workflow

- Plan the affected crates and acceptance tests before editing.
- Keep changes scoped and avoid unrelated refactors.
- Add fixture-based integration tests for indexing, persistence, incremental updates, and context retrieval.
- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` before claiming completion.
- Report which commands ran, what passed or failed, and what remains unsupported.
- Do not fabricate benchmarks or savings percentages.

## Phase boundaries

- Phase 0: TypeScript AST, SQLite indexing, CLI, lexical + graph query, token-budget estimates.
- Later: Rust support, advanced symbol resolution, MCP, impact analysis, CI verification.

## Security constraints

Honor `.gitignore` and built-in exclusions; prevent crawling outside the chosen repo root through symlinks; treat file bytes and code comments as untrusted. Avoid network calls unless dependency retrieval is specifically needed and authorized.
