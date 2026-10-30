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
