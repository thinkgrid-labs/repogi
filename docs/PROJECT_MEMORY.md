# Project memory

Project memory, with durable storage, backup and recovery, is part of release 0.1.1. Until 0.1.1 is published, [build from source](DEVELOPER_PREVIEW_INSTALLATION.md#build-from-source) to use it.

Project memory lets developers keep **reviewed project knowledge** across sessions: architecture decisions, constraints, requirements, conventions and implementation notes, each stated as a short claim with the indexed evidence it was based on. Coding agents can read approved memories, through the CLI, the opt-in `atlas_memory` MCP tool or `repogi investigate --include-memory`, instead of rediscovering the same decisions.

A memory is not indexed Markdown and not an investigation result:

| Record | What it is | Lifetime |
| --- | --- | --- |
| Document | Repository source material (Markdown, TypeScript). | The working tree. |
| Knowledge entity / relationship | A requirement, decision or explicit claim parsed from documents. | Rebuilt by every `repogi index`. |
| Investigation | A temporary plan of what to inspect. | Never stored. |
| **Memory** | A developer-reviewed claim with immutable evidence snapshots, revisions and a review history. | Kept in `.repogi/memory.db` until retired or superseded; never deleted, and independent of the rebuildable index. |

Nothing becomes a memory automatically. Investigations, searches, indexing and MCP calls never create, approve or change memories. A developer proposes a claim, and a named reviewer approves the exact revision.

## Lifecycle

```text
propose ──► pending_review ──approve──► approved ──supersede──► superseded
                 │                        │   └──retire──────► retired
                 └──reject──► rejected    └──correct──► (approved, with a pending revision)
                                                          ├──approve──► approved at the new revision
                                                          └──reject───► approved at the old revision
```

- **Review status** (`pending_review`, `approved`, `rejected`, `superseded`, `retired`) records what a reviewer decided.
- **Evidence verification** (`current`, `stale`, `missing`, `unresolved`, `unverified`) is computed live on every read and is never folded into the review status. An approved memory can have stale evidence, and current evidence does not prove that a claim is correct.
- A **correction** creates a new revision on an approved memory. The approved revision stays authoritative until the correction is reviewed. Only one revision can await review at a time.
- **Supersession** links two approved memories. The old memory keeps its history and is shown only as historical.
- Revisions, evidence snapshots and review events are never rewritten or deleted. SQLite triggers refuse updates and deletes of evidence and events, any change to a reviewed revision, and deletion of memories.

## CLI

All memory commands work on the nearest repository with a `.code-intel/index.db` or `.repogi/memory.db`. Memories are stored durably in `.repogi/memory.db` (see [memory storage](MEMORY_STORAGE.md)); the code index is read to capture and verify evidence and is optional for reads. `list`, `show`, `history`, `search`, `verify`, `conflicts`, `status` and `export` are read-only and never create or migrate anything. `propose`, `correct`, `approve`, `reject`, `supersede`, `retire`, `verify --record`, `migrate`, `import` and `rebind` are explicit writes. No memory command writes the code index's data.

```sh
repogi memory propose --type architecture_decision \
  --title "Sessions persist in SQLite" \
  --claim "Session records are stored in an embedded SQLite database file; no external database server is required." \
  --evidence ADR-0001 --evidence knowledge-edge:<id> --source docs/ARCHITECTURE.md --author dev
repogi memory show mem-0001                     # pending revision, evidence, live verification, digest
repogi memory approve mem-0001 --digest a0e2013eedbbc467 --reviewer alice
repogi memory reject  mem-0001 --digest a0e2013eedbbc467 --reviewer alice --reason "..."
repogi memory search "SQLite"                   # approved memories only, by default
repogi memory verify mem-0001                   # or --all; add --record --reviewer <name> to log the result
repogi memory correct mem-0001 --claim "..." --reason "..." --evidence ADR-0001 [--expect-version N]
repogi memory supersede mem-0003 --by mem-0004 --reason "..." --reviewer alice
repogi memory retire mem-0004 --reason "..." --reviewer alice
repogi memory history mem-0001
repogi memory list [--status approved] [--type requirement] [--after mem-0020] [--limit 20]
repogi memory conflicts
repogi memory status [--check]                  # where memories live, store identity, integrity
repogi memory migrate                           # move index-stored memories into .repogi/memory.db
repogi memory export --output repogi-memory.json
repogi memory import repogi-memory.json [--restore] [--dry-run]
repogi memory rebind --from <recorded root>     # after moving the repository
```

Memory types: `architecture_decision`, `project_constraint`, `requirement`, `project_convention`, `implementation_note`. Titles are at most 200 bytes and claims at most 2,000 bytes; both refuse control characters, newlines included. `--asserts SUBJECT=status` (for example `REQ-AUTH-001=deprecated`) records a structured statement used only for conflict detection. Every read takes `--format text|json` and `--max-bytes` (default 12,000 text, 24,000 JSON). Output that does not fit drops trailing items and reports `truncated: true`.

Failures exit non-zero with `{"error":{"code":…}}` on stderr. Memory errors keep stable codes: `memory_not_found`, `index_required`, `digest_invalid`, `memory_root_mismatch`, `memory_store_invalid`, `memory_store_newer`, `memory_store_copied`, `migration_conflict`, `migration_required`, `import_invalid`, `import_unsupported`, `integrity_mismatch`, `import_conflict`, `repository_mismatch`, `output_exists`, `output_rejected`, `duplicate_memory`, `evidence_required`, `evidence_not_found`, `ambiguous_evidence`, `evidence_cannot_back`, `evidence_not_current`, `evidence_unreadable`, `invalid_path`, `digest_mismatch`, `reviewer_required`, `nothing_to_review`, `review_pending`, `invalid_transition`, `concurrent_update`, `invalid_actor`, `invalid_memory_id` and `invalid_arguments`.

## Approval model

Approval is a deliberate, recorded act:

1. **Digest binding.** Every revision has a BLAKE3 digest over its type, title, claim, assertion and every evidence snapshot. `approve` and `reject` must name at least 12 hex characters of the pending digest (from `memory show`). A digest for anything else is refused with `digest_mismatch`.
2. **Named reviewer.** Every review write (`approve`, `reject`, `supersede`, `retire`, `verify --record`) needs `--reviewer <name>` or `REPOGI_MEMORY_REVIEWER`. Without one it fails with `reviewer_required`. The name is recorded in the history. It is a self-reported label, not authentication.
3. **Current evidence at approval, inside the write transaction.** Under the memory write lock, approval re-reads the pending revision, recomputes its digest from the stored claim and evidence (`digest_invalid` if they differ), and re-verifies every backing item against one read snapshot of the index and the working tree. Every backing item must be `current` (`evidence_not_current` otherwise), at least one is required, and approval needs the code index (`index_required`). The state change and the `approved` event commit together. See [approval consistency](MEMORY_STORAGE.md#approval-consistency).
4. **No agent write path.** MCP exposes only read operations. Repository text, search results and investigation output never trigger writes. A note in a document that looks like an approval command is just indexed text.

Repogi cannot tell a human from an agent that has unrestricted shell access and runs the CLI. The digest and reviewer requirements make approval explicit and auditable, not impossible to automate. If an agent has shell access, review its commands, or keep `REPOGI_MEMORY_REVIEWER` unset in its environment.

## Evidence

Evidence is named explicitly and resolved to one exact indexed identity. Several matches are refused as ambiguous (`ambiguous_evidence`), and names are never resolved by similarity.

| Selector | Resolves to | Class | Can back a claim |
| --- | --- | --- | --- |
| `--evidence REQ-…` / `ADR-…` / entity ID | Requirement, decision, document or section | `recorded_intent` | yes |
| `--evidence knowledge-edge:<id>` (validated) | An explicit document claim with a validated target | `validated_knowledge_reference` | yes |
| `--edge <source-fact-id> <target-fact-id>` | A resolved CALLS/REFERENCES/EXTENDS edge | `verified_static_relationship` | yes |
| `--source <path>` | An indexed Markdown document | `recorded_intent` | yes |
| `--evidence <fact-id>`, `--source <code path>` | A declaration or file | `indexed_code_location` | context only |
| `--evidence knowledge-edge:<id>` (not validated) | An unresolved, ambiguous, invalid or stale claim | `unresolved_reference` / `stale_reference` | context only |

`--context` and `--context-source` attach evidence that never backs the claim. Passing a context-only item with `--evidence` is refused with `evidence_cannot_back`. A revision holds at most 16 evidence items.

Each item is captured **by value**: class and role, the selector as typed, the exact subject ID, author ID, kind, title, source range, the indexed whole-file hash, a BLAKE3 hash of the cited bytes, the index revision, relationship details (ID, kind, endpoints, status, target path and, for static edges, endpoint names) and the decision status. The cited file must match the index when it is captured, so the snapshot describes bytes that were both indexed and on disk. Snapshots are never updated: a re-index or source change cannot rewrite them.

These classes describe the evidence, not the truth of the claim. A validated reference proves target identity only, a recorded decision is intent, and a static edge is a static fact about the code.

## Verification and staleness

`memory verify`, `show`, `search`, `conflicts` and `investigate --include-memory` verify each snapshot against the current index and working tree, read-only:

1. **Identity.** The recorded entity, relationship, fact or static edge is looked up by exact ID. Fact and relationship IDs contain byte offsets, so a missing ID is re-located only by exact data: the same author ID, the same claim (source, kind and target text), or the same declaration kind, name and file. A unique match is reported as `identity_relocated` and the bytes then decide. No match is `missing` (`identity_removed`, `relationship_removed`); several matches are `unresolved` (`identity_ambiguous`). A relationship that is no longer validated, or now points elsewhere, is reported as `relationship_status_changed` or `relationship_target_changed`.
2. **Decision status.** A cited decision whose recorded status changed to `superseded`, `deprecated` or `rejected` is `stale` (`decision_status_changed`).
3. **Bytes.** The cited file is read through the same symlink-refusing, root-confined primitive as selected-file checks. A file that differs from the index is `stale` (`source_changed_since_index`; re-index, then verify again). A deleted file is `missing` (`source_deleted`). Cited bytes whose BLAKE3 hash differs from the snapshot are `stale` (`cited_bytes_changed`). An unreadable file, or one past the 64-file read cap, is `unverified`.
4. **Index revision.** A newer index revision is reported as `index_revision_advanced`, and a lower one (the index was rebuilt or replaced) as `index_rebuilt`. Both are informational only: identities are re-resolved, never assumed.
5. **No index.** Without `.code-intel/index.db`, every item is `unverified` (`index_unavailable`), and responses carry a `storage` note. Memories and history stay readable.

A memory's state is the worst state of its backing evidence (`missing` > `unresolved` > `stale` > `unverified` > `current`). With no backing evidence it is `unverified`. Verification never edits, approves, corrects or supersedes a memory, and never stores its result. `verify --record --reviewer <name>` appends a `verified` event to the history and changes nothing else.

When a source changes, the approved claim and its snapshot stay as they were. Reads show `stale_evidence` with the changed paths, and `memory history` keeps the record. A reviewer then corrects, supersedes or retires the memory.

## Search

`memory search` uses SQLite FTS5 over each memory's authoritative revision: its title, claim, cited author IDs, paths and assertion subject. Memories that cover every query term rank first (`match_mode: all_terms`). Partial matches only fill remaining slots and are labelled `partial_terms`. Results are ordered by BM25 rank, then ID.

By default only approved memories are searched. `--status rejected|superseded|retired|pending_review` and `--include-unreviewed` show other records, always with `authoritative: false` and a warning (`unreviewed`, `rejected`, `superseded`, `retired`). Each result includes its ID, type, status, claim, live verification, cited author IDs, source citations with per-item state, and staleness warnings. Candidates are capped at 64 per query, and the result limit is 1–100. No embeddings or network services are used.

## Conflicts

`memory conflicts` reports structured conflicts only, and never decides which memory is right:

| Kind | Detected when |
| --- | --- |
| `conflicting_assertions` | Two approved memories assert different statuses for the same `--asserts` subject. |
| `superseded_decision_reference` | Approved backing evidence cites a decision whose current status is historical, or that a validated `SUPERSEDES` claim replaces (successors are named). |
| `evidence_missing` / `evidence_unresolved` / `evidence_changed` | Approved backing evidence verifies as `missing`, `unresolved` or `stale`. |

Each conflict lists the memory IDs, the subject, the evidence items with their changes, and `review_required: true`. Natural-language contradictions are not detected. A scan verifies at most 200 approved memories, in ID order. `scan_limit_reached` and `--after <id>` continue a larger set.

## Investigation integration

`repogi investigate "<task>" --include-memory` (MCP: `include_memory: true`, offered only when `atlas_memory` is enabled) adds approved memories as their own evidence class, `approved_project_memory`. This class records review status, not runtime proof. Without the flag, investigation never reads memory, and its output is byte-identical to investigation without memory support.

A memory is recalled when its approved evidence cites a plan identity together with at least one task term, or when it matches every task term. With explicit selectors, only the selected identities count and no term is needed. Plan identities are the explicit selection if there is one, otherwise the ranked code targets and knowledge targets. Memories citing an identity and matching every term rank first, then identity-only matches by plan priority, then text-only matches. At most 4 memories are recalled (`budget.max_memories`). Pending, rejected, superseded and retired memories are never recalled.

- Current memories form one `review_memory` step (category `memory`, after `knowledge`). Its evidence is one `approved_project_memory` item per backing citation (up to 4), and it carries `memory_review_only` uncertainty.
- Memories whose evidence is not current form a separate, tier-7 step with `stale_memory` or `unverified_memory` uncertainty naming the changed paths. Balanced selection picks it only after every other category's first step, including lexical candidates, so stale memory never displaces current evidence.
- Memory steps never alter other steps. Knowledge, code and test steps are identical with and without memory (a step limit may drop some).
- `next` items point to `atlas_memory {"operation":"get","id":…}` and to the cited source.

## MCP

`atlas_memory` is opt-in (`repogi mcp --tools atlas_memory[,…]`) and read-only (`readOnlyHint: true`). Default `full` and `compact` profiles are unchanged. Operations:

| `operation` | Arguments | Returns |
| --- | --- | --- |
| `search` | `query`, `status[]`, `include_unreviewed`, `limit` | Ranked memories with verification and citations. |
| `get` | `id` | One memory, its approved and pending revisions and verified evidence. |
| `list` | `status[]`, `type`, `after`, `limit` | Memories in ID order. |
| `history` | `id`, `limit` | Revisions and review events. |

All operations also accept `root`, `format` (`agent` or `json`) and `max_bytes` (256–32,000, default 8,000). Unknown arguments, and arguments that do not apply to the operation, are refused with `invalid_arguments`. There is no write operation. Root confinement, cancellation and response caps reuse the existing adapter.

## Storage and migration

Memories live in `.repogi/memory.db`, a separately versioned SQLite store (memory schema 1, `application_id` "RPGM") that belongs to one repository root and one memory lineage. Deleting or rebuilding `.code-intel/` never removes them. [Memory storage](MEMORY_STORAGE.md) documents the layout, the schema, migration and crash recovery, export and import, approval consistency and recovery procedures.

- Earlier source builds kept memories in index schema 5. The first explicit write (`memory migrate`, any memory write, or `repogi index`) copies them row for row into a validated new store, installs it without clobbering, and freezes the old rows (kept, never deleted) against writes by older binaries. Reads never migrate.
- Every write is one `IMMEDIATE` transaction that re-checks the state it depends on. Concurrent reviews of one revision commit exactly once. `correct --expect-version` refuses with `concurrent_update` when the memory changed since it was read. A failed write rolls back completely.
- Impact snapshot caches never contain memory records.
- **Downgrade:** the index schema stays 5. Those earlier builds cannot write the frozen legacy tables and do not read `.repogi/`. A newer memory schema is refused (`memory_store_newer`), never downgraded.

## Security boundaries

- Stored claims and repository text are data, never instructions. Memory text is never executed or interpreted. Text output escapes every stored and repository string, and JSON carries it verbatim.
- No project code, script or test runs during capture, verification, search or investigation.
- Evidence paths must be repository-relative without `..`, a root or backslashes. Files are read only through the confined, symlink-refusing primitive, and symlinked index or memory state is refused.
- Imports are validated in full (size, structure, digests, lifecycle, text) before anything is written, and imported text is never executed or followed.
- Every query is bounded: candidates, results, history, evidence per revision, files read per request (64) and conflict scans (200). Reads honour cancellation.

## Evaluation (labeled fixture)

`fixtures/project-memory` extends the investigation fixture with `docs/ARCHITECTURE.md` and a note imitating approval commands. Its `cases.json` labels 11 proposals with their review outcomes, 6 refused proposals, a supersession, 8 searches, 5 investigations, 5 cumulative source mutations with expected verification states, 6 expected conflicts and expected histories.

```sh
python3 scripts/evaluation/project_memory.py target/release/repogi --output /tmp/repogi-memory-results
```

Results on one Linux host (labels are developer-written, not independently reviewed):

| Measure | Result |
| --- | --- |
| Search precision / recall (all-terms results, 8 queries) | 1.000 / 1.000 |
| Non-approved memories returned as authoritative | 0 |
| Rejected, pending or superseded memories in default results | 0 |
| Evidence citations that cite the bytes they name | 16 / 16 |
| Verification states after mutations | 24 / 24 |
| Structured conflicts (expected 6) | 6 found, 0 unexpected |
| History sequences preserved | 5 / 5 |
| Reads that changed the database | 0 |
| Investigation memory precision / recall (5 tasks) | 1.0 on all tasks / 1.0 on four, 0.8 on one (4-memory cap) |

The evaluator also records latency, database growth and a synthetic scale run (by default 300 approved memories). These are fixture retrieval measurements only. They make no claim about coding-agent task success, token use or cost, which require a controlled, approved agent evaluation.

## Known limitations

- Approval cannot distinguish a human from an agent with unrestricted shell access (see the approval model above). Reviewer names are self-reported.
- The memory store and the code index are separate databases: approval verifies against one index read snapshot inside the memory transaction, but cannot freeze the working tree. A source edit in the milliseconds between the byte check and the commit is not detected at approval; the digest still binds the exact snapshot, and later reads report the change.
- Export digests are unkeyed integrity checks, not signatures, and the repository ID identifies a memory lineage, not a repository.
- Verification compares identities and bytes. It does not check that a claim is true, and code edits that leave the cited bytes unchanged are not detected.
- Relocation is limited to exact author IDs, identical claims, and same-kind, same-name declarations in the same file. A renamed declaration or moved file is reported as missing.
- Conflict detection is structural (assertions, decision status, evidence state). Contradictions between free-text claims are not detected.
- Search is lexical (FTS5 prefix terms). Synonyms and paraphrases are not matched.
- Investigation recall is capped at 4 memories and uses lexical task terms, so a relevant memory can be cut by the cap.
