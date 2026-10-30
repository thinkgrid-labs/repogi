# Public architecture

Repogi is a Rust workspace: `core` defines graph facts and response types; `index` discovers source, parses Tree-sitter syntax and resolves supported bindings; `store` persists SQLite/FTS5 snapshots; `retrieval` ranks context and traverses relationships; `cli` exposes commands and read-only stdio MCP tools.

BLAKE3 hashes let explicit indexing skip unchanged parsing. Source edits trigger atomic graph replacement from cached syntax; configuration changes invalidate affected resolution state. Queries return source identities, uncertainty and truncation metadata. Git impact uses separate snapshot caches under `.code-intel/impact/`. Indexing never executes source projects.

See [semantic scope](SEMANTICS.md), [module resolution](MODULE_RESOLUTION.md), [JSX](JSX_SEMANTICS.md), [CLI overview](../README.md) and [MCP API](MCP.md).

## Markdown project knowledge

`core::knowledge` defines documents, sections, separate indexing counters and evidence hits. `index::markdown` uses CommonMark offsets to build disjoint sections with hierarchical headings. It shares ignore traversal, bounded text reads and BLAKE3 hashing with code indexing. A small `pulldown-cmark` dependency handles syntax boundaries instead of approximating Markdown with heading regular expressions.

`store` adds schema migration 3 for documents, sections and a separate FTS5 index. Ownership is the existing recorded repository root. Write opens validate existing ownership before migrating. Migration is additive and transactional; read opens accept old schemas and never migrate. The inventory update commits code, documents, FTS cleanup, run report and shared revision atomically. Changed documents alone do not rebind code. Document revisions record their last content update; the global revision identifies the query snapshot.

`retrieval::knowledge` supplies lexical section candidates; the existing context packer handles budgets and overlap. CLI `knowledge search` has its own evidence envelope and whole-response byte budget. Explicit context scopes opt into document candidates; code remains the default. Documents have `recorded_intent` confidence, not resolved code relationships. Selected-file reads can report stale or missing evidence but cannot certify the whole document inventory. MCP exposes optional scopes on the existing read-only `atlas_context`; tool names and repository authorization remain intact.

The document index is rebuildable evidence storage; it does not persist reviewed project memories. See [Knowledge](KNOWLEDGE.md) for supported behavior, limits, migration details and source-evidence semantics.
