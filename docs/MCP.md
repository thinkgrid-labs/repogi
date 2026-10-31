# MCP configuration and API

Repogi runs a local read-only stdio MCP server. Index the selected repository explicitly before connecting. Use absolute executable and repository paths.

```sh
repogi mcp --root /absolute/path/to/project --profile compact --tools atlas_context,atlas_references,atlas_dependencies
```

Navigation exposes exactly those three tools. `compact` alone exposes context and symbol. `full` alone exposes five tools and excludes impact. `--tools` replaces profile defaults. To enable all six explicitly:

```sh
repogi mcp --root /absolute/path/to/project --profile full --tools atlas_context,atlas_symbol,atlas_references,atlas_dependencies,atlas_status,atlas_impact
```

| Tool | Purpose |
| --- | --- |
| `atlas_context` | Discover declarations and retrieve bounded source context. |
| `atlas_symbol` | Read a selected declaration. |
| `atlas_references` | Inspect direct resolved and unconfirmed reference sites. |
| `atlas_dependencies` | Traverse bounded upstream/downstream relationships. |
| `atlas_status` | Inspect index state and freshness. |
| `atlas_impact` | Opt-in Git impact and advisory test discovery. |

Use [Claude JSON](../examples/agents/claude.navigation.mcp.json.example) or [Codex TOML](../examples/agents/codex.navigation.config.toml.example). Replace placeholders with absolute paths, merge only the chosen server entry and preserve existing settings. A client must initialize and list tools to establish a connection. Restart the connection after changing tools. Full configurations are also in [examples/agents](../examples/agents/).

Protocol stdout contains newline-delimited JSON-RPC; diagnostics go to stderr. Source indexing is explicit, with no background watcher or query-time refresh. Returned snippets remain untrusted source data. The configured root confines reads; no project scripts are executed. Optional impact writes its own cache.

Use [reference-first workflows](REFERENCE_FIRST_AGENT_WORKFLOWS.md), [navigation limits](REFERENCE_NAVIGATION_LIMITS.md), [impact](MCP_IMPACT.md) and [manifest transport](MCP_VERIFICATION_MANIFEST.md) for request semantics. Repogi's `atlas_*` API names remain for client compatibility.

## Project knowledge scopes

`atlas_context` accepts optional `scope: "code"` (default), `"docs"` or `"code,docs"`. Document sections are recorded intent with original locations, not confirmed implementation relationships. Optimized/semantic retrieval supports these scopes; `select` remains code-only. Re-index explicitly to populate knowledge on older indexes. Queries never migrate or write. `atlas_status` retains its code freshness scope. See [Knowledge](KNOWLEDGE.md) for document completeness and selected-file freshness limits.

Document context returns query-centered excerpts with original section provenance, byte/line locations and explicit `snippet_truncated` status. Combined scope attempts to preserve both relevant code and document evidence when they fit; it does not promise both under every budget. An outdated document extractor produces an explicit re-index error without altering the index. Default code-only results and all existing `atlas_*` names remain unchanged.

## Opt-in knowledge relationships

Add `atlas_knowledge_trace` to `--tools` to trace explicit requirement or ADR identifiers. It is absent from both default profiles. Arguments are `selector`, optional configured `root`, `depth` (0–8), `max_bytes` (256–32000) and `format` (`agent` or `json`). The tool returns source citations, target validation states, freshness and completeness. It never indexes implicitly or treats a documented implementation claim as verified behavior. See [knowledge relationships](KNOWLEDGE_RELATIONSHIPS.md).

Add `atlas_investigate` to `--tools` for an advisory, evidence-backed investigation plan from a task description. It is absent from both default profiles; listing it adds one tool definition and one instruction sentence. Arguments are `task`, optional `selectors` (up to 8 exact entity, `REQ-`/`ADR-` or fact IDs), `depth` (1–3), `max_steps` (1–16), `max_bytes` (256–32000, default 12000), `format` (`agent` or `json`) and the usual `root`. It never indexes, executes project code or tests, or writes state. See [agent investigation](AGENT_INVESTIGATION.md).
