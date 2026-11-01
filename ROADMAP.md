# Roadmap

No dates. Order within each table is the intended order of work. Status: **In progress**, **Planned**, **Exploring**, **Research**. Shipped features are listed in the [README](README.md).

## P0 — Release and evaluation

| Item | Status | Done when |
| --- | --- | --- |
| 0.1.2 release | In progress | JavaScript, `init`, `doctor` and agent setup pass release checks on all targets; publishing requires approval |
| Install and onboarding | Planned | Scripted README quick start passes on every published target |
| Agent-task evaluation | In progress | Harness, tasks and graders are in source; done when live paired runs are published and third parties can reproduce them |
| Windows | Exploring | Full test suite passes natively on Windows |

## P1 — Developer experience

| Item | Status | Done when |
| --- | --- | --- |
| `repogi doctor` | In progress | Read-only check of index, memory, freshness and agent setup, with fixes (in source; ships in 0.1.2) |
| `repogi init` | In progress | First index and Git ignore rules, never overwrites files (in source; ships in 0.1.2) |
| Agent setup | In progress | Project-scoped MCP config for Claude Code and Codex in source (`repogi agent add`); other clients later |
| Optional `repogi.yaml` | Exploring | Include/exclude and defaults; cannot widen access beyond the repo |
| Opt-in `repogi watch` | Exploring | Foreground incremental re-indexing; explicit indexing stays default |

## P1 — Languages

Each language needs syntax extraction, module resolution, reference semantics and a labeled evaluation. Parsing alone is not support.

| Item | Status | Done when |
| --- | --- | --- |
| Shared analyzer architecture | In progress | New languages added without changing TypeScript results (JavaScript changed only coverage wording; Python next) |
| JavaScript, JSX, MJS, CJS, MTS, CTS | In progress | In source: resolved references and impact, zero wrong confirmed edges on the labeled fixture. Next: CommonJS bindings, instance dispatch, more labeled repositories |
| Python | Exploring | Imports, then resolved direct calls; dynamic code stays unconfirmed |
| Rust | Exploring | Cargo workspaces, then resolved references for macro-free code |
| Go | Exploring | Packages, modules and resolved references |

## P1 — Repository intelligence

| Item | Status | Done when |
| --- | --- | --- |
| Discovery across vocabulary gaps | Exploring | Higher recall on labeled tasks, no precision loss |
| Compact JSON and progressive retrieval | Exploring | More evidence per byte; `investigate` gains `context`-style detail levels |
| Bounded multi-hop knowledge traversal | Exploring | Depth limits, cycle handling and lineage; gain shown on labeled tasks |
| More precise test-impact candidates | Exploring | Higher precision and recall on labeled change sets |
| Monorepo workspaces and package `exports` | Exploring | Cross-package imports resolve with provenance |
| Mixed-language graph | Exploring | Each result names its package, language and resolution status |

## P2 — Knowledge and memory

| Item | Status | Done when |
| --- | --- | --- |
| OpenAPI, YAML, JSON, SQL and config indexing | Exploring | Source-located, parsed never executed, with negative tests |
| Git-aware memory relocation | Exploring | Memories survive renames and are re-verified |
| Memory conflict review | Exploring | Conflicts show both claims, evidence and actions |
| Signed reviewer attestations (optional) | Exploring | Verified on read and import; unsigned stores unchanged |
| Multi-repository queries (explicitly authorized) | Exploring | Only configured repos queried; indexes and memories stay separate |

## Research

Pursued only if evaluation shows value: local semantic search, hybrid graph and lexical retrieval, VS Code/LSP integration, graph visualization.

## Contributing

Report wrong *confirmed* results, add labeled fixtures, test on macOS and Linux, or open a design issue before adding a language. See the [README](README.md#documentation-and-contributing).
