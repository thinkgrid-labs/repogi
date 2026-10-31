# Public architecture

Repogi is a Rust workspace: `core` defines graph facts and response types; `index` discovers source, parses Tree-sitter syntax and resolves supported bindings; `store` persists SQLite/FTS5 snapshots; `retrieval` ranks context and traverses relationships; `cli` exposes commands and read-only stdio MCP tools.

BLAKE3 hashes let explicit indexing skip unchanged parsing. Source edits trigger atomic graph replacement from cached syntax; configuration changes invalidate affected resolution state. Queries return source identities, uncertainty and truncation metadata. Git impact uses separate snapshot caches under `.code-intel/impact/`. Indexing never executes source projects.

See [semantic scope](SEMANTICS.md), [module resolution](MODULE_RESOLUTION.md), [JSX](JSX_SEMANTICS.md), [CLI overview](../README.md) and [MCP API](MCP.md).

## Markdown project knowledge

`core::knowledge` defines documents, sections, separate indexing counters and evidence hits. `index::markdown` uses CommonMark offsets to build disjoint sections with hierarchical headings. It shares ignore traversal, bounded text reads and BLAKE3 hashing with code indexing. A small `pulldown-cmark` dependency handles syntax boundaries instead of approximating Markdown with heading regular expressions.

`store` adds schema migration 3 for documents, sections and a separate FTS5 index. Ownership is the existing recorded repository root. Write opens validate existing ownership before migrating. Migration is additive and transactional; read opens accept old schemas and never migrate. A separate extractor-version marker invalidates document parsing caches when location semantics change; outdated documentation queries require explicit re-indexing. The inventory update commits code, documents, FTS cleanup, run report and shared revision and build identity atomically. Changed documents alone do not rebind code. Document revisions record their last content update; the global revision identifies the query snapshot.

`retrieval::knowledge` supplies lexical section candidates; the existing context packer handles budgets and overlap. FTS5 supplies match offsets using noncolliding control markers, and a shared window selector preserves untouched source bytes and original section provenance. Combined scopes try a relevant code/document pair before filling the remaining budget; this is a coverage preference, not a new graph edge. CLI `knowledge search` has its own evidence envelope and whole-response byte budget. Explicit context scopes opt into document candidates; code remains the default. Documents have `recorded_intent` confidence, not resolved code relationships. Selected-file reads can report stale or missing evidence but cannot certify the whole document inventory. MCP exposes optional scopes on the existing read-only `atlas_context`; tool names and repository authorization remain intact.

The document index is rebuildable evidence storage; it does not persist reviewed project memories. See [Knowledge](KNOWLEDGE.md) for supported behavior, limits, migration details and source-evidence semantics.

Unix source/config reads use descriptor-relative no-follow traversal and bounded reads; non-Unix checks retain the existing path validation. Source inspection is not a repository-wide filesystem snapshot. The foundation uses schema 3; additive schema 4 stores the separate knowledge relationship graph; no separate database, model, embeddings or remote service is introduced.

## Structured source knowledge

`core::relationships` defines typed entities, relationships, source ranges and validation states. `index::relationships` extracts explicit requirement IDs, ADR metadata and CommonMark links. `store::relationships` resolves references against the transactionally updated document/code inventory; it never augments the static TypeScript dependency graph. Migration 4 preserves the existing schema-1/2/3 paths and repository identity checks. Unchanged documents reuse extraction; changed inventories revalidate cached relationships.

`retrieval::relationships` performs bounded bidirectional trace traversal and selected-source freshness checks. Combined context expands relevant seeds through explicit validated links and carries claim citations in explanations. CLI `knowledge trace` and opt-in MCP `atlas_knowledge_trace` share the same read-only response and byte accounting. Default tool exposure and code-only retrieval stay compatible. See [the relationship contract](KNOWLEDGE_RELATIONSHIPS.md) for identity, uncertainty, migration and work limits. This remains rebuildable source knowledge, with no managed memory writes.

`retrieval::investigate` (v0.4 development milestone) is a read-only layer over the same snapshot: it reuses the lexical identifier ranking, document FTS5 search, one-hop explicit claims, the bounded upstream `navigate` walk and selected-file `signal` checks to assemble an ordered, evidence-backed plan with a shared, content-addressed evidence ledger. It adds no schema, migration or persisted state. CLI `investigate` and opt-in MCP `atlas_investigate` share one response and byte accounting. See [agent investigation](AGENT_INVESTIGATION.md).
