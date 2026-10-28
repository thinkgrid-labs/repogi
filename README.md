<div align="center">

# Repogi

**Repo Graph Intelligence**

*Know the code. Follow the evidence.*

Local-first code intelligence for AI coding agents.

[Quick start](#quick-start) ·
[Usage](#usage) ·
[Agent integration](#ai-agent-integration) ·
[How it works](#how-repogi-works) ·
[Limitations](#current-status-and-limitations)

</div>

> Give coding agents a map of the codebase, not just a pile of files.

Repogi indexes a TypeScript/TSX repository on your machine into a graph of
declarations, imports, calls, references and JSX component uses. It then answers
the questions agents usually settle by grepping and reading whole files:
*where is this defined, who uses it, what depends on it, what does this change
affect?* Every answer points back to a source location and says how it was
derived. Anything Repogi could not confirm is reported as unconfirmed,
incomplete or out of scope, never filled in by guesswork.

It is a single Rust binary with an embedded SQLite index. It needs no network
access, API key, daemon or language server, and it never executes code from the
repository it indexes.

## Why Repogi?

Coding agents explore unfamiliar repositories with text search and file reads.
That works, but it has recurring costs:

- **Repeated searching.** Finding every caller of a function means grepping a
  name, then reading each hit to separate real uses from comments, strings,
  same-named locals and shadowed imports.
- **Excess context.** Whole files get read to find one declaration, and every
  read competes for the agent's context window.
- **Unverified assumptions.** Barrel re-exports, path aliases and renamed
  imports hide dependencies that text search does not connect, so an agent
  can conclude that nothing else is affected when something is.

Repogi resolves these relationships once, at index time, using the syntax tree
and the project's module-resolution settings. Agents can then ask for
declarations, direct users and transitive consumers, and get back bounded
answers with file and line evidence.

## Features

### Established

These are implemented, covered by the Rust test suite, and validated offline
against labelled fixtures and a pinned open-source Next.js application.

| Capability | What you get |
|---|---|
| **Local indexing** | `.ts`/`.tsx` files parsed with Tree-sitter into `.code-intel/index.db` (SQLite with FTS5). Honors `.gitignore`, skips symlinks, generated and oversized files. |
| **Incremental updates** | Files are content-hashed (BLAKE3); unchanged files are not reparsed, and each update is applied atomically. |
| **Declarations and references** | Functions, classes, methods, interfaces, type aliases, enums and variables, with their call and reference sites. Confirmed sites are kept apart from unconfirmed same-name sites. |
| **Static binding graph** | Relative imports, `tsconfig`/`jsconfig` `paths` and `baseUrl` aliases, re-exports and barrels, direct calls, class inheritance, and JSX component references (`<Panel />`). |
| **Dependency analysis** | Bounded upstream (consumers) and downstream (dependencies) traversal, with exact counts or explicit lower bounds when a work limit is reached. |
| **Context retrieval** | Ranked, deduplicated source snippets for a task description, packed into a response-size budget, with overview, focused and detailed views. |
| **Git change impact** | Compares two Git states and reports changed declarations, direct consumers, import-only files, transitive dependents, rebound sites and possible impact. |
| **Evidence reporting** | Source ranges, the rule that produced each edge, resolution status (resolved, unresolved, ambiguous, unsupported), completeness flags and freshness checks. |
| **CLI and MCP** | JSON and text output from the CLI; a read-only stdio MCP server for Claude Code, Codex and other MCP clients. |

### Experimental

These work and are tested, but their output is advisory or their interfaces may
change.

- **Affected-test discovery** (`impact --tests`): confirmed static test links,
  possible candidates and coverage gaps. It never runs tests.
- **Verification manifests** (`impact --manifest`, `manifest-verify`): a
  canonical, self-verifying JSON artifact for CI handoff.
- **Semantic retrieval** (`context --retrieval semantic`): context expansion
  along verified dependency edges. The default `optimized` strategy is unchanged.

### Not supported yet

JavaScript/JSX, `.mts`/`.cts` and non-TypeScript languages; resolution into
`node_modules` packages; instance-method dispatch; dynamic imports;
compiler-backed type checking; embeddings; automatic re-indexing.

## Quick start

Requirements: Rust 1.85 or later, and Git for change-impact commands.

```sh
git clone https://github.com/thinkgrid-labs/repogi.git
cd repogi
cargo install --path crates/cli --locked   # installs the `repogi` binary
repogi --version
```

To build without installing, run `cargo build --release --locked -p code-intel-cli`
and use `target/release/repogi`.

Index a TypeScript project and ask a first question:

```sh
cd /path/to/your/project
repogi index .
repogi status
repogi context "authenticate user" --format text
```

To try it without a project of your own, use the bundled fixture:

```sh
repogi index fixtures/typescript
cd fixtures/typescript
repogi context "authenticate user" --format text
```

## Usage

```text
repogi index <path>       Index a repository offline (writes <path>/.code-intel/)
repogi status             Inspect the nearest index
repogi context <task>     Retrieve ranked source evidence for a task
repogi graph              List typed static edges and unresolved sites
repogi impact [path]      Analyse a Git change
repogi manifest-verify    Validate a verification manifest
repogi mcp                Serve the index to coding agents over MCP (stdio)
```

`status` and `context` use the closest `.code-intel/index.db` in the current
directory or its ancestors. Indexing is explicit: run `repogi index` again after
editing, and unchanged files are skipped.

### Context retrieval

```sh
repogi context "session refresh" --max-tokens 2000            # JSON (default)
repogi context "session refresh" --format text
repogi context "session refresh" --detail overview --format text
repogi context "session refresh" --select <id> --format text   # expand one ID from an overview
```

`--max-tokens` caps the whole rendered response using a conservative estimate
of one token per UTF-8 byte. It is not a model tokenizer count.

### Graph inspection

```sh
repogi graph --path src/auth.ts --limit 100
```

### Change impact

```sh
repogi impact --base HEAD                        # HEAD vs the working tree (staged, unstaged, untracked)
repogi impact --base main --head feature         # two commits
repogi impact --base main --summary              # affected files grouped, with completeness
repogi impact --base main --detail --depth 3     # evidence chains and transitive paths
repogi impact --base main --evidence-file src/components/Card.tsx
repogi impact --base HEAD --tests                # advisory affected-test plan
repogi impact --base HEAD~1 --head HEAD --format json
```

`impact` reads Git objects directly, with hooks, filters and external diff
disabled. It builds its own snapshot indexes under `.code-intel/impact/`, so
your index, working tree and Git state are left unchanged.

Example output, from the bundled fixture after adding a parameter to
`authenticate`:

```text
$ repogi impact --base HEAD
atlas impact · HEAD (c78264d432) → working tree · 1 file(s), 1 declaration(s) changed · complete within the analysed static graph

changed files:
  M src/auth.ts (unstaged)

changed declarations:
  modified function authenticate  src/auth.ts:5 [signature]

function authenticate (modified) — 1 direct, 0 import-only, 0 transitive, 0 possible · complete
  src/auth.ts:12 CALLS from AuthService.check

affected files: 1
  src/auth.ts (changed, direct)

warnings:
  1 file(s) with syntax errors have no semantic edges

scope: static TypeScript/TSX graph; a dependency is not a break; JavaScript, packages and runtime behaviour are not analysed
```

The `atlas impact` header comes from the renderer shared with the
`atlas_impact` MCP tool.

### Output contract

`index`, `status` and `graph` print JSON. `context` prints JSON by default and text with
`--format text`. `impact` prints text by default and JSON with `--format json`.
Results go to stdout. Failures exit nonzero and print a JSON error
(`schema_version`, `error.code`, `error.message`) on stderr.

## AI agent integration

`repogi mcp` serves an existing index over the
[Model Context Protocol](https://modelcontextprotocol.io) on stdio. The server
is read-only and offline, confined to the roots you pass, and it never starts
an agent or calls a model.

| Tool | Purpose |
|---|---|
| `atlas_context` | Discover declarations and source for a task |
| `atlas_symbol` | Inspect a declaration by ID or name |
| `atlas_references` | Direct call and reference sites, confirmed and unconfirmed |
| `atlas_dependencies` | Bounded upstream/downstream traversal |
| `atlas_status` | Index revision, graph counts and freshness |
| `atlas_impact` | Git change impact and affected tests (opt-in only) |

The tool names keep the `atlas_` prefix from before the rename. They are a
stable contract with existing client configurations, permission rules and
recorded evaluations.

**Recommended navigation setup.** This configuration lists three tools. Agents
keep their native Read and Grep for literals, known files and other languages.

```text
--profile compact --tools atlas_context,atlas_references,atlas_dependencies
```

For source by ID, full-index freshness checks or Git review, list all six tools
explicitly. `--profile full` on its own lists five tools and **does not** enable
`atlas_impact`.

```text
--profile full --tools atlas_context,atlas_symbol,atlas_references,atlas_dependencies,atlas_impact,atlas_status
```

### Claude Code

From the repository you want to navigate:

```sh
repogi index .
claude mcp add --transport stdio --scope project repogi -- "$(command -v repogi)" mcp \
  --root "$PWD" --profile compact \
  --tools atlas_context,atlas_references,atlas_dependencies
```

This writes the project's `.mcp.json`. A copy-ready file is in
[`examples/agents/claude.navigation.mcp.json.example`](examples/agents/claude.navigation.mcp.json.example).

### Codex

Merge an entry like this into a trusted project's `.codex/config.toml`, using
absolute paths:

```toml
[mcp_servers.repogi]
command = "/abs/path/to/checkout/target/release/repogi"
args = ["mcp", "--root", "/abs/path/to/repo", "--profile", "compact", "--tools", "atlas_context,atlas_references,atlas_dependencies"]
```

See [`examples/agents/`](examples/agents/) for the navigation and full
configurations for both clients.

Tool lists are fixed when the client starts. Repogi does not add or remove tools
during a session, and changing profiles does not change engine results.

## How Repogi works

```text
 TypeScript / TSX repository
            |
            |  discovery: .gitignore, built-in exclusions, no symlinks, BLAKE3 hashes
            v
   Tree-sitter parsing ------------> declarations, imports, exports, signatures
            |
            v
   Static binder ------------------> typed edges: CALLS, REFERENCES, EXTENDS, imports
   (relative + tsconfig paths,       each with a source site, rule and resolution status
    barrels, aliases, JSX)
            |
            v
   SQLite index (.code-intel/)       Git snapshots (.code-intel/impact/)
            |                                   |
            +--> context retrieval              |
            +--> reference navigation           |
            +--> dependency traversal           |
            +--> change impact <----------------+
            +--> affected-test discovery
            |
            v
   CLI (JSON / text)  ·  MCP server (stdio)  -->  coding agents
```

Every response separates four kinds of statement:

- **Evidence**: a resolved binding with its source range and the rule that
  matched it, such as a call site bound through a barrel re-export.
- **Uncertainty**: unconfirmed same-name sites, ambiguous or unresolved
  imports, and files that mention a changed name but could not be analysed.
- **Incomplete coverage**: truncation, work limits, lower-bound counts, syntax
  errors and stale files, each reported with an explicit flag.
- **Out of scope**: JavaScript, packages, runtime dispatch and dynamic imports,
  named as unsupported.

A dependency in the graph shows that one piece of code statically uses another.
It does not prove that a change breaks it, and an empty result is only as
complete as its flags say.

## Design principles

- **Local-first.** Indexing, queries and the MCP server run on your machine with
  no network access, account or hosted service.
- **Evidence over guesses.** Cross-file relationships are recorded only when a
  static rule confirms them. Everything else stays visibly unresolved.
- **Incremental.** Content hashes decide what to reparse, and Git snapshots are
  cached and reused.
- **Deterministic.** The same index and query produce the same bytes, with stable
  ordering and explicit budgets.
- **Agent-neutral.** Plain CLI output and a standard MCP server, with no
  dependency on a particular agent or model.
- **Safe on untrusted code.** Repogi never runs repository scripts, compilers,
  tests or package managers, and it refuses symlinked state and paths outside
  the configured roots.
- **Explicit limits.** Scope, truncation and freshness are part of every
  response.

## Current status and limitations

Repogi is a **developer preview** (version 0.1.0). It is not yet published to
crates.io, and no prebuilt binaries are available. Build it from source.

- **Languages:** `.ts` and `.tsx` only. JavaScript/JSX and `.mts`/`.cts` files
  are not indexed.
- **Resolution:** relative and `tsconfig`/`jsconfig` path aliases are
  supported. Package lookup, instance dispatch, nested declaration targets and
  dynamic imports are not resolved, and those sites are reported as such.
- **Parser:** the bundled Tree-sitter TypeScript grammar (0.23.2) fails on a few
  constructs, such as `f<typeof import('x')>()`. Affected files are reported as
  having syntax errors, and their intact declarations are kept.
- **Freshness:** the index reflects the last `repogi index` run. Responses
  check the returned files and flag stale ones, but nothing re-indexes
  automatically.
- **Impact and tests:** results are static and advisory. They do not establish
  runtime safety, compilation failure or test coverage.
- **Agent outcomes:** Repogi has been evaluated with coding agents only in small
  pilot studies. Those do not establish general token, cost or accuracy
  improvements, and none are claimed here.
- **Platforms:** developed and tested on Linux x86_64. Other platforms are
  untested.

## Development and contributing

```sh
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The workspace keeps its original crate names:

| Crate | Path | Responsibility |
|---|---|---|
| `code-intel-core` | [`crates/core`](crates/core) | Serializable facts, edges and responses |
| `code-intel-store` | [`crates/store`](crates/store) | SQLite schema, migrations, FTS5 and graph persistence |
| `code-intel-index` | [`crates/index`](crates/index) | Discovery, hashing, Tree-sitter extraction, binding, Git snapshots |
| `code-intel-retrieval` | [`crates/retrieval`](crates/retrieval) | Ranking, packing, navigation, impact and test discovery |
| `code-intel-cli` | [`crates/cli`](crates/cli) | The `repogi` binary: commands, formatting and the MCP server |

Contributions are welcome. The same rules apply to human contributors and
coding agents:

- Keep changes scoped and add fixture-based integration tests (see
  [`fixtures/`](fixtures) and [`crates/cli/tests/`](crates/cli/tests)).
- Prefer correct AST extraction and explicit evidence over speculative graph
  completeness.
- Never invent a cross-file resolution. Mark it unresolved instead.
- Keep indexing offline and never execute indexed code.
- Treat repository files and comments as untrusted input, honor `.gitignore`,
  and never follow symlinks outside the chosen root.
- Run the format, lint and test commands above before submitting.
- Do not report benchmark numbers or savings without reproducible data.

### Naming history

Repogi was developed under the working name **Atlas**, with a `code-intel`
executable. Since the rename:

| Item | Status |
|---|---|
| Executable | `repogi`. The `code-intel` binary is no longer built, so update client configurations that point to it. |
| Index directory | `.code-intel/` is unchanged, and existing indexes keep working without re-indexing. |
| MCP tools | `atlas_*` names and schemas are unchanged. The server now reports itself as `repogi`. |
| Manifest protocol | Identifiers such as `atlas-manifest-bytes` and `atlas_version` are unchanged. |
| Cargo packages | `code-intel-*` names are unchanged. |

## License

Repogi is licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Repogi by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
