# Rust Agent Code Intelligence — Product Requirements (Draft v0.1)

> **Working codename:** `code-intel`. This is not a final package, crate, or brand name.
> **Status:** Draft / architecture and first implementation slice.
> **Primary users:** Developers using AI coding assistants; maintainers of CI pipelines.

## 1. Product thesis

A local-first, Rust-native code intelligence and verification engine that helps AI coding agents **understand more, read less, and change code with evidence**.

It retains the core value of Graphify-like tools: parse source locally, create a persistent code graph, retrieve a compact and relevant subset of that graph for a task, and reuse the graph between sessions. It then adds change-aware impact analysis, affected-test discovery, and verification.

## 2. Problem

AI coding assistants often repeat expensive repo searches, read whole files for small tasks, and rely on incomplete intuition about dependencies. They may also make changes without systematically verifying which behavior is affected.

The engine should reduce unnecessary context tokens **without hiding information needed for correctness**, while providing verifiable explanations for what changed and why related files/tests matter.

## 3. Success criteria

### V0.1 — demonstrable vertical slice

- A single local CLI indexes a small TypeScript repository without an LLM API call.
- Index persists in SQLite and can be reused in another process.
- First-pass extraction includes source files, top-level declarations, imports, exports, and their source ranges.
- A query returns ranked snippets with `path:start_line-end_line` evidence, capped by a requested context token **estimate**.
- An unchanged second index run skips re-parsing files and reports counts.
- JSON output is machine readable; text output is human readable.
- End-to-end fixtures cover indexing, querying, ignored files, repeated runs, and malformed input.

### Broader product metrics (later)

- Median and p95 index/search latency.
- Peak RSS and on-disk graph size.
- *Whole-task* input/output token usage versus plain agent search and a Graphify-like baseline.
- Task success and test pass rates; evidence completeness and impact-analysis precision/recall.
- Cold-index cost and warm-index reuse over multiple tasks; no claims of token savings before measurement.

## 4. Product pillars

1. **Token-efficient repository context:** file/symbol graph, lexical search, bounded context packs, incremental retrieval.
2. **Change intelligence:** Git-diff mapping to symbols, downstream edges, test candidates, confidence tags.
3. **Verification:** targeted test execution, explicit architectural constraints, evidence-based machine-readable reports.

## 5. Scope by phase

### Phase 0 — first executable milestone (implement first)

- CLI: `code-intel index <path>`, `code-intel status`, `code-intel context "<task>" --max-tokens 2000 --format json|text`.
- Rust workspace, `clap`, `tree-sitter` + TypeScript grammar, `rusqlite`, `ignore`, `blake3`, `serde`.
- File discovery honors `.gitignore`; excludes `node_modules`, `.git`, generated/binary files and large files by default.
- SQLite database at `.code-intel/index.db` by default, which must itself be ignored by indexing.
- File content hash to support skip-unchanged incremental runs; transactionally replace extracted records for modified files; remove deleted files.
- AST-based extraction for TypeScript: file, named top-level functions, classes, interfaces, type aliases, methods where robust, import statements and export declarations. Source-range provenance required for every symbol.
- Query combines SQLite FTS5 lexical search with nearby graph relationships; returns ranked compact context, with path/line sources.
- JSON output includes `query`, `index_revision`, `budget`, `estimated_tokens`, `items[]` with `id`, `kind`, `path`, `start_line`, `end_line`, `snippet`, `score`, `why`, `confidence`.
- A best-effort token estimator is explicitly labeled as an estimate. Use conservative truncation to prevent obvious budget overflow. Real provider token totals may differ.
- Tests on a small fixture repository; `cargo fmt`, `cargo clippy`, and `cargo test` pass.

### Phase 1 — richer code intelligence

- Add Rust grammar/parser and pluggable language interfaces.
- Symbol identity/cross-file resolution, improved graph traversal and edge provenance. `calls` edges must be labeled heuristic unless language-semantic evidence exists.
- Query API with bounded neighborhood traversal, selected file summaries and progressive disclosure.
- MCP stdio tools `index_repository`, `get_task_context`, `find_symbol`, `explain_dependencies`.
- File watching and background incremental updates (not necessary for Phase 0).

### Phase 2 — change impact

- `impact --base <ref>` from Git diff, impacted files/symbols, direct versus inferred edges.
- Test candidates with traceable reasons; never claim complete coverage from a partial graph.
- Evidence and confidence reporting, exported JSON.

### Phase 3 — verification / CI

- Run selected tests through constrained adapters; capture machine-readable evidence.
- Architectural policies, CI integration, agent hooks and `verify` reports.
- Benchmark suite and reproducible comparisons to baselines.

## 6. Non-goals for Phase 0

- No cloud service, hosted graph database, embeddings, or mandatory model API.
- No semantic analysis that pretends Tree-sitter syntax alone resolves dynamic references.
- No runtime traces, general code execution, web UI, VS Code extension, or multi-agent orchestration.
- No guaranteed savings or correctness conclusions without measurement.
- No attempt to support dozens of languages immediately.

## 7. Architecture principles

- **Local-first:** files never leave the machine unless users explicitly opt into an integration.
- **Evidence over guesses:** every edge/snippet records `source_path`, source range, extraction method and confidence.
- **Incremental from day one:** hash/change detection and schema versioning included in the first milestone.
- **Deterministic:** same repository snapshot and config should yield equivalent indexes/responses.
- **Agent-neutral:** first-class CLI, then optional MCP. Codex/Claude/Cursor are consumers, not hard dependencies.
- **Secure input:** respect exclusions, prevent symlink traversal outside the root, treat repo contents as untrusted data, never run discovered scripts while indexing.
- **Honest retrieval:** distinguish returned evidence from inferred dependencies; if information exceeds the budget, say what was omitted.

## 8. Tentative data model

- `repositories(id, root, schema_version, created_at)`
- `files(id, repository_id, relative_path, language, content_hash, size_bytes, last_index_revision)`
- `symbols(id, file_id, kind, name, qualified_name, start_byte, end_byte, start_line, end_line, signature)`
- `edges(id, repository_id, from_symbol_id, to_symbol_id, from_file_id, to_file_id, kind, source_path, source_line, extraction_method, confidence)`
- `index_runs(id, repository_id, revision, started_at, finished_at, files_scanned, files_changed, files_removed, errors)`
- Search index: SQLite FTS5 over relevant symbol identifiers and compact text/signature records.

Schema may evolve. Avoid storing absolute user paths inside exported context responses. Use migrations from the start.

### Graph facts

- `contains` (file → symbol)
- `imports` (file → file/module reference; unresolved imports retained as text)
- `exports` (file → symbol)
- `calls` (later; distinguish inferred from resolved)
- `references` (later)

Do not emit a resolved `calls` edge merely because two names match.

## 9. CLI contract for Phase 0

```shell
code-intel index .
code-intel status
code-intel context "Add rate limiting to the authentication endpoint" --max-tokens 2000 --format json
```

Examples of valid command output shapes, not fabricated runtime benchmark claims:

```json
{
  "query": "Add rate limiting to the authentication endpoint",
  "index_revision": 1,
  "budget": {"max_tokens": 2000, "method": "estimated"},
  "estimated_tokens": 730,
  "items": [
    {
      "id": "symbol:example",
      "kind": "function",
      "path": "src/auth.ts",
      "start_line": 9,
      "end_line": 21,
      "snippet": "export function authenticate(...) { /* truncated for example */ }",
      "score": 0.91,
      "why": ["matched auth", "exported function"],
      "confidence": "syntax"
    }
  ],
  "truncated": false
}
```

## 10. Recommended workspace boundaries

- `crates/core`: domain models, typed error and graph facts, query/result types.
- `crates/index`: discovery, hashing, AST extraction and indexing orchestration.
- `crates/store`: SQLite migrations, persistence, FTS queries.
- `crates/retrieval`: ranked results, graph expansion, snippet packing/budget handling.
- `crates/cli`: binary and user-facing formatters.

Keep this modular but prefer completing the vertical slice over creating empty crates. `mcp`, `impact`, and `verify` come later.

## 11. Testing & benchmarking plan

Phase 0 fixtures should include: basic TypeScript module imports; re-export; multi-file symbol names; comments with code-like text; ignored `node_modules`; renamed/deleted files; syntax error file; non-UTF8/binary input; symlink to outside repo; oversized source; empty query; 50-token budget; repeat index with unchanged content; changed content invalidating old symbol records.

Benchmark against a baseline of `rg` + opening whole files using identical task prompts and repository snapshots. Track **complete agent task** consumption rather than the isolated MCP response. Publish per-repo and per-task distributions and failures, not only averages or headline reduction ratios.

## 12. Open design questions

- Final project and binary name (the current codename is provisional).
- Whether token counting should use a model-specific tokenizer library or a conservative approximate default with optional plugins.
- Relative versus content-based stable symbol IDs under file moves/renames.
- Which test harness best evaluates context relevance independently of the agent model.

## 13. First merge criteria

Phase 0 is done when indexing a fixture TypeScript repo creates SQLite state, `context` returns traceable symbols and snippets within an estimated budget, a second index skips unchanged files, and tests + lint checks pass. Do not implement MCP or test runners before this works.
