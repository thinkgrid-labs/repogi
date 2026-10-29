# Compact change-impact summaries (Phase 4.4)

`--summary` on the CLI, or `summary: true` on the opt-in `atlas_impact` MCP tool,
returns one row per repository-relative file. It projects the existing impact
model without changing the resolver, confirmed edges, baseline consumers or
traversal. Start here for file discovery; request evidence for selected files
before deciding what to edit or test.

```sh
code-intel impact . --base main --summary --max-bytes 8000
code-intel impact . --base main --summary --format json --max-bytes 8000
code-intel impact . --base main --evidence-file src/components/Card.tsx
code-intel impact . --base main --detail --format json --max-bytes 1000000
```

Equivalent MCP calls use the existing tool:

```json
{"base":"main","summary":true,"max_bytes":8000}
{"base":"main","summary":true,"format":"json","max_bytes":8000}
{"base":"main","evidence_file":"src/components/Card.tsx"}
{"base":"main","detail":true,"format":"json","max_bytes":32000}
```

## Compatibility and scope

Summary is explicit and defaults to false. Existing agent text, `--detail` and
structured report JSON retain their meanings, defaults, list caps and fitting
order. Summary JSON is a separate schema, selected by summary plus JSON.
Summary cannot be combined with detail or a file evidence selector. All existing
CLI budgets and MCP bounds remain in force; no profile enables impact by default.
The full and compact MCP profiles retain their tool schemas and instructions.

The implementation retains evidence until grouping or file selection, then
applies output limits. The existing per-node limit (5,000 incoming sites per
declaration and snapshot), changed-declaration limit and transitive work limits
still apply. For summary, `max_results` also caps returned file rows. It can
therefore limit traversal as well as output. Raising the byte budget cannot
recover relationships that were never traversed; rerun with appropriate work
limits or acknowledge the missing coverage.

## Reading the rows

Rows are sorted by repository-relative path. Each path appears once and may
carry several categories. A direct consumer can also be reachable transitively;
both facts are retained. To obtain disjoint review lists, subtract changed and
direct files from the transitive set. Categories never imply compilation errors.

| Category | Meaning |
|---|---|
| `changed`, `declaration` | Git file change; changed declaration identity, kind and change type |
| `direct_call`, `direct_reference`, `direct_extends`, `direct_jsx` | Confirmed static use of a changed declaration, including baseline uses of removed declarations |
| `transitive` | Confirmed upstream declaration reached at the requested depth |
| `import_only` | Confirmed import without a direct use in that file/snapshot |
| `re_export`, `module_importer` | Confirmed re-export or file import; these are separate from direct uses |
| `binding_gained`, `binding_retargeted`, `binding_lost` | Binding changes; a lost binding needs verification and is never promoted into a confirmed new dependency |
| `possible` | Unverified same-name site or unanalysable file mentioning a changed declaration |
| `unanalysable` | Syntax-error file in either snapshot, regardless of a matching mention |

Row numbers count evidence entries, not executions or distinct runtime effects.
`total_by_category` counts distinct files, including a `direct` union count for
all four direct categories. Counts for different categories overlap. The syntax
error row count may include both snapshots; the file row is still unique.

## Completeness and uncertainty

These fields deliberately describe different axes:

| Field | Contract |
|---|---|
| `direct_files_complete` | Conservative claim that all confirmed direct files are returned within the supported analysis. False if analysis/work/output was limited, snapshots are unverifiable, or either snapshot has skipped, syntax-error or diagnostic coverage. Possible same-name effects alone do not invalidate enumeration of confirmed edges. |
| `traversal_complete` | No analysis/list/work limit prevented traversal to requested `depth`, within the analysed static graph. Output omissions do not change whether traversal ran. |
| `analysis_limited` | At least one work, changed-declaration, module-importer or inherited report list limit was reached. |
| `truncated` | At least one grouped file row was omitted by `max_results` or byte fitting. This concerns summary output, independently of analysis limits. |
| `total_files`, `omitted_files` | Total discovered grouped rows and excluded rows; shown + omitted = total. Totals are not estimates of undiscovered findings beyond work limits. |
| `total_by_category`, `omitted_by_category` | Distinct file totals and omissions for each category, including the direct union. |
| `possible_unverified` | Possible sites or lost bindings remain. This stays true even if their file rows are omitted. |
| `snapshots_fresh` | Both snapshots passed verification with no completeness-blocking source/Git limitation. False means freshness/verification is unproven, not necessarily that a stale file was observed. |
| `unanalysable_files` | Sum of syntax-error and skipped-file counts across base and head, not a unique-file count. Skipped samples are never presented as an exhaustive list. |
| `coverage_diagnostics` | Indexing/materialisation diagnostics across both snapshots. Full messages remain in the detailed report. |
| `complete` | All preceding analysis/coverage/freshness checks pass, no unverified effects remain, and no file rows were omitted. |

Duplicate removal alone never proves completeness. All rows may fit while
coverage remains unknown: Umami contains syntax-error and skipped sources, so
its summaries conservatively remain incomplete even with perfect labelled file
recall. The unsupported profile is printed in every summary: JavaScript and
other unsupported extensions, packages/excluded directories, and dynamic runtime
behavior. Enumeration of confirmed dependencies does not establish runtime
safety, exhaustive effects, or TypeScript compilation breakage. Compilation
claims require compiler evidence, which this engine does not produce.

## Evidence on demand

`evidence_file` / `--evidence-file` selects an exact repository-relative path,
including baseline-only files. Selection happens before per-site caps and byte
fitting. It uses the existing detailed renderer and includes relevant changed
declarations, direct and transitive evidence, binding changes and possible sites.
Global coverage and limitations remain visible. Selection is explicitly labelled
as a partial scope; it cannot claim global completeness. Unrelated findings are
intentionally excluded and are discoverable through summary totals.

The existing snapshot cache is reused; graph comparison is recomputed on each
request. There is no second MCP tool, result cache, pagination protocol or hidden
continuation state. Each response describes the exact currently analysed
`base_digest` and `head_digest`. A working-tree edit causes a new digest and new
analysis; pin base/head commits when successive evidence requests must describe
the same revisions. Compare summary digests after edits.

If the selected evidence still exceeds the budget, omissions are counted by the
existing fitter. Increase the CLI byte budget or narrow the review. Summary drops
whole rows deterministically and retains global/category totals and uncertainty.
If even fixed summary metadata cannot fit, it returns an explicit error requesting
a larger budget; it never returns partial JSON or silently clips paths.

## First-pass essentials and deferred details

Keep the changed declarations, affected paths, relationship categories, counts,
snapshot identities, uncertainty and completeness axes in the first response.
Source lines/byte spans, enclosing declaration IDs, binding chains, transitive
paths, export/signature/body details and diagnostic messages remain accessible
through detailed evidence. They are valuable for verification but repeating them
for every site is unnecessary for initial file discovery.

The recorded-response audit partitions bytes into paths, residual per-site fields,
binding chains, source locations, transitive paths, metadata and formatting/JSON
keys. Paths embedded in chains belong to their chain category. Reference tokens
for fragments are counted separately and do not add to whole-response BPE counts.
The report includes the exact audit method and every recorded response.
