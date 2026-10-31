# Knowledge relationships

Knowledge Relationships is an internal development milestone (v0.3), not a published release. It extends the local Markdown foundation with explicit requirements, architecture decisions and source-backed links. The workspace version remains 0.1.0. Public release preparation is deferred until the persistent-memory milestone has been validated and publication is explicitly approved. [Agent investigation](AGENT_INVESTIGATION.md) (v0.4) consumes these relationships one explicit hop at a time; it does not change their semantics.

A validated target means that an explicit reference identifies indexed evidence. It does **not** establish that code implements a requirement, satisfies acceptance criteria, or behaves correctly. Repository text remains untrusted data. Repogi never follows document instructions or fetches remote links.

## Querying

```sh
repogi index /path/to/project
cd /path/to/project
repogi knowledge trace REQ-AUTH-001 --depth 2 --format json --max-bytes 8000
repogi knowledge trace ADR-0004 --depth 3 --format text
repogi context "Modify session expiration" --scope code,docs --max-tokens 3000
```

Trace accepts an author identifier, an exact returned knowledge ID, or an exact existing code fact ID. It follows incident relationships in both directions, preserving the declared edge direction. Multiple declarations of the selected identifier return `selection=ambiguous` and their individual identities without choosing one or traversing it. Missing selectors return `selection=not_found`. `--depth 0` returns entity evidence without edges. There is no implicit indexing or migration.

JSON returns `selected_ids`, entities, relationships, original source ranges, source/target hashes, index revision, decision statuses, diagnostics, selected-source freshness and budget/completeness fields. Text renders the same classifications with citations. Historical decisions remain queryable.

## Requirement convention

Only a top-level CommonMark heading whose first token is an explicit `REQ-` identifier creates a requirement. Identifiers contain uppercase ASCII letters, digits and hyphens, with at most 128 bytes. A colon or whitespace separates the identifier from its title. Ordinary headings and examples inside fences, indented code, list items or blockquotes do not create requirements.

```markdown
# Authentication PRD
## REQ-AUTH-001: Session expiration
Sessions must expire after inactivity.
### Acceptance criteria
- Expired sessions are rejected.
### Related implementation
- [Session service](../src/auth/session.ts)
### Architecture
- [Policy](ADR-0004.md#ADR-0004)
```

A requirement spans its heading and descendants up to the next heading at the same or a higher level. The document retains the exact content; the entity records its original range. An exact, case-insensitive `Acceptance criteria` descendant heading contributes a source range for that section's text. No natural-language interpretation is attempted. An exact `Related implementation` heading classifies its links as `DECLARED_IMPLEMENTATION`: a documented claim, without functional verification. Other Markdown links are `REFERENCES`.

## ADR convention and metadata

An ADR requires an explicit `id: ADR-…` in the initial `---` front matter. File names and prose never determine its type or status.

```markdown
---
id: ADR-0004
status: accepted
supersedes: ADR-0002
relates_to: [REQ-AUTH-001, ADR-0001]
depends_on: ADR-0003
implementation: ../src/auth/session.ts
---
# Session lifetime policy
## Context
Why a policy is needed.
## Decision
Use a fixed inactivity timeout.
## Consequences
Every session consumer must respect it.
```

Supported statuses are `proposed`, `accepted`, `rejected`, `deprecated` and `superseded`. A missing status stays undeclared; an unknown status produces a diagnostic. All statuses remain historical records. ADR section entities retain the original Context/Decision/Consequences locations without inferring their meaning.

Metadata is a deliberately small, non-executing subset: unquoted scalar `id`/`status`; unquoted scalar, comma-separated or inline-list values for `supersedes`, `depends_on`, `relates_to` and `implementation`. Unknown keys are ignored. Quoted scalars, nested mappings, block lists/scalars, tags, anchors and aliases are unsupported. Malformed, duplicate recognized keys, oversized or unclosed front matter produce diagnostics and suppress metadata extraction for that block. Normal Markdown extraction still proceeds. Metadata is limited to 256 lines, 16 KiB and 2048 bytes per recognized value. Use relative paths from the document for `implementation`.

`SUPERSEDES` requires an ADR source and an explicit ADR target identifier. Multiple targets are preserved; no winner or implicit status transition is selected. Missing/duplicate targets remain unresolved/ambiguous. Cycles are diagnosed using a bounded lineage walk. An accepted historical record explicitly superseded by another accepted record, or a rejected record declaring supersession, receives a metadata-conflict diagnostic. Historical statuses are never rewritten to reconcile declarations. Arbitrary prose contradictions are outside scope.

Diagnostics are attached to document/decision entities and are visible through trace. Skipped parsing limits appear in index-run diagnostics and the existing document skipped counter.

## Entities, identity and storage

| Entity kind | Identity |
| --- | --- |
| `document` | Existing `document:<repository-relative path>` |
| `document_section` | Existing hash of document, heading hierarchy and occurrence |
| `requirement` | `requirement:<explicit ID>:<path>:<same-ID occurrence>` |
| `architecture_decision` | `decision:<explicit ID>:<path>` |
| `code_file`, `code_symbol` | Existing static fact IDs, projected on lookup |

Every identity is scoped to the database's recorded repository root. Explicit IDs are lookup keys, not unique storage constraints: duplicates are retained as distinct occurrences with diagnostics. Body edits and unrelated line shifts preserve requirement/ADR identities. Requirement title/ancestor heading changes preserve its explicit occurrence identity, while ordinary section IDs still follow heading hierarchy. A document rename changes occurrence identities; the explicit identifier remains the lookup key. Inserting another same-ID declaration can shift occurrence numbers. Deleted or excluded sources remove their extracted entities and outgoing relationships; incoming references are revalidated. Collisions never silently overwrite author declarations.

SQLite migration **4** adds `knowledge_entities(id, path, explicit_id, data)` and `knowledge_relationships(id, path, source_id, target_id, data)`, with path/source/target/identifier lookup indexes. Structured JSON carries typed fields. Entities retain source ranges, content hash, title, author identifier, decision status, acceptance-criteria ranges and diagnostics. Relationships retain source/optional target identity, literal reference, kind, evidence range, source/target hash, target path, resolution status/rule, reason and validation revision. Evidence stays in the existing document snapshot; there is no duplicated evidence table or separate graph database.

Document-owned records cascade on source deletion. Target identity deliberately has no foreign key: an absent target must become visible uncertainty rather than deleting the originating claim. Code entities are projections of existing facts, keeping the static graph authoritative and avoiding duplicate code inventories.

Schema 1/2/3 indexes migrate additively during explicit indexing. Migration remains its own transaction before the inventory transaction, matching the existing contract. Read-only older indexes stay byte-for-byte unchanged; trace requires a re-index instruction. Storage schema 4 is separate from the existing version-1 CLI response envelope. Old binaries supporting at most schema 3 must not open the new index; downgrade is unsupported.

A separate relationship extractor marker triggers a one-time document refresh for migrated indexes. Thereafter unchanged documents skip parsing. Code, document, FTS, knowledge, run report, revision and build identity share one transaction. A failed update rolls them all back. Impact snapshots remain code-only.

## Relationships and resolution

| Kind | Source evidence |
| --- | --- |
| `CONTAINS` | Document sections and explicit structured declarations |
| `REFERENCES` | A CommonMark link |
| `DECLARED_IMPLEMENTATION` | A link under `Related implementation`, or `implementation` metadata |
| `RELATES_TO` | `relates_to` metadata |
| `DEPENDS_ON` | `depends_on` metadata; distinct from static code dependencies |
| `SUPERSEDES` | `supersedes` ADR metadata, directed newer declaration to older decision |

Supported targets are repository-wide exact `REQ-…`/`ADR-…` identifiers in metadata; document-relative paths; `#REQ-…`/`#ADR-…` within an explicit document; and code paths with exact static fact identities in fragments, for example `../src/session.ts#function:src/session.ts:1:0` **only if that exact ID exists in that file**. Obtain actual IDs from this index. A fragment containing only `expireSession` never resolves by name. Ordinary heading slugs and line-number fragments are unsupported and remain unresolved. Links can use CommonMark reference definitions; their evidence range cites the link use, and the stored document retains the definition. Images are not links in this model.

Resolution percent-decodes paths/fragments, normalizes `.`/`..`, rejects escaping paths, absolute paths, backslashes, control bytes, malformed escapes and unsupported path/scheme forms, then looks up only the selected repository's indexed inventory. Encoded spaces are supported. Query strings are unsupported. Remote links/schemes are recorded as unsupported and never fetched. Symlink, ignored, generated and unsupported-language targets cannot become validated from filesystem existence alone.

`validated` means one exact indexed target; `unresolved` means none; `ambiguous` means more than one; `invalid` means rejected syntax/boundary; `unsupported` identifies remote schemes; `stale` means a previously validated target disappeared after indexing. Only unique targets have `target_id`. The validation `rule` identifies containment, explicit identifier, exact indexed path or exact identity constrained to a path. No keyword, filename-similarity, embedding or model inference creates edges.

## Freshness, traversal and context limits

When inventories change, the resolver reevaluates cached relationships against the updated snapshot without reparsing unchanged documents. A changed target acquires its current indexed hash; a removed/renamed target invalidates the old reference. Stale references remain stale until they resolve again or their source declaration is replaced. There is no inferred rename mapping. Resolution currently reevaluates the cached relationship inventory on a changed index; affected-target-only scheduling is future optimization. Indexing is not an atomic filesystem snapshot.

Trace checks at most 32 selected source/target files through the existing bounded no-follow reader. `source_freshness` describes those paths. Edge `freshness` summarizes selected-file checks independently of its indexed validation status. A live edit can make a snapshot-validated target stale at read. Further or inaccessible files remain unverified. Additions, identifier uniqueness outside the indexed inventory, other files and graph freshness remain unverified.

Trace uses deterministic breadth-first traversal, depth 0–8, at most 128 entities and 256 edges, plus bounded per-node lookups. Duplicate selection returns at most 16 declarations. Cycles terminate through visited identities. `work_limit_reached`, `depth_limit_reached` and `truncated` disclose frontiers; discovered counts and byte-omission counts do not count the unseen graph. Depth-limited output can be complete within the requested depth while reporting a further frontier. Selected entities and their direct claims receive output priority. Whole rendered output, including metadata, escaping and final newline, fits the byte cap or returns an explicit budget-too-small error. Default CLI budget is 8000 bytes, maximum 8,000,000.

Combined context (`code,docs`) expands only relevant lexical seeds: at most eight seed candidates, sixteen incident edges per seed, 32 examined edges in total, sixteen added candidates and eight source declarations from a referenced file. It follows one hop of validated explicit relationships. Added candidates have a fixed priority score of 24 and explanations containing the claim type, literal target and source citation. For file references, located symbols are identified as file contents, not verified implementations. Related requirements, acceptance text and ADR excerpts use original bytes, capped at 2000 bytes per added document candidate. Existing packing, source checks and completeness fields apply. Small budgets can still omit linked evidence. Code-only and docs-only retrieval retain their existing paths; relationship paths appear in explanations and never enter the verified static edge list.

## MCP

`atlas_knowledge_trace` is selectable only through explicit `--tools`; neither default profile adds it:

```sh
repogi mcp --root /absolute/project --profile compact \
  --tools atlas_context,atlas_knowledge_trace
```

Arguments: `selector`, optional `depth` (0–8), `root` (a configured root), `format` (`agent` or `json`), and `max_bytes` (256–32000, default 4000). Existing `atlas_*` identifiers, schemas and default tool sets remain compatible. The tool reuses root authorization, cancellation, read-only database access and whole-response budgets. No source or document instructions execute.

## Reproducible checks

```sh
cargo test -p code-intel-cli --test knowledge_relationships --test knowledge --test mcp
python3 scripts/evaluation/knowledge_relationships.py target/release/repogi \
  --output /tmp/repogi-knowledge-evaluation --runs 9
```

The labeled fixture covers four requirement occurrences, ADR history/cycles, valid code/document links, duplicate IDs, missing symbols, malformed paths and remote references. The evaluator fails on any false validated target, incorrect source citation, extraction/status mismatch, incomplete within-depth traversal, or failed incremental checks. Unit/integration tests additionally cover valid exact symbol IDs, schema 1/2/3 upgrades, transactional failure injection, symlink/root attacks, resource limits and read-only CLI/MCP compatibility. These small fixture measurements do not establish general agent, token or runtime-correctness benefits.

Knowledge entities are rebuildable source evidence, not durable memories. Future reviewed memory must retain its own approval/correction history and provenance across index rebuilds; indexing alone cannot create it.
