# Repogi Roadmap

This roadmap describes where Repogi is today and where it is heading next. It also explains how the maintainers decide what comes first. It lists directions and acceptance criteria, not dates. Plans change as evaluation results come in; anything below **Now** is a proposal and may be reordered or dropped.

*Last reviewed: 2026-10-31.*

**Status labels**

| Label | Meaning |
| --- | --- |
| **Released** | In a published GitHub Release with prebuilt binaries. |
| **On `main`** | Merged and tested in CI on Linux, but not yet in a published release. Build from source to use it. |
| **In progress** | Actively being worked on. |
| **Planned** | Agreed direction with a defined acceptance criterion. Not started. |
| **Exploring** | Needs a design or evaluation before it is committed to. |
| **Research** | Interesting, but only pursued if its value can be demonstrated. |

**Priorities:** **P0** blocks the next release or every later step. **P1** is the next major theme. **P2** is valuable but can wait. Effort (S/M/L) is a rough relative guess, not an estimate.

## Vision

**Repogi helps AI coding agents understand repository code, project requirements, architectural decisions and reviewed project knowledge through bounded, source-backed evidence.**

Repogi is a local repository intelligence engine, not a coding agent. It answers questions about a repository. It does not write code, run tests or decide what is true. The properties below are part of the design, and every roadmap item must keep them:

- **Local-first and offline.** One Rust executable. No account, hosted service, telemetry or required model.
- **Source-backed and bounded.** Every answer cites files and lines, stays within an explicit size budget and reports what it omitted.
- **Honest about uncertainty.** Static evidence, documented intent and functional correctness stay separate. A name match is never reported as a confirmed reference, and unresolved or dynamic relationships stay unconfirmed.
- **Deterministic.** The same index and query give the same answer.
- **Explicit writes.** Indexing is explicit. Project memory changes only through deliberate, reviewed CLI commands, and MCP access is read-only.
- **Repository isolation.** Each repository has its own index and memory. Nothing is combined implicitly.
- **Safe on untrusted repositories.** Indexing reads and parses files. It never executes project code, scripts or configuration, and never follows links outside the repository.

## Current capabilities

Verified against the source on `main`.

| Capability | Interfaces | Status |
| --- | --- | --- |
| TypeScript/TSX code graph: declarations, imports, `tsconfig`/`jsconfig` paths, re-exports, calls, references, JSX component use | CLI `context`, `graph`, `status`; MCP `atlas_context`, `atlas_symbol`, `atlas_references`, `atlas_dependencies`, `atlas_status` | **Released** (0.1.0) |
| Git change impact and statically linked test candidates (`*.test`/`*.spec` files, `__tests__/`, configured test directories; test-runner configuration is recorded but not interpreted) | CLI `impact`; MCP `atlas_impact` (opt-in) | **Released** (0.1.0) |
| Prebuilt binaries for Linux x86_64/ARM64 (glibc 2.34+) and macOS 11+, checksum-verifying installer | `install.sh`, GitHub Releases | **Released** (0.1.0) |
| Markdown project knowledge: section search, combined code and docs context | CLI `knowledge search`, `context --scope`; MCP `atlas_context` with `scope` | **On `main`** (0.1.1) |
| Knowledge relationships: explicit requirement IDs, ADR metadata and links, bounded traces | CLI `knowledge trace`; MCP `atlas_knowledge_trace` (opt-in) | **On `main`** (0.1.1) |
| Agent investigation: ordered, advisory, evidence-backed plans | CLI `investigate`; MCP `atlas_investigate` (opt-in) | **On `main`** (0.1.1) |
| Reviewed project memory with evidence snapshots, history and live verification | CLI `memory …`; MCP `atlas_memory` (opt-in, read-only) | **On `main`** (0.1.1) |
| Durable memory store (`.repogi/memory.db`) with migration, export/import and recovery | CLI `memory status`, `migrate`, `export`, `import`, `rebind` | **On `main`** (0.1.1) |

**Known limits today:**
- **Languages:** only TypeScript and TSX code is indexed. JavaScript, JSX, `.mts` and `.cts` files are not indexed, and a TypeScript import that resolves to one is reported as *unsupported*.
- **Packages:** package imports, `package.json` `exports` and workspace packages are not resolved.
- **Knowledge:** project knowledge is Markdown only, with lexical search. Investigations follow one explicit-claim hop.
- **Windows:** not supported.
- **Measurement:** so far it is limited to labeled fixtures. There is no published, reproducible evaluation of Repogi with real coding agents.

## Priorities at a glance

| Horizon | Initiative | Priority | Status |
| --- | --- | --- | --- |
| Now | [Release 0.1.1 and platform reliability](#now-release-and-reliability) | P0 | In progress |
| Now | [Reproducible agent evaluation harness](#evaluation-and-measurable-outcomes) | P0 | Planned |
| Next | [Language-support levels and a shared analyzer architecture](#multi-language-indexing-architecture) | P1 | Exploring |
| Next | [JavaScript ecosystem support](#language-expansion) | P1 | Planned |
| Next | [`repogi doctor` and guided setup](#developer-experience) | P1 | Exploring |
| Next | [Better candidate discovery and compact context](#repository-intelligence-improvements) | P1 | Exploring |
| Later | [Python](#language-expansion), then [Rust](#language-expansion) and [Go](#language-expansion) | P2 | Exploring |
| Later | [Monorepo and workspace intelligence](#monorepos-and-multiple-repositories) | P2 | Exploring |
| Later | [Structured project knowledge (OpenAPI, SQL schemas)](#project-knowledge-expansion) | P2 | Exploring |
| Later | [Memory relocation and review workflows](#persistent-memory-improvements) | P2 | Exploring |
| Research | [Semantic retrieval, editor integration, visualization](#longer-term-research) | Research | — |

## Now: release and reliability

**P0 · In progress.** Every feature marked *On `main`* should reach users as a prebuilt, verified release (0.1.1) before new major features land.

| Item | Status | Acceptance criterion |
| --- | --- | --- |
| macOS test suite passes in CI | Planned | The CI macOS job and the release workflow's macOS builds pass on `main`. The macOS job currently fails to compile one test that uses a Linux-only API. |
| Release dry run | Planned | A non-publishing run of the release workflow passes on every published target. That covers build, tests, binary inspection, the install matrix and the official MCP SDK check. |
| Durable memory on macOS filesystems | Planned | Memory migration, export and recovery tests pass natively on macOS. Linux is covered in CI, and the fallback for filesystems without hard links was also checked by hand on a real FAT mount. |
| Upgrade notes for 0.1.1 | Planned | Release notes explain the one-way index upgrade: 0.1.0 refuses newer indexes, and deleting `.code-intel/` and re-indexing restores it. |
| Release governance | Planned | Publishing requires an approval step, and changes to `main` require passing CI. |
| Windows | Exploring | The full test suite passes natively on Windows. Until then, Windows stays documented as unsupported and no Windows binary or `install.ps1` is published. |
| Security regression coverage | Planned | Existing protections against symlinks, path traversal and hostile imports keep their tests. Each new file format or command adds equivalent negative tests. |

## Evaluation and measurable outcomes

**P0 · Planned · Effort L.** This prerequisite comes before any claim about agent accuracy, token use or cost. Until it exists, Repogi makes no such claims.

**Benefit:** adopters and maintainers can see where Repogi helps, where it is neutral and where it costs more. Priorities can then follow evidence.

**Proposed harness:** the same coding agent, model, prompts, tasks and environment, run in two arms:

- **Baseline:** normal repository search and file reads.
- **With Repogi:** the same, plus Repogi's CLI or MCP tools.

The harness records for each arm:

| Outcome | Measures |
| --- | --- |
| Correctness | Task completion against labeled expectations. Code-change quality where tasks involve edits. |
| Evidence quality | Recall of relevant evidence, false confirmed relationships, citation correctness. |
| Cost and effort | Input and output tokens, tool calls, estimated cost. |
| Time and resources | End-to-end latency, indexing time, memory use, index size. |
| Scale | The same measures on large real repositories, not only fixtures. |

**Rules:**
- Task-level results stay separate from microbenchmarks.
- Task sets, labels and harness code are versioned, and each published result names the exact binary, agent version and model.
- Regressions and neutral results are published alongside improvements.
- Labels for new task sets are reviewed by someone other than their author.
- Repeated runs report variance, and runs that need a paid model require explicit maintainer approval.

**Acceptance criterion:** a third party can rerun a published comparison from the repository and reproduce its result within the reported variance.

## Language expansion

Parsing a language with Tree-sitter is not the same as supporting it. Repogi describes language support in levels, and every supported language states which level it reaches. Different languages may stop at different levels.

| Level | Name | What it provides |
| --- | --- | --- |
| 1 | **Syntax discovery** | Declarations with exact source locations, searchable with `context`. |
| 2 | **Module intelligence** | Imports, exports and package relationships, with an explicit status for unresolved modules. |
| 3 | **Resolved references** | Validated cross-file symbols and typed graph edges such as calls and references. Unresolved and dynamic sites stay unconfirmed. |
| 4 | **Impact intelligence** | Dependency traversal, change impact and test-candidate relationships built on level 3. |

| Language | Today | Next target | Priority |
| --- | --- | --- | --- |
| TypeScript, TSX | Level 4 (package imports unresolved) | Keep; package resolution under [monorepos](#monorepos-and-multiple-repositories) | — |
| Markdown | Project knowledge (intent, not code) | See [project knowledge](#project-knowledge-expansion) | — |
| JavaScript, JSX, `.mjs`, `.cjs`, `.mts`, `.cts` | Not indexed | Level 3, then 4 | P1 |
| Python (`.py`, `.pyi`) | Not indexed | Level 2, then 3 | P2 |
| Rust | Not indexed | Level 2, then 3 for macro-free code | P2 |
| Go | Not indexed | Level 3 | P2 |
| Java, Kotlin, C#, PHP, Ruby, C, C++ | Not indexed | Candidates only | Research |

The order follows reuse of existing work, user demand and how much of each language's semantics can be resolved statically. Every language depends on the [shared analyzer architecture](#multi-language-indexing-architecture).

### 1. JavaScript ecosystem (P1 · Planned · Effort L)

This is the most natural first step: TypeScript projects routinely mix in JavaScript, and today those imports are reported as *unsupported*. The existing TypeScript rules cannot simply be reused, because JavaScript has different semantics:

- **Grammar:** the bundled grammars are TypeScript and TSX grammars, not JavaScript grammars. Whether to add the JavaScript grammar or reuse them must be decided with parser tests, especially for JSX in `.js` files.
- **CommonJS:** `require`, `module.exports` and `exports.x` need their own binding rules. Patterns that cannot be resolved statically must stay unconfirmed.
- **ESM and Node resolution:** ESM imports and re-exports, `.mjs`/`.cjs`, and the TypeScript NodeNext extensions (`.mts`, `.cts`) follow different resolution rules.
- **JSX:** component references in `.jsx`.
- **Dynamic code:** `import()` with computed specifiers and reflective access remain unconfirmed.
- **Mixed projects:** TypeScript files importing JavaScript, `allowJs` and declaration files.

**Acceptance criterion:**
- Zero incorrect confirmed edges on a labeled set from at least two real open-source JavaScript or mixed repositories, with reported resolution coverage.
- Fixture suites for ESM, CommonJS, JSX and mixed projects.
- Existing TypeScript outputs stay byte-identical.

### 2. Python (P2 · Exploring · Effort L)

Python analysis must follow Python's semantics, not TypeScript's. The scope:

- **Code:** modules, functions, classes and methods; decorators; `.pyi` stubs.
- **Imports:** absolute and relative imports, `__init__.py` packages and `__all__`.
- **Resolution:** a static resolution profile that reports dynamic attribute access, `importlib` and monkey-patching as unconfirmed.

**Acceptance criterion:** level 2 with explicit unresolved-import reasons on a labeled real-repository set, followed by level 3 for direct calls and references with zero incorrect confirmed edges.

### 3. Rust (P2 · Exploring · Effort L)

Rust support is useful in its own right, and it would let Repogi be evaluated on its own source. The scope:

- **Code structure:** modules, `use` and re-exports, structs, enums, traits, `impl` blocks and methods.
- **Build context:** Cargo workspace and crate boundaries, and feature-gated (`cfg`) code, recorded as conditional.
- **Macros:** macro-generated items are reported as unresolved, never guessed.

**Acceptance criterion:** level 2 across a Cargo workspace (Repogi itself), then level 3 for code that does not depend on macros, with macro-dependent sites reported as unresolved.

### 4. Go (P2 · Exploring · Effort M)

The scope covers packages and imports, functions and methods, interfaces and struct relationships, module boundaries, build tags and cross-package references. Go's explicit package model makes level 3 comparatively attainable. Interface satisfaction is implicit, so it stays a separate, clearly labeled relationship.

### Later candidates (Research)

Java, Kotlin, C#, PHP, Ruby, C and C++ are candidates, not commitments. They are considered after the shared architecture has proven itself on at least two new languages. Each needs community demand and an evaluation plan.

## Multi-language indexing architecture

**P1 · Exploring · Effort L.** This is an internal design that lets languages be added without duplicating the index, storage and retrieval engines. It is not a user-installable plugin system.

The proposed design keeps one shared model for evidence and many language-specific analyzers:

- **Shared:** file discovery and eligibility, hashing and incremental updates, stable identities, the declaration and relationship model, explicit resolution status and confidence, bounded traversal, response budgets, storage and schema migration.
- **Per language:** grammar, declaration extraction, module resolution and binding rules. Each analyzer has a versioned extractor contract, so a change forces a re-parse of only the affected files.
- **Mixed-language projects:** every fact records its language, and symbols from different languages are never merged by name. A cross-language reference, such as TypeScript importing JavaScript, exists only when a resolution rule establishes it, and it carries that rule as provenance.
- **Correctness tests:** each analyzer has its own fixture suite and labeled gold set. Adding a language must leave existing outputs byte-identical.

**Dependency:** this comes before any new language and is designed together with JavaScript support, which is the first consumer. **Acceptance criterion:** JavaScript is added through the shared design without changing the TypeScript analyzer's outputs, and the extractor contract is documented for contributors.

## Repository intelligence improvements

| Initiative | Priority · Status | User benefit | Acceptance criterion |
| --- | --- | --- | --- |
| **Better candidate discovery.** Task words often differ from code names. Expand queries through imports, re-exports, requirement-to-code links, documented terminology and identifier normalization. | P1 · Exploring | Finds the right code when the task uses different words | Higher recall on labeled tasks without lower precision. Inferred candidates stay labeled separately from validated relationships. |
| **Compact investigation context.** Compact JSON, selectable detail levels, excerpt deduplication, progressive loading. | P1 · Exploring | More useful evidence per response byte | Measured evidence recall at fixed byte budgets improves, with completeness reporting intact. |
| **Bounded multi-hop knowledge discovery.** Go beyond one explicit-claim hop. | P2 · Exploring | Follows requirement → decision → code chains | Configurable depth, work limits, cycle handling, evidence lineage, truncation reporting and deterministic order. It ships only if labeled tasks show a gain. |
| **Change-impact and test discovery.** Better test ranking, test-framework configuration awareness, test ownership, monorepo package boundaries, mocks and fixtures. | P2 · Exploring | Fewer irrelevant test candidates | Precision and recall on labeled change sets. Output still states that static links do not prove coverage or passing tests. |
| **Freshness.** Faster stale-index detection, configurable selected-file verification, incremental graph invalidation, clearer incomplete-inventory reporting. | P2 · Exploring | Agents know when evidence is out of date | Queries stay read-only and bounded. Stale and incomplete states are reported in every affected response. |

## Developer experience

All commands in this section are **proposals**. None exist yet.

| Proposal | Priority · Status | What it would do | Constraints |
| --- | --- | --- | --- |
| `repogi doctor` | P1 · Exploring | Check the binary version, index status and schema compatibility, root ownership, memory store health, freshness, configured agent integrations and platform support. Report problems with fixes. | Read-only. Reports, never repairs silently. |
| `repogi init` | P1 · Exploring | Detect a supported repository, explain where the index and memory live, suggest `.gitignore` entries, run the first index on confirmation and print agent setup steps. | Never overwrites existing files or configuration. Every write is shown first. |
| Guided agent setup | P1 · Exploring | Print, or with confirmation write, project-scoped MCP configuration for Claude Code, Codex and other MCP clients. | Never edits global or unrelated settings. |
| Project configuration (`repogi.yaml` or similar) | P2 · Exploring | Optional include/exclude rules, language and document discovery, retrieval defaults, watch preferences. | Cannot widen access beyond the repository root, follow links outside it or disable symlink and size protections. |
| `repogi watch` | P2 · Exploring | Opt-in, foreground incremental re-indexing on file changes. | No hidden background service. CPU, memory and file-watcher limits documented per platform. Explicit indexing stays the default. |

## Agent integrations

Repogi works with any MCP client over stdio. Claude Code and Codex have documented setups and examples today.

- **Keep compatibility.** `atlas_*` tool IDs and default profiles stay stable. New tools are opt-in, and the release workflow checks interoperability with the official MCP SDK.
- **Routing guidance (P1).** Document, and measure through the [evaluation harness](#evaluation-and-measurable-outcomes), when an agent should use Repogi rather than grep and when it should not.
- **More clients (P2).** Add setup guides for other MCP-compatible agents, each tested against a real client.
- **No write tools by default.** MCP stays read-only for memory. Any future write capability would need an explicit design and opt-in.

## Monorepos and multiple repositories

**P2 · Exploring · Effort L.**

| Initiative | Acceptance criterion |
| --- | --- |
| Package and workspace detection (npm, pnpm and Yarn workspaces; later Cargo and Go workspaces) | Package boundaries appear in responses, and cross-package imports resolve with provenance instead of being reported as *unsupported*. |
| Package `exports`/`imports` resolution for TypeScript and JavaScript | Labeled cross-package references reach zero incorrect confirmed edges. Unresolvable conditions stay unconfirmed. |
| Workspace-level search across mixed-language packages | Every result names its package, language and resolution status. |
| Explicit multi-repository queries | Only repositories configured together are queried. Each result carries its repository, results are bounded in aggregate, and indexes and memories stay separate. |

**Rule:** Repogi never implicitly combines unrelated repositories or their memories. One executable can serve many projects, but indexes, identities and durable memory stay scoped to their own repository. Cross-repository queries require explicit configuration and report provenance for every result.

## Project knowledge expansion

**P2 · Exploring.** Structured formats come first, because they can provide verifiable relationships.

| Format | Value | Priority |
| --- | --- | --- |
| OpenAPI specifications | Operations and schemas linked to handlers and clients | P2 |
| SQL migrations and schema files | Tables and columns linked to the code that uses them | P2 |
| JSON/YAML project configuration and architecture manifests | Declared components, services and ownership | P2 |
| PDF and DOCX | Prose only, with weak source locations | Research, only if justified |

Every format keeps source locations and documented extraction limits. Ingestion is untrusted data processing: files are parsed, never executed or evaluated. Structured formats are kept distinct from prose. A documented relationship stays recorded intent, never proof of implementation.

## Persistent memory improvements

**P2 · Exploring.** The foundation is complete: reviewed claims, immutable evidence snapshots, durable storage, export/import and recovery. Next:

| Initiative | Benefit |
| --- | --- |
| Relocation after file renames and Git-aware source identity recovery | Fewer memories reported `missing` after refactors, re-verified rather than assumed |
| Stale-memory review workflow and conflict explanations | Faster, clearer decisions to correct, supersede or retire |
| Supersession navigation | Easier to follow how a decision changed |
| Memory-aware investigation ranking | Approved memories placed where they help, measured by the evaluation harness |
| Streaming export and import | Large stores without loading everything into memory |
| Optional signed review records | Verifiable reviewer identity. Today reviewer names are self-reported labels. |
| Sharing between explicitly linked workspaces | Reuse reviewed knowledge across authorized repositories through review import |

These rules do not change: approval is explicit, evidence history is immutable, approval stays separate from source verification, memory is isolated per repository, and MCP memory access is read-only by default. Repogi will not promote agent-generated claims into memory automatically.

## Longer-term research

These are considered only if their value can be shown through the evaluation harness. They are not commitments.

- Optional local semantic embeddings and hybrid lexical, graph and semantic retrieval, always local and optional.
- Editor or language-server integration, once the CLI and MCP experience is stable.
- Repository graph visualization.
- Agent feedback and investigation-quality metrics.
- Structured API and data-flow tracing.
- Incremental polyglot graph resolution.

## What we're not building yet

| Deferred | Why |
| --- | --- |
| Mandatory hosted indexing, cloud accounts or telemetry | Conflicts with local-first operation and repository isolation. |
| Autonomous code modification | Repogi provides evidence; agents and developers make changes. |
| Automatic memory generation or promotion | Memory is valuable because a human reviewed it. |
| Model-based indexing | Expensive, non-deterministic and hard to verify. |
| A full IDE | The core CLI and MCP experience comes first. |
| A public plugin marketplace | The internal analyzer architecture must stabilize first, and third-party analyzers raise security and support costs. |

## How priorities are decided

Initiatives are ranked by, in order:

1. User value.
2. Correctness and reliability.
3. Engineering dependencies.
4. Maintenance cost.
5. Security implications.
6. Evaluation evidence.
7. Community demand.

Popularity alone does not reorder the roadmap. An initiative moves forward when its dependencies are met and it has a measurable acceptance criterion. It moves back, or is dropped, when evaluation shows little value or a reliability or security cost. Changes to this file are made through pull requests, so the reasoning stays visible.

## Contributing

Good ways to help now:

- **Report real-world gaps.** Open an issue with a repository pattern that Repogi resolves incorrectly or reports as unresolved. Incorrect *confirmed* results are the most valuable reports.
- **Add fixtures and labels.** Small, licensed fixture repositories and labeled expectations improve every evaluation.
- **Test platforms.** Report macOS and Linux distribution results, especially filesystem and installer behavior.
- **Language design.** Before writing code for a new language, open a design issue that covers its target support level, resolution rules and how unresolved sites are reported.

See the [README](README.md#documentation-and-contributing) for development commands and the [documentation index](docs/README.md) for architecture and behavior details. Prefer scoped changes with fixture-backed tests, offline processing and honest documentation of what is not supported.
