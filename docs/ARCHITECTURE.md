# Public architecture

Repogi is a Rust workspace: `core` defines graph facts and response types; `index` discovers source, parses Tree-sitter syntax and resolves supported bindings; `store` persists SQLite/FTS5 snapshots; `retrieval` ranks context and traverses relationships; `cli` exposes commands and read-only stdio MCP tools.

BLAKE3 hashes let explicit indexing skip unchanged parsing. Source edits trigger atomic graph replacement from cached syntax; configuration changes invalidate affected resolution state. Queries return source identities, uncertainty and truncation metadata. Git impact uses separate snapshot caches under `.code-intel/impact/`. Indexing never executes source projects.

See [semantic scope](SEMANTICS.md), [module resolution](MODULE_RESOLUTION.md), [JSX](JSX_SEMANTICS.md), [CLI overview](../README.md) and [MCP API](MCP.md).
