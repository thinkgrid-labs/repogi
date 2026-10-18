# Code Intelligence Engine — Phase 0

A Rust-native, local-first TypeScript/TSX index for coding agents. Indexing is offline, never executes repository code, and needs no AI service.

```sh
cargo build --workspace
cargo run -p code-intel-cli -- index fixtures/typescript
cd fixtures/typescript
../../target/debug/code-intel status
../../target/debug/code-intel context "authenticate user" --max-tokens 2000 --format json
../../target/debug/code-intel context "authenticate user" --max-tokens 2000 --format text
```

Install the binary with `cargo install --path crates/cli`. `index <path>` writes `<path>/.code-intel/index.db`; `status` and `context` find the closest index in the current directory or its ancestors. They reuse persisted snippets without reading live source, so re-index after editing. Moving an indexed repository requires rebuilding its `.code-intel` directory.

The workspace has five crates: `core` (serializable facts/responses), `store` (SQLite schema migration, FTS5 and graph persistence), `index` (discovery, BLAKE3 and Tree-sitter), `retrieval` (ranking and packing), and `cli` (commands and formatting). SQLite is bundled with FTS5. `Cargo.lock` pins dependencies.

Indexing records file previews, named top-level functions/classes/interfaces/type aliases/enums/simple variables, direct class/interface methods with literal identifier names, import statements, and export statements. Each fact and edge has source ranges, extraction method, and confidence. Lines are inclusive and one based; byte ranges are zero based and end exclusive. IDs depend on path, kind and source offsets, so edits and moves can change IDs.

Persistent graph edges include containment, declaration exports and literal module imports. Imports retain their module text even when unresolved. A unique relative `.ts`/`.tsx` file candidate (including `index` and `.js`/`.jsx` spelling) gets an additional `module_candidate` edge labeled `path_heuristic`. This is a filename relationship, not TypeScript semantic resolution. Ambiguous candidates, external packages, aliases, dynamic imports, escaped module strings and arbitrary compiler resolution remain unresolved. Export clauses are stored as syntax facts; local export-clause bindings, computed methods, destructuring bindings, namespaces and ambient wrappers are not resolved. No calls/references graph is claimed.

Files up to 1 MiB are hashed on each run; unchanged files skip parsing. Changed/deleted/newly excluded files lose stale facts, edges and FTS records in the same transaction that installs new facts and the run report. Revision advances only on changed indexed content (or the first run); every successful run is recorded. Traversal/read failures abort without replacing the previous snapshot. Malformed syntax produces diagnostics and preserves intact top-level declarations; `syntax_error_files` remains visible in status/context even after an unchanged re-index.

Discovery respects repository and nested `.gitignore` rules, including outside Git repositories. It does not read global Git exclusions or parent ignore files. Source symlinks are skipped entirely. `.code-intel` state symlinks are rejected. Built-in excluded directory names are `node_modules`, `.git`, `.code-intel`, `dist`, `build`, `coverage`, `.next`, and `generated`. Invalid UTF-8/control-byte sources, oversized files, `.min.ts`, `.generated.ts`/`.generated.tsx`, and files with a `// @generated` header in the first five lines are skipped. These checks assume the repository is not being concurrently replaced by a malicious filesystem actor.

`context` uses safely quoted FTS5 prefix terms (up to 32 words), BM25 ordering, and deterministic two-hop graph expansion. Work is capped at 128 candidates, 16 lexical seeds, and 24 neighbors per node. The output reports truncation, omitted candidate count, and whether traversal/candidate limits were reached. Omitted counts describe considered candidates, not all potentially relevant repository facts. There are no relevance guarantees or benchmark claims.

The token budget applies to the **entire rendered response**, including query, metadata, escaping and final newline. `budget.method = "estimated_utf8_bytes"` uses one UTF-8 byte per estimated token, deliberately conservative rather than a provider tokenizer. Actual provider token counts can differ. Packing preserves whole source lines; `start_line`/`end_line` describe the returned snippet while provenance retains the full original range. A budget smaller than the empty response envelope (for example 50 tokens in JSON) fails with a minimum-budget message. Long single lines may be omitted. Unmatched queries return an empty pack. Text and JSON packing can select different items because formatting costs differ.

`index` and `status` emit JSON. Context JSON has `schema_version`, `query`, `index_revision`, `budget`, `estimated_tokens`, `items`, `truncated`, `omitted_items`, `candidate_limit_reached`, and `syntax_error_files`. Items include ID, kind, relative path, snippet range, score, reasons, confidence and provenance. Scores rank candidates within a query; they are not probabilities. Successful output goes to stdout. Runtime failures return nonzero and a JSON error on stderr with `schema_version`, `error.code` and `error.message`; argument/help output follows clap conventions.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Fixture-based CLI tests cover persistence across processes, warm runs, modification/deletion/rename, ignored/generated/binary/oversized inputs, symlinks, malformed syntax, rollback on failed writes, graph evidence, deterministic output and full-response budgets.

See [docs/PRD.md](docs/PRD.md) and [AGENTS.md](AGENTS.md) for the product scope. Rust parsing, semantic symbol resolution, embeddings, MCP, impact analysis and execution/verification are later phases.
