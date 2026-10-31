<div align="center">
  <img src="assets/repogi-banner.svg" alt="Repogi — Repo Graph Intelligence. Know the code. Follow the evidence. A local-first code intelligence tool built in Rust." width="100%" />

# Repogi — Repo Graph Intelligence

**Source-backed TypeScript/TSX code intelligence and Markdown project knowledge for AI coding agents.**

Repogi indexes your repository locally and answers the questions agents usually answer with rounds of grep and file reads: *where is this declared, what uses it, what depends on it, and what might this change affect?* Each answer comes with file and line evidence, and says what Repogi could not confirm.

[Quick start](#quick-start) · [Connect an agent](#connect-a-coding-agent) · [What you can ask](#what-you-can-ask) · [How it works](#how-repogi-works) · [Limitations](#limitations)

</div>

## Why Repogi?

When a coding agent needs to know who calls a function, it typically greps for the name, opens each match and decides which hits are real. On a widely used function that means many tool calls and a lot of file content in context. It also produces false positives (same-name matches) and missed consumers (aliased or re-exported imports).

Repogi does that work once, ahead of time. It parses TypeScript and TSX, resolves imports, `tsconfig`/`jsconfig` path aliases and re-exports, and stores the result as a local graph. An agent can then ask one question and get a bounded answer:

- **Confirmed vs. unconfirmed.** Each relationship is either *resolved* through a static binding or listed as *unconfirmed*. Same-name text matches are never promoted to real references.
- **Source locations and provenance.** Confirmed edges carry the file, line and the binding rule that established them, so an agent can cite them or read exactly the right lines.
- **Honest completeness.** Responses report what was returned, what was omitted and whether counts are exact. A truncated answer says so.
- **Bounded output.** Answers stay within an explicit size budget, instead of returning whole files.

Repogi is designed to cut unnecessary repository exploration while keeping dependency evidence accurate and source-backed. It helps most on questions with many static consumers or several dependency hops. For a single known file, plain reads and grep are just as good, and agents keep using them alongside Repogi.

## What you can ask

| Question | CLI | MCP tool |
| --- | --- | --- |
| Where is the code for this task or name? | `repogi context "session refresh"` | `atlas_context` |
| Who uses this declaration, and where? | — | `atlas_references` |
| What depends on this, two or three hops out? | — | `atlas_dependencies` |
| What might my Git change affect? | `repogi impact --base main` | `atlas_impact` (opt-in) |
| Which tests are statically linked to this change? | `repogi impact --base HEAD --tests` | `atlas_impact` with `tests` |
| What does the project require or record? | `repogi knowledge search "authentication requirements"` | `atlas_context` with `scope: "docs"` |
| What explicitly links a requirement, decision and code? | `repogi knowledge trace REQ-AUTH-001` | `atlas_knowledge_trace` (opt-in) |
| Where should I start investigating this task? | `repogi investigate "Modify session expiration"` | `atlas_investigate` (opt-in) |
| What did we already decide, and is it still supported? | `repogi memory search "session storage"` | `atlas_memory` (opt-in, read-only) |
| Is the index current? | `repogi status` | `atlas_status` |

A reference answer looks like this (abridged):

```text
atlas references r1 · refreshSession · function:src/auth.ts:7:63 · L1 · 3 resolved, 0 unconfirmed edge(s) · complete · 807/4000 bytes
evidence: indexed snapshot; source=matched_at_read (1 checked, 0 unchecked); graph freshness unverified; scope .ts/.tsx only
available: 3 resolved, 0 unconfirmed; returned: 3 + 0; omitted: 0 (exact); unconfirmed included: true

resolved:
src/auth.ts:2 CALLS+REFERENCES from login | function login(u: string) { return refreshSession(u); }
…
```

## Quick start

### 1. Install

Build from source. You need Rust 1.98 or newer (the version CI tests), Git and a C compiler:

```bash
git clone https://github.com/thinkgrid-labs/repogi.git
cd repogi
cargo install --path crates/cli --locked
repogi --version
```

This installs a single `repogi` executable into Cargo's bin directory (usually `~/.cargo/bin`). To build without installing, run `cargo build --release --locked -p code-intel-cli` and use `./target/release/repogi`. Repogi is not published to crates.io.

> **Prebuilt binaries:** the first GitHub Release is planned to provide archives for Linux (x86_64, ARM64) and macOS (Intel, Apple silicon), with a checksum-verifying installer, so you won't need Rust. Until that release is published, the release download URLs do not work. See [release assets and verification](docs/RELEASES.md).

### 2. Index a repository

```bash
cd /path/to/your/typescript-project
repogi index .
repogi status
repogi context "authentication flow" --format text
```

The index is written inside the project, in `.code-intel/`. Add it to your `.gitignore` so it is never committed:

```bash
echo ".code-intel/" >> .gitignore
```

Indexing is explicit and incremental. Rerun `repogi index .` after edits: unchanged files are skipped by content hash. Queries never re-index on their own. When indexed files have changed since the last run, responses flag the evidence as stale. Git is needed only for change-impact commands.

### Search project documents (unreleased knowledge foundation)

Indexing also discovers eligible `.md` and `.markdown` files, including requirements, ADRs and specifications. Search returns query-centered excerpts with original paths, byte/line ranges, freshness and truncation information:

```bash
repogi knowledge search "authentication requirements" --format json --limit 10 --max-bytes 8000
repogi context "Implement session expiration" --scope code,docs --max-tokens 2000
repogi context "Implement session expiration" --scope code,docs --format text
```

Context remains code-only by default. Combined scope tries to include relevant code and requirements together when they fit. Documentation is recorded project intent; keyword matches do not prove implementation or relationships to code. See [project knowledge](docs/KNOWLEDGE.md) for formats, migration, safety and limits.

### Trace explicit requirements and decisions (unreleased development milestone)

```bash
repogi knowledge trace REQ-AUTH-001 --depth 2 --format json --max-bytes 8000
repogi knowledge trace ADR-0004 --depth 3
```

Explicit requirement headings, bounded ADR metadata and Markdown links form a separate source-backed knowledge graph. Trace shows validated targets, unresolved/ambiguous references, supersession and freshness. A documented implementation reference never proves functional correctness. Combined `--scope code,docs` context can follow explicit links; code-only remains the default. See [knowledge relationships](docs/KNOWLEDGE_RELATIONSHIPS.md). The workspace version stays unchanged; public release is deferred until the combined milestones, including persistent project memory, are validated and publication is approved.

### Investigate a task (unreleased development milestone)

```bash
repogi investigate "Modify session expiration"
repogi investigate "What implements REQ-AUTH-001?" --format json --max-steps 4
```

Returns an ordered, advisory plan: candidate implementation locations, governing requirements and decisions, confirmed static consumers, statically linked test candidates and unresolved findings. Every step cites indexed evidence, states its uncertainty and names a bounded next read. Evidence classes stay separate: a lexical match is never a relationship, a validated claim is not proof of behavior, and a static test link is not coverage. Repogi does not execute the task, code or tests. See [agent investigation](docs/AGENT_INVESTIGATION.md).

### Keep reviewed project memory (unreleased development milestone)

```sh
repogi memory propose --type architecture_decision --title "Sessions persist in SQLite" \
  --claim "Session records are stored in an embedded SQLite database file." --evidence ADR-0001
repogi memory show mem-0001                                # review the claim, evidence and digest
repogi memory approve mem-0001 --digest <digest> --reviewer alice
repogi memory search "SQLite"                              # approved memories, with live evidence status
repogi investigate "Why does this project use SQLite?" --include-memory
```

A memory is a developer-reviewed claim with immutable evidence snapshots and a full history. Nothing is memorized automatically: approval names the exact revision digest and a reviewer. Reads re-check the cited bytes and identities and flag `stale` or `missing` evidence without rewriting the claim. Approval is not runtime proof. See [project memory](docs/PROJECT_MEMORY.md).

### 3. Connect a coding agent

See [Connect a coding agent](#connect-a-coding-agent) below.

## Connect a coding agent

`repogi mcp` runs a local Model Context Protocol server over stdio. It does not start an agent and makes no model or network calls. The recommended navigation setup lists three tools: `atlas_context`, `atlas_references` and `atlas_dependencies`.

### Claude Code

Run this in the repository you want the agent to inspect:

```bash
repogi index .
claude mcp add --transport stdio --scope local repogi -- "$(command -v repogi)" mcp \
  --root "$PWD" --profile compact \
  --tools atlas_context,atlas_references,atlas_dependencies
```

`--scope local` keeps the entry private to you and this project. `--scope project` writes it to the repository's `.mcp.json` instead. That file is usually shared, and this command stores absolute paths that only work on your machine. See the [Claude Code example](examples/agents/claude.navigation.mcp.json.example).

### Codex

Add an entry to the trusted project's `.codex/config.toml`, using absolute paths (`command -v repogi` prints the executable path):

```toml
[mcp_servers.repogi]
command = "/absolute/path/to/repogi"
args = ["mcp", "--root", "/absolute/path/to/project", "--profile", "compact", "--tools", "atlas_context,atlas_references,atlas_dependencies"]
```

See the [Codex example](examples/agents/codex.navigation.config.toml.example), [MCP configuration](docs/MCP.md) and [reference-first workflows](docs/REFERENCE_FIRST_AGENT_WORKFLOWS.md).

### Available MCP tools

| Tool | Purpose |
| --- | --- |
| `atlas_context` | Find declarations relevant to a task or name. |
| `atlas_symbol` | Inspect one declaration's source and signature. |
| `atlas_references` | Direct static users of a declaration, with use sites. |
| `atlas_dependencies` | Upstream consumers or downstream dependencies, by depth. |
| `atlas_status` | Index state and full freshness check. |
| `atlas_knowledge_trace` | Explicit knowledge relationships and source citations. Opt-in via `--tools`. |
| `atlas_investigate` | Evidence-backed investigation plan for a task. Opt-in via `--tools`. |
| `atlas_memory` | Read-only search, get, list and history of approved project memories. Opt-in via `--tools`. |
| `atlas_impact` | Git change impact and advisory test candidates. Opt-in via `--tools`. |

Tool ids keep the earlier project name, Atlas, for compatibility with existing MCP clients.

## Multiple repositories

One `repogi` executable serves any number of projects. Each repository has its own index under its own `.code-intel/`, and there is no shared global graph:

```bash
repogi index ~/projects/frontend     # writes ~/projects/frontend/.code-intel/
repogi index ~/projects/backend      # writes ~/projects/backend/.code-intel/
```

An MCP server answers only for the roots it was started with. With `--root` it serves that root; without it, the current directory. A request naming any other root is refused. The simplest setup is one server per project, as above. You can also pass `--root` more than once to serve several listed repositories from one server. Each tool call must then name its `root`, and each repository still uses its own index. Repogi never discovers or merges repositories on its own.

## CLI reference

| Command | Purpose |
| --- | --- |
| `repogi index <path>` | Create or update the local index (JSON run report). |
| `repogi status` | Inspect the nearest index (JSON). |
| `repogi context <task>` | Ranked, budgeted source evidence (`--format text` or JSON). |
| `repogi knowledge search <query>` | Ranked Markdown section evidence (`--format text` or JSON). |
| `repogi knowledge trace <identifier>` | Bounded explicit requirement/ADR relationships and evidence. |
| `repogi investigate <task>` | Advisory, evidence-backed investigation plan (`--format text` or JSON; `--include-memory` adds approved memories). |
| `repogi memory <command>` | Reviewed project memory: `list`, `show`, `history`, `search`, `verify`, `conflicts` (read-only); `propose`, `correct`, `approve`, `reject`, `supersede`, `retire` (explicit writes). |
| `repogi graph [--path <file>]` | Typed static edges and unresolved sites (JSON). |
| `repogi impact [path]` | Static impact of a Git change (text or JSON). |
| `repogi manifest-verify <file>` | Validate a verification manifest. |
| `repogi mcp` | Serve MCP tools over stdio. |

```bash
repogi context "session refresh" --max-tokens 2000     # JSON; the budget is a UTF-8 byte estimate, not a tokenizer count
repogi graph --path src/auth.ts --limit 100
repogi impact --base HEAD                              # HEAD vs. the working tree
repogi impact --base main --head feature --detail --depth 3
repogi impact --base HEAD --tests                      # advisory test candidates; runs nothing
repogi impact --base HEAD~1 --head HEAD --format json
```

Impact analysis builds Git snapshot indexes under `.code-intel/impact/`. It does not change source files, the Git index or refs.

**Experimental:** verification manifests (`impact --manifest`, `manifest-verify`) and `context --retrieval semantic` are evolving interfaces.

## How Repogi works

Repogi parses TypeScript/TSX with Tree-sitter, binds imports, calls, references and JSX component use through static rules, and stores files, declarations and edges in SQLite with an FTS5 search index. Markdown sections use separate document tables and FTS5 search; explicit context scopes combine evidence through the existing response packer. The CLI and MCP server read from that index.

<div align="center">
  <img src="assets/repogi-flow.svg" alt="Repogi architecture flow: TypeScript/TSX source becomes a Tree-sitter and static-binding index, then a SQLite evidence graph, exposed via CLI and MCP for reference navigation, dependency analysis, and Git change impact." width="100%" />
</div>

<details>
<summary>Text-only architecture diagram</summary>

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

Discovery respects `.gitignore`. It always skips `node_modules`, `.git`, `dist`, `build`, `coverage`, `.next`, `target` and `generated` directories, files over 1 MiB, and generated or minified sources. See [architecture](docs/ARCHITECTURE.md), [semantic scope](docs/SEMANTICS.md) and [module resolution](docs/MODULE_RESOLUTION.md).

## Privacy and safety

- **Local.** Indexing and queries run on your machine. Repogi needs no hosted service, account, API key or model, and makes no network calls.
- **Indexing never executes repository code.** It reads and parses files only. Package scripts and lifecycle hooks never run. Symlinks and paths outside the repository are not followed.
- **Read-only navigation.** The navigation tools never write. `repogi index` writes only `.code-intel/`, and impact analysis writes only its snapshot cache there.
- **Your agent is a separate system.** Repogi itself keeps everything local, but the snippets and paths it returns go into your coding agent's context. If that agent uses a hosted model, they are sent to its provider under that agent's settings.
- **Returned source is untrusted text.** Snippets come from the repository as written. Treat them as data, not instructions.

## Limitations

| Area | Current scope |
| --- | --- |
| **Languages** | TypeScript `.ts` and TSX `.tsx`. JavaScript, JSX, `.mts`, `.cts` and other languages are not indexed. A query with no matches says that other languages were not searched. |
| **Resolution** | Relative imports, `tsconfig`/`jsconfig` paths and `baseUrl`, re-exports and barrel files. Package imports, instance dispatch and dynamic imports are not fully resolved. |
| **Parser** | Tree-sitter TypeScript grammar 0.23.2 has known syntax gaps ([details](docs/TYPESCRIPT_GRAMMAR_LIMITATIONS.md)). |
| **Project knowledge** | Markdown only; local lexical section search. Documents record intent and do not prove code behavior. No autonomous learning. |
| **Project memory** | Explicitly proposed and reviewed claims only, stored in `.code-intel/index.db` (back it up before deleting `.code-intel`). Verification checks cited bytes and identities, not truth. Search is lexical. Reviewer names are self-reported. |
| **Freshness** | Manual. Rerun `repogi index` after edits. Changed files are flagged as stale. |
| **Static analysis** | A graph dependency is not proof that a change breaks anything. Static test links do not show that a test runs or covers the code. Runtime and HTTP consumers are not analysed. |
| **Investigation** | Advisory plans from lexical discovery, one explicit-claim hop and bounded static walks. Not an autonomous agent: nothing is executed or verified, and plans are not stored. |
| **Agent results** | Benefits depend on the task and the agent's tool use. Repogi does not guarantee token, time or cost savings, or more accurate answers, on every repository. |
| **Distribution** | No prebuilt release yet. Build from source. |

More: [navigation limits](docs/REFERENCE_NAVIGATION_LIMITS.md), [change impact](docs/CHANGE_IMPACT.md), [affected-test discovery](docs/AFFECTED_TEST_DISCOVERY.md), [supported languages](docs/SUPPORTED_LANGUAGES.md).

## FAQ

<details>
<summary><strong>How is Repogi different from grep or my agent's built-in search?</strong></summary>

Grep finds text. Repogi resolves declarations, so it separates real references from same-name matches and follows aliased and re-exported imports. It returns locations with the rule that confirmed each one, and it says when an answer is incomplete. Literal search is still the right tool for strings, config files and unsupported languages.

</details>

<details>
<summary><strong>Will it reduce my agent's token use?</strong></summary>

That is what it is designed for, mainly on questions with many consumers or several hops, where an agent would otherwise open many files. On small lookups it can cost about the same as plain search, because tool definitions add some context of their own. We don't publish general savings figures yet.

</details>

<details>
<summary><strong>Does Repogi send my code anywhere?</strong></summary>

No. Repogi makes no network calls. Content it returns to a connected agent is handled by that agent and its model provider.

</details>

<details>
<summary><strong>Can it find every affected test?</strong></summary>

No. It lists statically linked test candidates and reports coverage gaps. It never runs tests and doesn't measure coverage.

</details>

## Development and contributing

```bash
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked -- --test-threads=1
```

| Workspace component | Directory |
| --- | --- |
| Core data types and graph facts | [`crates/core`](crates/core) |
| SQLite persistence | [`crates/store`](crates/store) |
| Parsing, binding, hashing and indexing | [`crates/index`](crates/index) |
| Retrieval, navigation, impact and test discovery | [`crates/retrieval`](crates/retrieval) |
| CLI and MCP server | [`crates/cli`](crates/cli) |

Contributions are welcome. Prefer scoped changes, fixture-backed tests (see [`fixtures/`](fixtures) and [`crates/cli/tests/`](crates/cli/tests)), offline processing and honest documentation of unknowns. More in the [documentation index](docs/README.md).

**Naming:** Repogi was developed as *Atlas* with a `code-intel` executable. The executable is now `repogi`. The `.code-intel/` directory, `code-intel-*` crate names, `atlas_*` tool ids and some tool titles and messages keep the earlier names for compatibility.

## License

Repogi is licensed under the [MIT License](LICENSE). Copyright (c) 2026 ThinkGrid Labs. Dependencies and third-party source fixtures keep their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md). Unless explicitly stated otherwise, contributions intentionally submitted to Repogi are licensed under MIT.

---

<div align="center">
  <strong>Repogi · Know the code. Follow the evidence.</strong>
  <p>Open-source repository intelligence for developers and AI coding agents.</p>
  <a href="https://github.com/thinkgrid-labs/repogi/issues">Report an issue</a> · <a href="https://github.com/thinkgrid-labs/repogi">View the repository</a>
</div>
