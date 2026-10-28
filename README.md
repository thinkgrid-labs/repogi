<div align="center">
  <img src="assets/repogi-banner.svg" alt="Repogi — Repo Graph Intelligence. Know the code. Follow the evidence. A local-first code intelligence tool built in Rust." width="100%" />

# Repogi — Repo Graph Intelligence

**Local-first code intelligence and dependency graph analysis for AI coding agents.**

Understand source references, navigate dependencies, retrieve relevant code context, and trace the impact of Git changes—with source-backed evidence.

[Get started](#quick-start) · [Explore features](#features) · [CLI examples](#usage) · [MCP integration](#ai-agent-integration) · [Architecture](#how-repogi-works) · [Limitations](#current-status-and-limitations)

</div>

> **Give coding agents a map of the codebase, not just a pile of files.**

**Repogi** is an open-source, Rust-based repository intelligence tool. It indexes **TypeScript and TSX** source code locally, connects declarations and references in a **static code graph**, and returns compact, evidence-backed answers about what code does, what depends on it, and what a proposed change might affect.

Built for developers and AI coding assistants that need more than grep, Repogi offers a **command-line interface (CLI)** and a read-only **Model Context Protocol (MCP)** server. There is **no hosted service, API key, background daemon, or model dependency**. The repository being analyzed is never executed by the indexer.

**At a glance:** TypeScript/TSX · Rust · SQLite + FTS5 · Tree-sitter · BLAKE3 · Git change impact · MCP · Offline-first

## Why Repogi?

When an AI coding agent investigates an unfamiliar codebase, text search and whole-file reads are useful—but they don't reliably distinguish actual bindings from same-name matches or explain how changes propagate through imports, exports, and call sites.

Repogi gives agents a **source-backed repository graph**, so they can:

- **Find the right code** — discover declarations, callers, references, and their file/line locations.
- **Understand dependencies** — follow direct and transitive relationships across imports, aliases, and re-exports.
- **Retrieve focused context** — fetch relevant snippets under explicit response-size budgets instead of always reading whole files.
- **Review changes** — inspect which declarations and static consumers a Git change may affect.
- **Know what is uncertain** — distinguish resolved evidence from ambiguity, unsupported patterns, stale indexes, and incomplete results.

Repogi does **not** claim to prove runtime safety, perfectly resolve every TypeScript construct, or guarantee token savings. Evidence and limits come together.

## Features

| Capability | What Repogi provides |
| --- | --- |
| **Local repository indexing** | Parse `.ts` and `.tsx` files into a persistent, local SQLite graph and FTS5 search index. Respect `.gitignore` and supported exclusions. |
| **Incremental indexing** | Use BLAKE3 file hashes to skip unchanged files and update existing graph records. |
| **Symbol and reference navigation** | Inspect declarations, static calls, reference sites, JSX component use, and source ranges. |
| **Dependency graph exploration** | Follow upstream consumers and downstream dependencies with bounded traversal and completeness reporting. |
| **TypeScript module resolution** | Track relative imports, `tsconfig`/`jsconfig` aliases, re-exports, and barrel files where supported. |
| **Context retrieval** | Produce ranked, deduplicated source evidence with compact and detailed output modes. |
| **Git change impact analysis** | Compare Git states to report changed declarations and potential direct/transitive consumers. |
| **Evidence and uncertainty** | Include source locations, resolution status, confidence or limitations, and explicit truncation where applicable. |
| **CLI + MCP server** | Run locally using text/JSON CLI output or read-only stdio MCP tools for coding agents. |

**Experimental and advisory:** `impact --tests` discovers likely affected tests using static links; it does not execute tests. Verification manifests (`impact --manifest`, `manifest-verify`) and semantic retrieval (`context --retrieval semantic`) are also available, but should be treated as evolving interfaces rather than guarantees of coverage or correctness.

## Quick start

**Requirements:** Rust 1.85 or later. Git is required for change-impact commands.

Install from the source repository (Repogi is not currently published to crates.io):

```bash
git clone https://github.com/thinkgrid-labs/repogi.git
cd repogi
cargo install --path crates/cli --locked
repogi --version
```

Alternatively, build without installing:

```bash
cargo build --release --locked -p code-intel-cli
./target/release/repogi --help
```

Index a TypeScript/TSX project and ask for context:

```bash
cd /path/to/your/typescript-project
repogi index .
repogi status
repogi context "authentication flow" --format text
```

To explore the bundled TypeScript fixture from the **Repogi checkout**:

```bash
cd /path/to/repogi
repogi index fixtures/typescript
cd fixtures/typescript
repogi context "authenticate user" --format text
```

The index is stored in `.code-intel/index.db` for compatibility with earlier versions. Indexing is explicit: rerun `repogi index .` after edits to update the graph.

## Usage

### CLI commands

| Command | Purpose |
| --- | --- |
| `repogi index <path>` | Create or update a local index. |
| `repogi status` | Inspect the nearest repository index and its state. |
| `repogi context <task>` | Retrieve relevant code context and source evidence. |
| `repogi graph` | Inspect static graph relationships and unresolved sites. |
| `repogi impact [path]` | Analyze the potential impact of Git changes. |
| `repogi manifest-verify` | Check a verification manifest. |
| `repogi mcp` | Start a read-only MCP server over stdio. |

### Find code context and references

```bash
# Ranked, budgeted source evidence (JSON by default)
repogi context "session refresh" --max-tokens 2000

# A human-readable view
repogi context "session refresh" --format text

# Inspect graph edges in a specific source file
repogi graph --path src/auth.ts --limit 100
```

`--max-tokens` is a **conservative estimate** based on UTF-8 bytes, not a count from a particular model tokenizer.

### Trace Git change impact

```bash
# HEAD compared with the working tree
repogi impact --base HEAD

# Compare branches/commits
repogi impact --base main --head feature

# See transitive evidence paths
repogi impact --base main --detail --depth 3

# Advisory test candidates (does not run tests)
repogi impact --base HEAD --tests

# Machine-readable report
repogi impact --base HEAD~1 --head HEAD --format json
```

Impact analysis builds separate Git snapshot indexes under `.code-intel/impact/`; it does not change the repository's source files, Git index, or refs. A graph dependency is **not** proof that a change will break another component.

## How Repogi works

Repogi uses **Tree-sitter parsing, static binding, and SQLite graph storage** to make repository relationships accessible to coding assistants. The flow below illustrates the architecture; the animation is decorative and the diagram remains readable without motion.

<div align="center">
  <img src="assets/repogi-flow.svg" alt="Repogi architecture flow: TypeScript/TSX source becomes a Tree-sitter and static-binding index, then a SQLite evidence graph, exposed via CLI and MCP for reference navigation, dependency analysis, and Git change impact." width="100%" />
</div>

<details>
<summary>View a text-only architecture diagram</summary>

```text
TypeScript / TSX repository
          |
          v
  Discover + parse files
  Tree-sitter / BLAKE3
          |
          v
   Static bindings
   Imports / calls / references / JSX
          |
          v
  SQLite + FTS5 code graph <------- Git snapshots
          |
          +----> Reference navigation
          +----> Dependency traversal
          +----> Context retrieval
          +----> Git impact + advisory test candidates
          |
          v
     CLI / MCP server
          |
          v
      Coding agents
```

</details>

**Evidence matters.** A response can include a confirmed source binding, unresolved/ambiguous sites, an explicit incomplete-coverage flag, or an out-of-scope limitation. Repogi reports those differences rather than pretending every edge is certain.

## AI agent integration

Repogi exposes a **read-only Model Context Protocol (MCP)** server that AI coding assistants can use to navigate a project. The server is local, does not start an agent, and makes no model API calls.

| MCP tool | Purpose |
| --- | --- |
| `atlas_context` | Retrieve task-relevant declarations and context. |
| `atlas_symbol` | Inspect a declaration. |
| `atlas_references` | Find direct static reference sites. |
| `atlas_dependencies` | Traverse upstream/downstream relationships. |
| `atlas_status` | Inspect index state and freshness. |
| `atlas_impact` | Analyze change impact; opt-in only. |

The `atlas_*` tool identifiers are intentionally retained for backward compatibility with existing MCP clients and recorded evaluations. The server itself is branded Repogi.

### Claude Code

In the repository you want the agent to inspect:

```bash
repogi index .
claude mcp add --transport stdio --scope project repogi -- "$(command -v repogi)" mcp \
  --root "$PWD" --profile compact \
  --tools atlas_context,atlas_references,atlas_dependencies
```

See the [Claude Code MCP configuration example](examples/agents/claude.navigation.mcp.json.example).

### Codex

Add a configuration entry in the trusted project's `.codex/config.toml` using absolute paths:

```toml
[mcp_servers.repogi]
command = "/absolute/path/to/repogi"
args = ["mcp", "--root", "/absolute/path/to/project", "--profile", "compact", "--tools", "atlas_context,atlas_references,atlas_dependencies"]
```

See the [Codex MCP configuration example](examples/agents/codex.navigation.config.toml.example). Agents can continue using native file-read and grep tools alongside Repogi for unsupported languages or literal searches.

## Design principles

- **Local-first:** repository indexing, graph queries, and MCP responses operate on your machine.
- **Source-backed:** confirmed edges carry file/line evidence and the static rule that established them.
- **Explicit uncertainty:** unsupported, ambiguous, or incomplete relationships are not silently treated as confirmed.
- **Incremental and deterministic:** unchanged files are skipped and results have stable ordering within the supported contracts.
- **Agent-neutral:** any compatible CLI/MCP consumer can use the graph; no specific model provider is required.
- **Safe with untrusted code:** indexing does not execute scripts or arbitrary repository code, and path/symlink restrictions limit traversal.

## Current status and limitations

Repogi is a **developer preview (v0.1.0)**. It is currently built from source, with no prebuilt binaries or crates.io release listed by the project.

| Area | Current scope |
| --- | --- |
| **Languages** | TypeScript `.ts` and TSX `.tsx`. JavaScript, JSX, `.mts`, `.cts`, and other languages are not yet supported. |
| **Resolution** | Relative and selected `tsconfig`/`jsconfig` alias and barrel patterns. Package resolution, instance dispatch, and dynamic imports are not fully resolved. |
| **Parser** | Tree-sitter TypeScript grammar 0.23.2 has known syntax gaps in some constructs. |
| **Freshness** | Index updates are manual; files may be flagged stale until reindexed. |
| **Impact / tests** | Static and advisory. Results do not prove runtime failures or test coverage; affected-test discovery does not run tests. |
| **Platforms** | Developed and tested primarily on Linux x86_64; other platforms are not verified here. |
| **Agent outcomes** | Limited pilot evidence only; no general token, accuracy, or cost-saving guarantee is claimed. |

## FAQ

<details>
<summary><strong>What is Repogi?</strong></summary>

Repogi (Repo Graph Intelligence) is a local-first, open-source **code intelligence CLI and MCP server**. It builds a source-backed static dependency graph so developers and AI coding agents can discover relationships in TypeScript/TSX repositories.

</details>

<details>
<summary><strong>How is Repogi different from grep or ordinary code search?</strong></summary>

Grep finds text matches. Repogi uses source parsing and supported static binding rules to distinguish resolved code references from unconfirmed same-name matches, while returning source locations and explicit coverage limits. Literal search remains useful alongside it.

</details>

<details>
<summary><strong>Does Repogi use an LLM or send my source code to a cloud service?</strong></summary>

No. Indexing and querying are local and do not require an AI provider, API key, or hosted database. Agents connecting over MCP may have their own data-handling policies, which Repogi does not control.

</details>

<details>
<summary><strong>Can Repogi automatically find every affected test?</strong></summary>

No. It can suggest **static affected-test candidates** and report coverage gaps, but it does not execute tests or guarantee comprehensive test discovery.

</details>

## Development and contributing

The repository is a Rust workspace with `code-intel-*` internal crate names retained for compatibility. The public executable is `repogi`.

```bash
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

| Workspace component | Directory |
| --- | --- |
| Core data types and graph facts | [`crates/core`](crates/core) |
| SQLite persistence | [`crates/store`](crates/store) |
| Parsing, binding, hashing, and indexing | [`crates/index`](crates/index) |
| Retrieval, navigation, impact, and test discovery | [`crates/retrieval`](crates/retrieval) |
| CLI and MCP server | [`crates/cli`](crates/cli) |

Contributions are welcome. Prefer scoped changes, fixture-backed tests, explicit evidence, offline processing, and honest documentation of unknowns. See [`fixtures/`](fixtures) and [`crates/cli/tests/`](crates/cli/tests) for test examples.

### Naming and compatibility

Repogi previously used the working name **Atlas** and the `code-intel` executable. The executable is now `repogi`; old scripts invoking `code-intel` need to update their command. The `.code-intel/` index directory, `code-intel-*` Rust crate names, and `atlas_*` MCP tool identifiers are deliberately unchanged. Historical benchmark and protocol identifiers retain their original provenance.

## License

Repogi is dual-licensed under [MIT](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE), at your option.

Unless explicitly stated otherwise, contributions intentionally submitted to Repogi are dual-licensed under those terms.

---

<div align="center">
  <strong>Repogi · Know the code. Follow the evidence.</strong>
  <p>Open-source repository intelligence for developers and AI coding agents.</p>
  <a href="https://github.com/thinkgrid-labs/repogi/issues">Report an issue</a> · <a href="https://github.com/thinkgrid-labs/repogi">View the repository</a>
</div>
