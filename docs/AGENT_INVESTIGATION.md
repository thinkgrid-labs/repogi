# Agent investigation

Agent Investigation is an internal development milestone (v0.4), not a published release. The workspace version remains 0.1.0. Public release preparation is deferred until the persistent-memory milestone has been validated and publication is explicitly approved.

`repogi investigate` turns a development task into an ordered, evidence-backed **investigation plan**: where to start reading, which requirements and decisions govern the behavior, which static consumers and test candidates to look at, and what is unresolved, stale or missing. Every step cites indexed repository evidence and names a bounded next read or query.

Repogi is **not** an autonomous coding agent. An investigation never edits files, runs project code, scripts or tests, re-indexes, migrates the database or stores the plan. It prepares evidence for an external agent or developer, who reads the current source before changing anything.

## Querying

```sh
repogi index /path/to/project            # explicit; investigate never indexes
cd /path/to/project
repogi investigate "Modify session expiration"
repogi investigate "What implements REQ-AUTH-001?" --max-steps 4
repogi investigate "Review requirement impact" --selector REQ-AUTH-001 --format json
repogi investigate "Investigate token refresh failures" --depth 3 --max-bytes 20000
```

| Option | Default | Range | Meaning |
| --- | --- | --- | --- |
| `--format` | `text` | `text`, `json` | JSON is the full contract; text is a compact projection of the same plan. |
| `--max-bytes` | 12000 (text), 24000 (JSON) | 1–8000000 | Cap on the whole rendered response in UTF-8 bytes. |
| `--max-steps` | 8 | 1–16 | Steps returned after selection. |
| `--depth` | 2 | 1–3 | Upstream static-consumer hops from each code target. |
| `--selector` | none | up to 8 | Exact knowledge entity ID, author ID (`REQ-…`, `ADR-…`) or code fact ID. Repeatable. |

`REQ-…` and `ADR-…` tokens written in the task itself (uppercase, as indexed) are also treated as selectors. Names are never resolved by similarity: a selector that matches nothing is reported as `selector_not_found`, and one that matches several entities is reported as `ambiguous_selector` with every match shown.

## How evidence is selected

The pipeline is deterministic and uses only the existing index. There is no model call, embedding or vector search.

1. **Task normalization.** The original task is preserved. Words are lowercased; common filler words are dropped; a small suffix trim turns inflections into FTS5 prefixes (`expiration` → `expir*`). Keyword cues (`change`, `bug`, `knowledge`, `impact`, `tests`) only reorder whole plan phases. They are not semantic intent detection. A task with no searchable words and no selectors is classified `unsupported`.
2. **Discovery.** Code declarations come from the same lexical identifier ranking as `repogi context` (top 6). Markdown sections come from the knowledge FTS5 index; a section must contain every search term, or, when no section does, at least two thirds of them (minimum two). A matched section inside an explicit requirement or ADR seeds that entity.
3. **Explicit relationships (one hop).** From each seed requirement/decision (top 6), validated explicit claims add their targets: code files (expanded to at most 8 declarations), code symbols, and related requirements/decisions. Claims that explicitly target lexically found code add their requirement or decision as governing evidence. Unresolved, ambiguous, invalid, unsupported and stale claims are recorded as uncertainty and never gain a target.
4. **Ranking of code targets.** Explicit selection, then claim origin (explicit seed before lexical seed), then corroboration (distinct validated claims plus a lexical match), then how many search terms prefix the name's components, then lexical score. The top 4 become code targets.
5. **Bounded traversal.** Each code target gets an upstream walk over resolved `CALLS`, `REFERENCES` and `EXTENDS` edges (the same walk as `atlas_dependencies`), up to `--depth`. Consumers in files matching the test conventions used by affected-test discovery (`*.test.*`, `*.spec.*`, `__tests__/`) become advisory test candidates. Same-name sites without a resolved target are reported as `unconfirmed_sites`. The first code target also lists up to 8 direct resolved dependencies. Superseded decisions point at their explicit successors.
6. **Steps, selection and fitting.** Evidence is grouped into steps (see below), ordered by phase, evidence tier and rank. When there are more steps than `--max-steps`, selection is **balanced**: the first step of every category before any second step, with lexical-only candidates last. If the rendered plan exceeds `--max-bytes`, the most recently selected step is dropped first, so category coverage survives.
7. **Freshness.** Selected source files are hashed and compared with the index (see below).

Affected-test discovery (`impact --tests`) compares two Git states and maintains a snapshot cache. A task has no change set, so investigation does not call it. Its test candidates are static links only.

### Plan phases

Default order: selected entities → implementation → requirements and decisions → consumers → dependencies → test candidates → uncertainty → additional lexical candidates. Keyword cues promote a whole phase: knowledge words (`why`, `decision`, `requirement`, `ADR`, …) or an author-ID selector move requirements and decisions right after the selected entities; impact words (`depends`, `callers`, `affected`, …) move consumers after implementation; test words move test candidates after implementation.

## Evidence classes

Evidence classes are reported separately and are never folded into one confidence score.

| Class | Meaning | Does **not** mean |
| --- | --- | --- |
| `explicit_selector` | The caller named this exact indexed identity. | That it is relevant to the task. |
| `verified_static_relationship` | A resolved typed TypeScript edge in the indexed snapshot. | That a change breaks the consumer, or runtime behavior. |
| `validated_knowledge_reference` | An explicit document claim whose target identity validated in the index. | That the code implements the requirement correctly. |
| `recorded_intent` | An author-declared requirement or ADR, with its source range. | That the code matches it. |
| `advisory_test_candidate` | A resolved static consumer located in a test-convention file. | That the test runs, passes or covers the behavior. |
| `lexical_candidate` | FTS5 name/text discovery. | Any relationship. |
| `unresolved_reference` | Unresolved, ambiguous, invalid or unsupported claims, and same-name sites with no resolved target. | A link. |
| `stale_reference` | A claim whose indexed target was deleted or changed. | A current target. |

## Response structure

The JSON response (`schema_version` 1, `kind: "investigation"`) contains:

- `repository_root`, `index_revision`, `notice`.
- `task`: `original`, `classification` (`supported`, `ambiguous_selector`, `no_matching_evidence`, `unsupported`), `cues`, `terms`, `search_terms`, `selectors`.
- `steps[]`: `id` (content-derived, stable for the same action and targets), `order`, `action`, `category`, `objective`, `targets` (IDs), `omitted_targets`, `evidence` (IDs), `uncertainty[]` (`code`, `detail`, `evidence`), `next[]` and `priority` (`tier`, `rank`, `rationale`).
- `targets[]`: `id`, `kind`, `role` (`implementation`, `requirement`, `decision`, `document`, `consumer`, `dependency`, `test`), `name`, optional `explicit_id`/`decision_status`, original `location` (path, bytes, lines) and `freshness`.
- `evidence[]`: a shared, deduplicated ledger. `id` is derived from the evidence content, so the same evidence keeps its ID across runs and budgets. Each item has `class`, `subject`, `rule`, `citation`, optional `relationship` (`id` for knowledge claims, `kind`, `source_id`, `target_id`, `target_text`, `status`) and `depth`, and `freshness`. Several steps can cite one item without repeating it.
- `uncertainty[]`: request-level findings such as `selector_not_found`, `unsupported_task` or `no_matching_evidence`.
- `freshness`, `budget` and `completeness` (below).

Step actions: `inspect_implementation`, `review_requirement`, `review_decision`, `review_document`, `inspect_consumers`, `inspect_dependencies`, `review_test_candidates`, `resolve_uncertainty`, `review_lexical_candidates`.

`next` entries are built only from indexed identities and locations: `read` (repository-relative path and line range), `atlas_references {id}`, `atlas_dependencies {id, direction, depth}` and `atlas_knowledge_trace {selector, depth}`. The CLI equivalents are reading the file, `repogi knowledge trace`, and the MCP navigation tools. Objectives are fixed templates filled with identifiers and kinds. Document text is never copied into a step, and nothing in a document can add a step, a tool or an argument.

### Uncertainty codes

`behavior_unverified`, `lexical_only`, `historical_decision`, `superseded_decision`, `unresolved_reference`, `ambiguous_reference`, `invalid_reference`, `unsupported_reference`, `stale_reference`, `unconfirmed_sites`, `static_scope_only`, `tests_not_executed`, `stale_source`, `unverified_source`, and request-level `selector_not_found`, `ambiguous_selector`, `task_terms_truncated`, `unsupported_task`, `no_matching_evidence`.

## Example (fixture output)

Actual text output from the labeled fixture `fixtures/agent-investigation` (abridged; the repository root is replaced with `/repo`):

```text
Investigation r1 · /repo · balanced ranking
task: "Modify session expiration" · supported · cues [change] · search terms [session, expir] · selectors []
Advisory investigation plan from indexed evidence. Repogi executed, modified and verified nothing: …
11195 / 12000 bytes · 8 of 17 step(s) · depth 2 · complete within limits: false
limits: byte truncated false; step limit true; work limit false (144 / 2048 rows); candidate limit false; depth frontier true; unresolved findings 1; omitted lexical candidates 4

1. inspect_implementation [implementation; tier 3] Inspect function `isSessionExpired` as a candidate implementation location
   target: isSessionExpired · function:src/auth/session.ts:110:247 · src/auth/session.ts:5-7 [matched_at_read]
   evidence: ev-2ba9c026c435 ev-129c527bdd49 ev-4844d23276aa
   uncertain: behavior_unverified: an explicit claim validates this location's identity, not that it implements the requirement correctly
   next: read src/auth/session.ts:5-7 | atlas_references {"id":"function:src/auth/session.ts:110:247"}

3. review_requirement [knowledge; tier 3] Review requirement REQ-AUTH-001 (recorded intent)
   target: REQ-AUTH-001: Session expiration · requirement:REQ-AUTH-001:docs/PRD.md:1 · docs/PRD.md:3-12 [matched_at_read]
   next: read docs/PRD.md:3-12 | atlas_knowledge_trace {"depth":1,"selector":"REQ-AUTH-001"}

5. inspect_consumers [consumers; tier 2] Inspect 3 confirmed static consumer(s) of `isSessionExpired` within depth 2
   target: requireSession · function:src/api/middleware.ts:116:268 · src/api/middleware.ts:4-6 [matched_at_read]
   target: refreshToken · function:src/auth/token.ts:159:367 · src/auth/token.ts:6-11 [matched_at_read]
   uncertain: static_scope_only: dynamic, runtime, HTTP and non-TypeScript consumers are outside the static graph
   uncertain: unconfirmed_sites: 1 same-name site(s) have no resolved target and are not counted as consumers

evidence:
ev-129c527bdd49 validated_knowledge_reference · docs/PRD.md:11-11 [matched_at_read] · exact_indexed_repository_path; explicit DECLARED_IMPLEMENTATION claim; target identity only · requirement:REQ-AUTH-001:docs/PRD.md:1 --DECLARED_IMPLEMENTATION--> file:src/auth/session.ts (validated)
```

For a requirement whose references do not resolve, the plan contains no implementation step:

```text
1. review_requirement [selected; tier 1] Review selected requirement REQ-AUDIT-001 (recorded intent)
   uncertain: unresolved_reference: explicit DECLARED_IMPLEMENTATION claim to "../src/audit/writer.ts" has no validated target
   uncertain: ambiguous_reference: explicit DECLARED_IMPLEMENTATION claim to "#REQ-DUP-001" has no validated target
2. resolve_uncertainty [uncertainty; tier 7] Resolve 3 unresolved, ambiguous, invalid, stale or unconfirmed finding(s) before relying on them
```

## MCP

`atlas_investigate` is opt-in. No profile lists it, and existing tool IDs, schemas and default profiles are unchanged:

```sh
repogi mcp --root /abs/path/to/project --tools atlas_context,atlas_references,atlas_investigate
```

```json
{"task":"Modify session expiration","selectors":["REQ-AUTH-001"],"max_bytes":12000,"max_steps":8,"depth":2,"format":"json"}
```

`task` is required (at most 8192 bytes). `max_bytes` defaults to 12000 and accepts 256–32000. `format` is `agent` (the text projection) or `json`. `root` follows the usual rules: optional with one configured root, required with several, and it must be an absolute configured root. Unknown arguments are rejected. The tool opens the index read-only and never indexes. It registers for cancellation like the other tools. The definition carries `readOnlyHint: true`.

## Limits and work bounds

| Bound | Value |
| --- | --- |
| Task | 8192 UTF-8 bytes, no control characters except tab/newline; 16 content words searched |
| Selectors | 8, each up to 1024 bytes |
| Lexical code candidates | 6 (more are counted in `omitted_lexical_candidates`) |
| Document sections | 8 |
| Knowledge seeds / edges per seed | 6 / 32 |
| Declarations per claimed file | 8 |
| Code targets walked | 4 |
| Graph nodes / static links per request | 64 / 128 (walk per node: 128 edges, as navigation) |
| Unconfirmed sites per target | 8 |
| Shared evidence ledger | 96 items |
| Targets shown per step | 8 (`omitted_targets` counts the rest) |
| Indexed rows read per request | 2048 (`work_used`, `work_limit_reached`) |
| Selected-file freshness checks | 32 files |

### Completeness

`complete_within_limits` is true only when no step was omitted (by `--max-steps` or bytes), no work or candidate limit was reached, no depth frontier remains and no source was unsupported. It never claims that lexical discovery was exhaustive, and unresolved findings are counted separately in `unresolved_findings`. Further fields: `byte_truncated`, `step_limit_reached`, `omitted_steps`, `work_limit_reached`, `candidate_limit_reached`, `depth_frontier` (an unvisited edge was observed past `--depth`, or frontier checks were capped), `omitted_categories` and `unsupported` (for example an index whose knowledge layer is outdated). Counts of unvisited graph regions are never estimated.

## Freshness

All queries in one investigation read one SQLite snapshot (the read-only connection holds a read transaction), and the response names its `index_revision`. Source files of returned targets and citations are then hashed, up to 32 files, with the existing no-follow reader. Each target and evidence item reports `matched_at_read`, `stale`, `missing` or `unverified`. A step whose target changed gets `stale_source`, and its `read` suggestion drops the indexed line range, so a stale location is never offered as a current verified range. Historical evidence stays visible and labelled. Whole-graph and inventory freshness (added, removed or other files; resolution settings) are always reported as unverified. Use `repogi status` or `atlas_status` for a full freshness check, and `repogi index` to refresh.

## Evaluation (labeled fixture)

`scripts/evaluation/agent_investigation.py` runs seven labeled tasks (feature change, bug, requirement, architecture, impact, incomplete evidence, tests) against `fixtures/agent-investigation`, plus stale, deleted-target and injection cases and a generated medium repository:

```sh
python3 scripts/evaluation/agent_investigation.py target/release/repogi --output /tmp/repogi-investigation --runs 9
```

Hard failures: any false confirmed relationship, any citation whose bytes or lines do not match the file, nondeterministic output, budget overruns, any change to the fixture tree, unreported stale or deleted targets, and injected document text appearing in output. Results from the development run on one Linux host (median of 5 runs; full plans):

| Task | Precision | Recall | Required categories | `context --scope code,docs` precision / recall |
| --- | --- | --- | --- | --- |
| Feature change | 1.000 | 1.000 | 4/4 | 0.615 / 0.714 |
| Bug investigation | 1.000 | 1.000 | 3/3 | 0.706 / 0.800 |
| Requirement (`REQ-AUTH-001`) | 1.000 | 0.800 | 3/3 | 0.571 / 1.000 |
| Architecture (SQLite) | 1.000 | 1.000 | 2/2 | 0.500 / 1.000 |
| Impact (service consumers) | 0.875 | 1.000 | 2/2 | 0.533 / 0.500 |
| Incomplete evidence (`REQ-AUDIT-001`) | 1.000 | 1.000 | 2/2 | 0.000 / 0.000 |
| Tests (session timeout) | 1.000 | 1.000 | 2/2 | 0.667 / 1.000 |

Across all tasks: 0 false confirmed relationships out of 50 confirmed evidence items, 113/113 citations correct, expected uncertainty reported in 7/7 tasks, deterministic output, budget compliance at 1500–32000 bytes, and an unchanged fixture tree. With `--max-steps 3`, balanced selection kept equal or higher recall and category coverage than evidence-tier-only selection in every task.

These are small hand-labeled fixtures, labelled by the developers. They show that the contracts hold on known cases. They do not show agent task success, token or cost savings, or quality on other repositories.

## Known limitations

- Discovery is lexical. Prose that names code differently can miss it, and a lexical match is never a relationship. Only TypeScript/TSX and Markdown are indexed.
- Task cues are keyword lists, not intent understanding. The suffix trim is not a linguistic stemmer.
- Knowledge expansion is one explicit hop from each seed. Deeper chains need `repogi knowledge trace`.
- A claim that targets a file is expanded to at most 8 of its declarations; which of them matter is decided by lexical corroboration.
- Test candidates are static consumers in conventional test files. Runner configuration, mocks, dynamic imports and HTTP-level tests are not evaluated, and nothing is executed.
- Consumers are static and TypeScript-only. Runtime dispatch, dependency injection, HTTP and other languages are outside the graph.
- Selected-file checks are point-in-time, cover at most 32 files and do not certify the rest of the repository.
- Titles and identifiers in `targets[].name` and `rule` fields are repository data. They are escaped and capped at 160 bytes, but they remain untrusted text.
- Plans are not stored. An investigation is not a reviewed project memory.
