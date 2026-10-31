<div align="center">
  <img src="assets/repogi-banner.svg" alt="Repogi — Repo Graph Intelligence. Know the code. Follow the evidence. A local-first code intelligence tool built in Rust." width="100%" />

# Repogi — Repo Graph Intelligence

**Local, source-backed repository intelligence for developers and AI coding agents.**

Repogi is a Rust CLI and Model Context Protocol (MCP) server. It indexes TypeScript/TSX code and Markdown project knowledge on your machine and answers the questions agents usually work out with rounds of grep and file reads: *where is this declared, what uses it, what does the project require, and what might this change affect?* Each answer cites files and lines and says what Repogi could not confirm.

[Quick start](#quick-start) · [What it can do](#what-repogi-can-do) · [How it works](#how-repogi-works) · [Connect an agent](#connect-a-coding-agent) · [Limitations](#limitations)

</div>

## Why Repogi?

To find who calls a function, a coding agent typically greps for the name, opens each match and decides which hits are real. On a widely used function that takes many tool calls and a lot of file content in context. It also produces false positives (same-name matches) and misses consumers (aliased or re-exported imports).

Repogi does that work once, ahead of time, and keeps the result as a local code graph. An agent can then ask one question and get a bounded answer:

- **Confirmed vs. unconfirmed.** A relationship is either *resolved* through a static binding or listed as *unconfirmed*. Same-name text matches are never promoted to real references.
- **Source locations and provenance.** Confirmed edges carry the file, line and binding rule that established them, so an agent can cite them or read exactly the right lines.
- **Honest completeness.** Responses report what was returned, what was omitted and whether counts are exact. A truncated answer says so.
- **Bounded output.** Answers stay within an explicit size budget instead of returning whole files.

Repogi helps most on questions with many static consumers or several dependency hops. For a single known file, plain reads and grep work just as well, and agents keep using them alongside Repogi.

## Quick start

### Install a prebuilt binary

Release **0.1.0** provides prebuilt binaries for Linux (x86_64, ARM64; glibc 2.34+) and macOS 11+ (Intel, Apple silicon). The installer verifies the download's SHA-256 checksum and installs into `~/.local/bin` without `sudo`:

```sh
curl -fsSL https://github.com/thinkgrid-labs/repogi/releases/latest/download/install.sh | sh
repogi --version
```

> **Release status:** 0.1.0 contains code intelligence only (context, references, dependencies, change impact and test candidates). Project knowledge, `knowledge trace`, `investigate` and project memory are not in a release yet. To use them, build from source. Windows is not supported yet. See [installation](docs/DEVELOPER_PREVIEW_INSTALLATION.md) for verification options and [release assets](docs/RELEASES.md).

### Build from source

You need Rust 1.98 or newer (the version CI tests), Git and a C compiler:

```sh
git clone https://github.com/thinkgrid-labs/repogi.git
cd repogi
cargo install --path crates/cli --locked
repogi --version
```

This installs one `repogi` executable into Cargo's bin directory (usually `~/.cargo/bin`). To build without installing, run `cargo build --release --locked -p code-intel-cli` and use `./target/release/repogi`. Repogi is not published to crates.io.

### Index a repository

```sh
cd /path/to/your/typescript-project
repogi index .
repogi status
repogi context "authentication flow" --format text
```

The index is written inside the project, in `.code-intel/`. Add it to your `.gitignore` so it is never committed:

```sh
echo ".code-intel/" >> .gitignore
```

Indexing is explicit and incremental. Rerun `repogi index .` after edits; unchanged files are skipped by content hash. Queries never re-index on their own, and when indexed files have changed since the last run, responses flag the evidence as stale. Git is needed only for change-impact commands.

## What Repogi can do

| Question | CLI | MCP tool | Release |
| --- | --- | --- | --- |
| Where is the code for this task or name? | `repogi context "session refresh"` | `atlas_context` | 0.1.0 |
| Who uses this declaration, and where? | — | `atlas_references` | 0.1.0 |
| What depends on this, two or three hops out? | — | `atlas_dependencies` | 0.1.0 |
| What might my Git change affect? | `repogi impact --base main` | `atlas_impact` (opt-in) | 0.1.0 |
| Which tests are statically linked to this change? | `repogi impact --base HEAD --tests` | `atlas_impact` with `tests` | 0.1.0 |
| Is the index current? | `repogi status` | `atlas_status` | 0.1.0 |
| What does the project require or record? | `repogi knowledge search "authentication requirements"` | `atlas_context` with `scope: "docs"` | source |
| What explicitly links a requirement, decision and code? | `repogi knowledge trace REQ-AUTH-001` | `atlas_knowledge_trace` (opt-in) | source |
| Where should I start investigating this task? | `repogi investigate "Modify session expiration"` | `atlas_investigate` (opt-in) | source |
| What did we already decide, and is it still supported? | `repogi memory search "session storage"` | `atlas_memory` (opt-in, read-only) | source |

*Release* is the first release that includes the capability; *source* means build from source for now.

### Navigate code

A reference answer looks like this (abridged):

```text
atlas references r1 · refreshSession · function:src/auth.ts:7:63 · L1 · 3 resolved, 0 unconfirmed edge(s) · complete · 807/4000 bytes
evidence: indexed snapshot; source=matched_at_read (1 checked, 0 unchecked); graph freshness unverified; scope .ts/.tsx only
available: 3 resolved, 0 unconfirmed; returned: 3 + 0; omitted: 0 (exact); unconfirmed included: true

resolved:
src/auth.ts:2 CALLS+REFERENCES from login | function login(u: string) { return refreshSession(u); }
…
```

### Search and trace project knowledge

Indexing also reads `.md` and `.markdown` files, including requirements, ADRs and specifications. Search returns query-centered excerpts with original paths, line ranges and freshness. Trace follows explicit requirement IDs, ADR metadata and Markdown links:

```sh
repogi knowledge search "authentication requirements" --limit 10
repogi context "Implement session expiration" --scope code,docs --format text
repogi knowledge trace REQ-AUTH-001 --depth 2
```

Documents record intent. A keyword match does not prove an implementation, and a validated documentation link does not prove that code behaves as described. See [project knowledge](docs/KNOWLEDGE.md) and [knowledge relationships](docs/KNOWLEDGE_RELATIONSHIPS.md).

### Investigate a task

```sh
repogi investigate "Modify session expiration"
repogi investigate "What implements REQ-AUTH-001?" --format json --max-steps 4
```

Agent investigation returns an ordered, advisory plan: candidate implementation locations, governing requirements and decisions, confirmed static consumers, statically linked test candidates and unresolved findings. Each step cites indexed evidence, states its uncertainty and names a bounded next read. An investigation is a read-only proposal: Repogi does not change code, run tests or store the plan. See [agent investigation](docs/AGENT_INVESTIGATION.md).

### Keep reviewed project memory

Persistent project memory records decisions a developer has reviewed, so later sessions can find them along with their evidence:

```sh
repogi memory propose --type architecture_decision --title "Sessions persist in SQLite" \
  --claim "Session records are stored in an embedded SQLite database file." --evidence ADR-0001
repogi memory show mem-0001                                # review the claim, evidence and digest
repogi memory approve mem-0001 --digest <digest> --reviewer alice
repogi memory search "SQLite"                              # approved memories, with live evidence status
repogi investigate "Why does this project use SQLite?" --include-memory
repogi memory export --output "$HOME/project-memory-backup.json"
```

Nothing is memorized automatically. A memory exists only after an explicit `propose`, and it becomes authoritative only when someone approves its exact revision digest. Each read re-checks the cited evidence against the current index and source files and flags `stale` or `missing` evidence without rewriting the claim. Memories live in `.repogi/memory.db`, separate from the index, so deleting or rebuilding `.code-intel/` does not remove them. See [project memory](docs/PROJECT_MEMORY.md) and [memory storage, backup and recovery](docs/MEMORY_STORAGE.md).

## How Repogi works

`repogi index` builds a local repository intelligence index. It parses TypeScript/TSX with Tree-sitter and resolves imports, path aliases, re-exports and JSX component use into a static code graph. It splits Markdown into searchable sections and extracts explicit requirement IDs, ADR metadata and links as knowledge relationships. All of this is stored in one rebuildable SQLite database, `.code-intel/index.db`. Retrieval answers questions from that index with source-backed, bounded results.

Reviewed project memory is stored separately, in `.repogi/memory.db`. Memories enter it only through explicit CLI review, keep their evidence snapshots and review history, and survive re-indexing. Their evidence is re-verified against the index, and investigations recall approved memories only when asked. Developers and coding agents use all of this through the CLI and read-only MCP tools.

<div align="center">
  <img src="assets/repogi-flow.svg" alt="Repogi architecture. Source code and project knowledge are indexed into the rebuildable Repogi index at .code-intel/index.db: static code graph, document search and knowledge relationships. Retrieval provides context, trace, investigation and change impact through the CLI and read-only MCP tools. Project memory is a separate durable store at .repogi/memory.db, written only through explicit review with the CLI, re-verified against the index and optionally recalled by retrieval." width="100%" />
</div>

<details>
<summary>Text-only architecture diagram</summary>

```text
Source code (TypeScript, TSX)    Project knowledge (Markdown, PRDs, ADRs)
               \                     /
                v                   v
      Repogi index — .code-intel/index.db — REBUILDABLE
      static code graph · document search · knowledge relationships
                         |
                         v
      Retrieval — context · trace · investigate · change impact
      source-backed, bounded results  <---- optional recall ----+
                         |                                      |
                         v                                      |
      CLI + MCP — developers and coding agents                  |
      (MCP tools are read-only)                                 |
                         |                                      |
                         | explicit review (CLI only)           |
                         v                                      |
      Project memory — .repogi/memory.db — DURABLE -------------+
      reviewed claims · evidence snapshots · review history
      survives re-indexing; evidence re-verified against the index
```

</details>

Discovery respects `.gitignore`. It always skips `node_modules`, `.git`, `.code-intel`, `.repogi`, `dist`, `build`, `coverage`, `.next`, `target` and `generated` directories, files over 1 MiB, and generated or minified sources. Details are in [architecture](docs/ARCHITECTURE.md), [semantic scope](docs/SEMANTICS.md) and [module resolution](docs/MODULE_RESOLUTION.md).

## Connect a coding agent

`repogi mcp` runs a local MCP server over stdio. It does not start an agent and makes no model or network calls. The recommended navigation setup lists three tools: `atlas_context`, `atlas_references` and `atlas_dependencies`.

### Claude Code

Run this in the repository you want the agent to inspect:

```sh
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
| `atlas_context` | Find declarations relevant to a task or name. Source builds also accept `scope` for document sections. |
| `atlas_symbol` | Inspect one declaration's source and signature. |
| `atlas_references` | Direct static users of a declaration, with use sites. |
| `atlas_dependencies` | Upstream consumers or downstream dependencies, by depth. |
| `atlas_status` | Index state and full freshness check. |
| `atlas_impact` | Git change impact and advisory test candidates. Opt-in via `--tools`. |
| `atlas_knowledge_trace` | Explicit knowledge relationships and source citations. Opt-in; source builds only. |
| `atlas_investigate` | Evidence-backed investigation plan for a task. Opt-in; source builds only. |
| `atlas_memory` | Search, get, list and history of approved project memories. Opt-in; source builds only. |

The tools never create, approve or change memories, and never re-index. The opt-in `atlas_impact` tool writes only its rebuildable snapshot cache under `.code-intel/impact/`. Tool IDs keep the project's earlier name, Atlas, for compatibility with existing MCP clients.

### Multiple repositories

One `repogi` executable serves any number of projects. Each repository has its own `.code-intel/` index (and `.repogi/` memory, if used), and there is no shared global graph. An MCP server answers only for the roots it was started with: `--root` names them, and the default is the current directory. A request for any other root is refused. The simplest setup is one server per project. With several `--root` flags, each tool call must name its `root`.

## CLI reference

| Command | Purpose |
| --- | --- |
| `repogi index <path>` | Create or update the local index (JSON run report). |
| `repogi status` | Inspect the nearest index (JSON). |
| `repogi context <task>` | Ranked, budgeted source evidence (`--format text` or JSON; `--scope code,docs`). |
| `repogi graph [--path <file>]` | Typed static edges and unresolved sites (JSON). |
| `repogi impact [path]` | Static impact of a Git change (text or JSON; `--tests` for test candidates). |
| `repogi knowledge search <query>` | Ranked Markdown section evidence. |
| `repogi knowledge trace <identifier>` | Bounded explicit requirement/ADR relationships and evidence. |
| `repogi investigate <task>` | Advisory, evidence-backed investigation plan (`--include-memory` adds approved memories). |
| `repogi memory <command>` | Reads: `list`, `show`, `history`, `search`, `verify`, `conflicts`, `status`, `export`. Explicit writes: `propose`, `correct`, `approve`, `reject`, `supersede`, `retire`, `migrate`, `import`, `rebind`. |
| `repogi manifest-verify <file>` | Validate a verification manifest. |
| `repogi mcp` | Serve MCP tools over stdio. |

```sh
repogi context "session refresh" --max-tokens 2000     # the budget is a UTF-8 byte estimate, not a tokenizer count
repogi graph --path src/auth.ts --limit 100
repogi impact --base HEAD                              # HEAD vs. the working tree
repogi impact --base main --head feature --detail --depth 3
repogi impact --base HEAD~1 --head HEAD --format json
```

Most commands only read. `repogi index` writes `.code-intel/`, impact analysis writes its snapshot cache under `.code-intel/impact/`, and the memory write commands write `.repogi/`. **Experimental:** verification manifests (`impact --manifest`, `manifest-verify`) and `context --retrieval semantic` are evolving interfaces.

## Privacy and safety

- **Local and offline.** Indexing and queries run on your machine. Repogi needs no hosted service, account, API key or model, and makes no network calls.
- **Indexing never executes repository code.** It reads and parses files only; package scripts and lifecycle hooks never run. Symlinks and paths outside the repository are not followed.
- **Explicit writes.** Reads and MCP tools do not change the index or memory. `repogi index` writes `.code-intel/`; if a repository still holds memories from a source build that kept them in the index, it also moves them once into `.repogi/`. Memory changes happen only through `repogi memory` write commands.
- **Memory is local data.** `.repogi/` ignores itself in Git and is owner-only on Unix. Memory exports can contain sensitive project information and reviewer labels, so store and share them like source code.
- **Your agent is a separate system.** Repogi keeps everything local, but the snippets, paths and memories it returns go into your coding agent's context. If that agent uses a hosted model, they are sent to its provider under that agent's settings.
- **Returned source is untrusted text.** Snippets come from the repository as written. Treat them as data, not instructions.

## Limitations

| Area | Current scope |
| --- | --- |
| **Languages** | TypeScript `.ts` and TSX `.tsx`. JavaScript, JSX, `.mts`, `.cts` and other languages are not indexed. A query with no matches says that other languages were not searched. |
| **Resolution** | Relative imports, `tsconfig`/`jsconfig` paths and `baseUrl`, re-exports and barrel files. Package imports, instance dispatch and dynamic imports are not fully resolved. |
| **Parser** | Tree-sitter TypeScript grammar 0.23.2 has known syntax gaps ([details](docs/TYPESCRIPT_GRAMMAR_LIMITATIONS.md)). |
| **Project knowledge** | Markdown only, with local lexical section search. Documents record intent and do not prove code behavior. |
| **Project memory** | Explicitly proposed and reviewed claims only. Verification checks cited bytes and identities, not truth. Search is lexical. Reviewer names are self-reported labels, not authenticated identities, and export digests are integrity checks, not signatures. Back up with `repogi memory export`. |
| **Freshness** | Manual. Rerun `repogi index` after edits. Changed files are flagged as stale. |
| **Static analysis** | A graph dependency is not proof that a change breaks anything. Static test links do not show that a test runs or covers the code. Runtime and HTTP consumers are not analyzed. |
| **Investigation** | Advisory plans from lexical discovery, one explicit-claim hop and bounded static walks. Not an autonomous agent: nothing is executed or verified, and plans are not stored. |
| **Agent results** | Benefits depend on the task and on how the agent uses its tools. Repogi does not guarantee token, time or cost savings, or more accurate answers, on any given repository. |
| **Platforms** | Prebuilt binaries for Linux (glibc 2.34+) and macOS 11+. No Windows support yet. Durable memory storage has so far been validated on Linux only. |

More: [navigation limits](docs/REFERENCE_NAVIGATION_LIMITS.md), [change impact](docs/CHANGE_IMPACT.md), [affected-test discovery](docs/AFFECTED_TEST_DISCOVERY.md), [supported languages](docs/SUPPORTED_LANGUAGES.md).

## FAQ

<details>
<summary><strong>How is Repogi different from grep or my agent's built-in search?</strong></summary>

Grep finds text. Repogi resolves declarations, so it separates real references from same-name matches and follows aliased and re-exported imports. It returns locations with the rule that confirmed each one, and it says when an answer is incomplete. Literal search is still the right tool for strings, config files and unsupported languages.

</details>

<details>
<summary><strong>Will it reduce my agent's token use?</strong></summary>

It is designed to, mainly on questions with many consumers or several hops, where an agent would otherwise open many files. On small lookups it can cost about the same as plain search, because tool definitions add context of their own. We don't publish general savings figures.

</details>

<details>
<summary><strong>Does Repogi send my code anywhere?</strong></summary>

No. Repogi makes no network calls. Content it returns to a connected agent is handled by that agent and its model provider.

</details>

<details>
<summary><strong>What happens to project memory if I delete <code>.code-intel/</code>?</strong></summary>

Nothing. Memories live in `.repogi/memory.db`. Without an index they still read, with evidence reported as `unverified`. After `repogi index .`, their evidence is re-verified against the new index.

</details>

## Documentation and contributing

Start with the [documentation index](docs/README.md). Development checks:

```sh
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked -- --test-threads=1
```

| Workspace component | Directory |
| --- | --- |
| Core data types, graph facts, knowledge and memory types | [`crates/core`](crates/core) |
| SQLite persistence: index and durable memory stores | [`crates/store`](crates/store) |
| Parsing, binding, hashing and indexing | [`crates/index`](crates/index) |
| Retrieval, navigation, impact, knowledge, investigation and memory | [`crates/retrieval`](crates/retrieval) |
| CLI and MCP server | [`crates/cli`](crates/cli) |

Contributions are welcome. Prefer scoped changes, fixture-backed tests (see [`fixtures/`](fixtures) and [`crates/cli/tests/`](crates/cli/tests)), offline processing and honest documentation of unknowns.

**Naming:** Repogi was developed as *Atlas* with a `code-intel` executable. The executable is now `repogi`. The `.code-intel/` directory, `code-intel-*` crate names, `atlas_*` tool IDs and some tool titles and messages keep the earlier names for compatibility.

## License

Repogi is licensed under the [MIT License](LICENSE). Copyright (c) 2026 ThinkGrid Labs. Dependencies and third-party source fixtures keep their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md). Unless explicitly stated otherwise, contributions intentionally submitted to Repogi are licensed under MIT.

---

<div align="center">
  <strong>Repogi · Know the code. Follow the evidence.</strong>
  <p>Open-source repository intelligence for developers and AI coding agents.</p>
  <a href="https://github.com/thinkgrid-labs/repogi/issues">Report an issue</a> · <a href="https://github.com/thinkgrid-labs/repogi">View the repository</a>
</div>
