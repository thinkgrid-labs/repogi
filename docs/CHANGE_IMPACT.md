# Git-aware change impact

`repogi impact` answers one question: *given these code changes, which declarations and files might be affected, and what static evidence supports that?*

- **Read-only.** It never edits sources, the user's `.code-intel/index.db`, the Git index or refs, and it never runs repository code.
- **Its own cache.** It writes only `.code-intel/impact/`, or the directory given with `--cache-dir`.
- **MCP.** The same engine is available to agents as the opt-in MCP tool `atlas_impact` ([MCP_IMPACT.md](MCP_IMPACT.md)). It uses identical analysis and rendering; the root is pinned, the cache location is fixed, and responses are capped at 32 KB.
- **Affected tests.** `--tests` (CLI, [AFFECTED_TEST_DISCOVERY.md](AFFECTED_TEST_DISCOVERY.md)) and `tests: true` (MCP, [MCP_TEST_DISCOVERY.md](MCP_TEST_DISCOVERY.md)) list existing tests statically linked to the change. Nothing is executed.
- **Verification manifest.** `--manifest --format json` composes change, impact,
  test and uncertainty evidence from the same comparison into a versioned,
  deterministic artifact. See [VERIFICATION_MANIFEST.md](VERIFICATION_MANIFEST.md).
  The opt-in [MCP transport](MCP_VERIFICATION_MANIFEST.md) retains the complete
  bytes; [CI integration](CI_ARTIFACT_INTEGRATION.md) documents export and
  `repogi manifest-verify`. Nothing is executed or declared safe.

```sh
code-intel impact --base HEAD                       # committed HEAD vs the working tree (default)
code-intel impact --base main --head feature --format json
code-intel impact --base HEAD~1 --head HEAD --detail --depth 3
code-intel impact --base HEAD --summary --max-bytes 8000
code-intel impact --base HEAD --evidence-file src/components/Card.tsx
code-intel impact --base HEAD --tests               # advisory test discovery; no execution
```

| Option | Default | Meaning |
|---|---|---|
| `[PATH]` | `.` | A Git work tree, or a directory inside one; only that directory is analysed |
| `--base <rev>` | `HEAD` | Baseline commit, branch or tag |
| `--head <rev>` / `--worktree` | working tree | The changed state: a commit, or the files on disk |
| `--format text\|json` | `text` | Concise text, or the full structured report |
| `--depth 1..4` | 2 | Upstream hops; 1 means direct consumers only |
| `--max-results 1..1000` | 200 | Cap on every result list (dropped entries are counted) |
| `--max-declarations` | 100 | Changed declarations whose consumers are analysed (the rest are listed) |
| `--max-bytes 2000..8000000` | 16000 text, 64000 JSON | Cap on the whole output; lists shrink to fit and stay valid JSON (MCP: 2000–32000, default 8000) |
| `--detail` | off | Rules, binding chains, transitive paths, import-only, possible and all rebound sites |
| `--summary` | off | Group files before site caps, with explicit completeness; combines with JSON. See [IMPACT_SUMMARIES.md](IMPACT_SUMMARIES.md). |
| `--tests` | off | Advisory confirmed/possible test files and verification gaps; conflicts with summary. See [AFFECTED_TEST_DISCOVERY.md](AFFECTED_TEST_DISCOVERY.md). |
| `--test-dir <relative-dir>` | none | Additional literal probable-test directory, repeatable; requires tests or manifest |
| `--manifest` | off | Complete versioned verification evidence; requires JSON and conflicts with tests, summary, detail and evidence selection. Default maximum is 8 MB; fails instead of clipping. |
| `--evidence-file <path>` | none | Detailed evidence for one affected repository-relative file; explicitly partial scope |
| `--cache-dir <dir>` | `<repo>/.code-intel/impact` | Snapshot cache |

## 1. Architecture

The engine reuses the existing crates: no new database, no new resolver, no sixth crate.

| Piece | Crate | What it does |
|---|---|---|
| Git access | `index` (`git.rs`) | Runs plumbing commands (`rev-parse`, `ls-tree`, `cat-file --batch`, `ls-files -s`, `hash-object --no-filters`, `check-attr`, `config --get`, and `diff -M --name-status` between two trees or with `--cached`) as argument vectors, with no shell. Hooks, fsmonitor, the untracked cache, external diff, textconv, replace objects and network protocols are disabled. `GIT_DIR`-style environment overrides are removed. Blob content is read raw: no clean or smudge filter runs. |
| Snapshots | `index` (`snapshot.rs`) | Builds or reuses one Repogi index per analysed state. A snapshot is the existing indexer run over a source directory into a separate database (`index_snapshot`). |
| Engine | `retrieval` (`impact.rs`) | Analysis over two read-only `Store`s: file and declaration diff, binding diff, incoming traversal, uncertainty and completeness. `impact::compare` is the entry point; `impact::summary::respond` renders the default, summary and selected-evidence views. |
| Test discovery | `retrieval` (`tests.rs`) | `tests::compare` reuses the same comparison (`impact::inspect_comparison`) to classify probable test files; `tests::respond` renders and fits the plan (Phase 4.5). |
| CLI | `cli` | `repogi impact`: argument validation and rendering under a byte budget. |
| MCP | `cli` (`mcp/tools.rs`) | The opt-in `atlas_impact` tool: argument validation, root confinement and error codes over the same `respond` functions. |

## 2. Snapshots

Symbol IDs (`kind:path:start:end`) are snapshot-specific, so the engine never compares IDs across snapshots. Each side of the comparison is a full Repogi index of exactly that state.

| State | Source directory | Database | Reuse |
|---|---|---|---|
| Commit | The commit's tree for the analysed directory, materialised from raw blobs into `<cache>/trees/<tree>-<build>/src`. Only what the indexer reads or what affects module resolution: TS/TSX, JS-family files, `*.json`, `.gitignore`. Symlinks and submodules are not materialised and appear as coverage diagnostics. | `…/index.db` | Keyed by tree OID and the **build key**: IR version plus a BLAKE3 prefix of the `repogi` executable. Reused only after validation (below). |
| Working tree | The repository itself | The user's `.code-intel/index.db`, opened read-only, **only if** it is provably current: same IR version, same build, same repository root, same resolution fingerprint, and every content hash matching the files on disk. Otherwise `<cache>/worktree/index.db`, updated incrementally. | Content-hash incremental |

How the cache stays trustworthy:
- **Build identity.** Every database records the build that wrote it (`indexer_build`). Snapshot indexing reuses a file's cached syntax IR only if the same build wrote it, so a database seeded from, or left by, another build is fully reparsed rather than trusted. The plain `index` command keeps its earlier behaviour: an IR-version match suffices.
- **Seeding.** A new snapshot database is seeded with a consistent copy (`VACUUM INTO`) of an existing one: same IR version, same repository root. It is then indexed incrementally. Incremental indexing yields the same graph as a cold index; a test compares them edge for edge.
- **Atomic install.** Commit snapshots are built in `.tmp-*` directories and renamed into place. A marker (`snapshot.json`: tree, build key, content digest, file and edge counts) is written last.
- **Validation on reuse.** An entry is reused only if the marker parses and the database's digest, counts and recorded build all match it. A tampered, truncated or corrupt entry is replaced, never trusted.
- **Concurrency.** If another run installed a valid copy meanwhile, that copy is used. Concurrent first opens of a database migrate it once (immediate transaction). Concurrent updates of the shared working-tree database retry on the revision check.
- **Eviction.** The four most recently used commit snapshots are kept. Interrupted `.tmp-*` builds older than an hour are removed. Eviction only touches directories with a valid Repogi marker, or named like Repogi temporaries.
- **Source verification.** Source text is read back for module-level comparison and checked against the snapshot's content hash. Text that is unreadable or altered is a completeness-blocking limitation.

## 3. What counts as a change

### Files

File lists come from blob IDs; renames come only from Git's evidence (`-M`, staged or committed).
- **Commit vs commit.** The two trees' blob IDs.
- **Working tree.** The files on disk as the indexer would read them, hashed with `hash-object --no-filters`. Staged and unstaged edits are both on disk, so both are analysed; staged content that differs from disk is not.
- **`git_state` labels.** Each working-tree change is labelled `staged`, `unstaged`, `staged+unstaged` or `untracked` (from base, Git-index and disk blob IDs). It is `excluded` when a file still on disk is no longer read: newly ignored, or no longer a regular file.
- **Untracked and ignored files.** Untracked files that `.gitignore` does not exclude are part of the working-tree snapshot, exactly as `index` would read them. Ignored files are not.
- **Ignore sources.** Only `.gitignore` files inside the analysed directory count; `.git/info/exclude` and global excludes do not, matching `index`.
- **Unstaged moves.** A plain `mv` leaves Git no evidence, so it is reported as a removal plus an addition.
- **Categories.** Every changed file has a category: `source`, `config`, `package`, `json`, `module` (JS-family) or `ignore_rules`.

### Configuration

`resolution_changed` is true when the module-resolution context differs: a config, or the set of module files. A warning appears only when a configuration file changed. The binding diff (§4) shows the effect either way.

### Declarations

Covered: functions, classes, methods, interfaces, type aliases, enums and top-level variables, including components. Nested declarations are not separate: a change inside one is a body change of its container.

Declarations are matched within a file by kind and name: identical text first, then the rest. Each modified declaration records its `identity`:

| `identity` | Meaning |
|---|---|
| `unique` | The only declaration of that kind and name on each side |
| `text` | Identical text |
| `remaining` | The single unmatched one on each side, after text matches |
| `position` | Several unmatched, paired in source order: **`uncertain: true`** |

Classification:
- **`unchanged`.** Equal text and exports, even if byte offsets or line numbers moved; not listed, only counted.
- **`modified`.** Carries `signature_changed`/`body_changed` where an AST header exists:
  - functions, classes and methods use the stored header;
  - arrow and function-expression variables are reparsed, including `memo(...)`/`forwardRef(...)` wrappers;
  - otherwise both flags are absent.

  `export_changed` is set when the exported names differ; `whitespace_only` when only whitespace differs.
- **`added` or `removed`.** Unmatched. A rename is never inferred from names or bodies.
- **`unknown`.** Missing on one side because that side's file has syntax errors, was skipped by the indexer, or is `excluded`.

A file rename with Git evidence carries `previous_path`. Its declarations keep their identity, so if their text is unchanged they are not listed at all.

### Module-level code

Changes outside every declaration are compared separately:
- imports;
- export clauses and re-exports;
- `export default <expression>`;
- other top-level statements, compared through the syntax tree, so comments are ignored, while `const`/`let` keywords are kept.

They are reported per file with that file's importers.

## 4. Impact

For each changed declaration, consumers come from both graphs:
- **The base graph** gives previous consumers. For a removed declaration, these are the only consumers.
- **The head graph** gives current consumers, including new ones.

A site found in both graphs is labelled `both`; sites in changed files are paired by consumer, kind and text.

1. **Direct consumers.** Resolved CALLS, REFERENCES (including JSX) and EXTENDS sites, one per source location. A call is also stored as a REFERENCES edge of its callee; the call is kept. Each site carries its enclosing declaration (none at module scope), rule, status and, with `--detail`, the binding chain.
2. **Import-only dependents.** Files whose only confirmed relationship is an IMPORTS edge. Barrels re-exporting the declaration are listed separately (`re_exported_by`).
3. **Transitive dependents.** Breadth-first over incoming resolved CALLS/REFERENCES/EXTENDS from the direct consumers' declarations, up to `--depth`, in the snapshot where each consumer was found. Results are deduplicated and cycle-safe, each with its evidence path (`via`).
4. **Rebound sites.** Sites in **unchanged** files whose binding differs between the snapshots, found by comparing every unchanged file's edges. This is how barrel, rename, path-alias and configuration changes reach code that did not change. Each has an `effect`:

   | `effect` | Meaning |
   |---|---|
   | `retargeted` | Resolved in both snapshots, to different declarations: a confirmed graph change |
   | `gained` | Newly resolved |
   | `lost` | No longer resolved: a possible break, **or only** that Repogi can no longer confirm the binding |
   | `unconfirmed` | Unresolved in both; only its status or rule changed |

   Example of a `lost` site that is not a break: a barrel with one unresolvable `export *` source makes every star-exported name of that barrel unconfirmed in Repogi's profile, although TypeScript still resolves the other names.

5. **Possible impact.** Never confirmed:
   - **Unconfirmed same-name sites.** Unresolved, ambiguous or unsupported CALLS/REFERENCES/EXTENDS whose text is the declaration's name, an export alias, or a member access ending in one. A plain identifier whose binding chain proves it is a parameter or nested local is excluded and counted in `local_same_name_excluded`.
   - **Unanalysable files** that mention the declaration: syntax errors, or sources the indexer skipped. Every such file is screened for the name and its aliases, plus module names where the declaration is reachable as a default export.

A confirmed static reference proves a dependency, not that the change breaks the consumer. Zero confirmed consumers is not proof of safety; the text output says so.

## 5. Completeness and bounded output

`complete` is true only when `incomplete_reasons` is empty. The reasons are:
- **Truncation.** Lists were truncated (`max_results` or the byte budget, with every omission counted) or the per-declaration work limit was hit (5,000 incoming sites per snapshot).
- **Unanalysed declarations.** Changed declarations beyond `--max-declarations`.
- **Possible impact.** A declaration has possible-impact entries.
- **Limitations.** Completeness-blocking conditions:
  - the working tree changed during the analysis (re-checked at the end);
  - source text was unreadable or no longer matched its snapshot;
  - content filters, or line-ending conversion with CRLF files, on changed files;
  - submodules in the working tree, which the working-tree index reads but commit snapshots omit.

The text header says "complete within the analysed static graph". JavaScript sources, `.mts`/`.cts`, packages, excluded directories and runtime behaviour are never analysed; they are always listed under `coverage.not_analysed`, so "complete" never means "all consumers in the program".

`--max-bytes` shrinks the least important lists first, by binary search:
1. coverage detail;
2. transitive, possible, import-only, rebound and affected-file lists;
3. direct consumers, changed files and declarations.

Truncated output always has `truncated: true` and `complete: false`. JSON stays valid at any budget from 2,000 bytes.

## 6. Unsupported and limited cases

**Git:**

| Case | Behaviour |
|---|---|
| Unstaged move or rename | Removal plus addition (no Git evidence) |
| Staged content that differs from disk | Disk is analysed; the file is labelled `staged+unstaged` |
| Clean/smudge filters (LFS…) | Raw blobs and files are compared; listed as a limitation (incomplete) |
| `eol`/`text`/`core.autocrlf` conversion | Raw bytes are compared; CRLF working-tree files with conversion are a limitation |
| Submodules | Not materialised in commit snapshots (coverage diagnostic). In the working tree, the indexer reads their files: a limitation |
| Symlinks | Skipped in both snapshots (coverage) |
| `.git/info/exclude`, global excludes | Ignored (as in `index`): such files count as untracked |
| Shallow or partial clones | Revisions and blobs must be present locally; missing objects are an error, never fetched |
| Analysed directory below the repository root | Supported (`git rev-parse --show-prefix`); renames are limited to that directory |

**Language** (from [SEMANTICS.md](SEMANTICS.md) and [JSX_SEMANTICS.md](JSX_SEMANTICS.md)):
- Nested declarations are not targets.
- Instance and inherited method dispatch is not resolved; such calls appear only as unconfirmed same-name member sites.
- Values passed through props, registries or `memo(X)` + `X.displayName = …` (mutated bindings) appear as unconfirmed same-name sites.
- Package and JavaScript consumers are not analysed.
- Files with syntax errors have no semantic edges.
- Overloads and merged declarations can only be paired by position when several change at once (`uncertain`).
- A rename is never inferred.
