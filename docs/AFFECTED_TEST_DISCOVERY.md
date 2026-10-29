# Affected-test discovery and verification planning

Use `impact --tests` for a read-only view of probable affected tests:

```sh
repogi impact --base HEAD --tests
repogi impact --base main --head feature --tests --format json
repogi impact --base HEAD~1 --head HEAD --tests --detail
repogi impact --base HEAD --tests --test-dir integration-tests
repogi impact --base HEAD --tests --evidence-file src/lib/format.test.ts --detail
```

The output recommends existing probable test files, gives static evidence and
uncertainty, and identifies changes without confirmed test linkage. Repogi never
runs these tests, project scripts, configuration code or dependency installers.
It emits no test-runner commands. Passing a recommended selection does not prove
safety, and unlisted tests may still be necessary.

The separate CLI artifact, `impact --manifest --format json`,
composes this same discovery with Git/declaration/affected-code evidence from one
comparison. It preserves individual candidates, uncertainty and original proof
semantics; see [VERIFICATION_MANIFEST.md](VERIFICATION_MANIFEST.md).

## Architecture

The retrieval crate's `tests::compare` returns a reusable structured plan. The CLI
renders it through `tests::respond`. This layer reuses `impact::inspect_comparison`
to inspect the same open baseline/head stores and sources, before the existing
final working-tree freshness check. Snapshot building, declaration matching,
binding changes, direct consumers, transitive traversal and Git limitations are
the existing impact engine's responsibilities. There is no independent resolver,
second snapshot build per candidate, new database schema or persisted test index.

Discovery builds a lightweight catalog for each snapshot, then streams each
snapshot's semantic edges once, parsing test edges. Impact-reached declarations
seed terminal queries into probable test files. This includes calls inside
module-scope test callbacks, which do not have enclosing declarations in the
original transitive result. It does not restart an independent graph traversal.
The same depth and work limits still apply. A depth-2 path can therefore be:

```text
formatLongNumber -> renderNumberLabels -> charts.test.ts:call
```

Each snapshot resolves its own IDs; source position/name/kind matching is confined
to that snapshot and ambiguous matches are refused. IDs are not exported as
cross-revision identities. One deterministic shortest available proof is retained
per candidate; `linked_changes` lists confirmed declaration origins, including
targets of changed usage bindings. It does not assert that each target's text
changed: textual declaration changes are listed separately in `changed`.
Detailed output includes the selected proof's binding chain; the default text
and JSON views omit it.
The `via` prefix retains the existing impact engine's declaration-name evidence;
the root and terminal target have explicit repository-relative declaration keys.
Paired `both` sites follow the existing impact model's head-location convention.

## Inventory and conventions

Default filename recognition is case sensitive:

- `*.test.ts`, `*.test.tsx`, `*.spec.ts`, `*.spec.tsx`.
- Corresponding test/spec filenames with other extensions are recognized as
  unsupported when semantic indexing is unavailable, including JavaScript tests.
- Files in `__tests__/` are probable tests by directory convention.
- Additional literal directories come from repeated `--test-dir` arguments or
  a package manifest's literal `directories.test`, relative to that manifest.

There is no automatic assumption that every file under `tests/`, `test/` or
`integration/` is an executable test. Configure those directories when desired;
test/spec suffixes inside them are already recognized. A configured directory can
contain helpers, so runner membership remains unverified even with a static link.

| Per-snapshot class | Meaning |
|---|---|
| `supported_indexed` | Test/spec filename, indexed TypeScript/TSX without reported syntax/semantic extraction failure |
| `possible_convention` | Indexed file identified through `__tests__` or an explicitly configured test directory |
| `recognized_unindexed` | Recognized TypeScript test without usable semantic indexing, including ignored, oversized, symlinked or syntax-error files |
| `unsupported_or_excluded` | Unsupported test extension or a built-in excluded path such as generated output |

Inventory uses Git tree paths and the indexer's existing ignore/exclusion and
no-follow walk. Tracked excluded/ignored locations can be named without reading
their source. Ignored untracked files and excluded directory contents are not
exhaustively enumerated; this is an explicit limitation. Unsafe path names are
counted as an inventory limitation rather than followed or printed unescaped.

`package.json` is parsed as bounded JSON data for `directories.test`, runner
dependency hints and presence of a test script. Script bodies are neither
interpreted nor executed. Jest/Vitest/Playwright configuration files are recorded
and fingerprinted as data, with `interpreted=false`: their arbitrary includes,
excludes, projects, setup, environments and executable expressions are unsupported.
Embedded Jest settings are likewise reported as unsupported. Parent-directory
references and glob expressions in `directories.test` / `--test-dir` are rejected
or reported unsupported. No runner or config module is imported.

Committed manifest/configuration bytes come from immutable Git blobs, read in a
batch. Working-tree reads reject symlinked parents, paths outside the root and
files over 1 MiB, and honor exclusions. A second working-tree inventory digest
check detects changes during discovery; the engine's final source freshness check
also runs after discovery. Inventory digests include observed names, classes and
configuration data fingerprints. Unsupported source content is not semantically
verified merely because its filename was inventoried.

## Three categories and the advisory plan

**Confirmed static linkage** requires a resolved `CALLS`, `REFERENCES` (including
JSX), or `EXTENDS` use, or a path composed of those supported relationships.
Direct tests precede transitive tests, ordered by depth then file path. Evidence
includes the changed declaration, snapshot, depth, line, terminal target, rule
and available path. A resolved gained/retargeted usage binding can also establish
static linkage to a binding change; it does not assert that the target's body
changed or that compilation fails.

**Possible relevance** includes import-only/re-export links, changed module
importers, unresolved sites, lost bindings, changed tests, colocation and source/
test basename matches. Basename matching is never a resolver. A file with a
separate confirmed proof can retain heuristic reasons without deriving its
confirmation from them. Lost bindings remain unverified unless independent
baseline usage of a changed/removed declaration supplies a confirmed proof.

**Unknown verification coverage** lists recognized tests without supported
semantic coverage. It remains visible alongside unsupported configuration and
global limitations for dynamic dispatch/imports, external packages and excluded
sources. Unknown coverage is never represented merely by an empty test list.

The plan also lists changed declarations without confirmed test linkage at the
requested depth. Module/configuration-only changes without confirmed tests receive
an explicit gap. These are discovery gaps, not proof that behavioral tests do not
exist. Pure module trivia produces no test recommendations; inventory/unknown
coverage is still reported for that valid no-impact result.

## Baseline/head correctness

Baseline and head catalogs are merged by repository-relative path. Per-snapshot
classes remain separate. A removed function can retain baseline tests; added and
removed test files can both be recommended. Renames, alias edits and barrel edits
reuse the impact engine's matching and binding evidence. Presence only in the
baseline means **not observed in the changed-state inventory**, not automatically
deleted: a test excluded by new ignore rules remains visible as unindexed in head.
Pure file moves with unchanged logical declarations can yield module/import
possibilities rather than an invented changed-declaration relationship.

## Completeness, output limits and evidence

The JSON plan includes Git state provenance, snapshot and inventory digests,
changed declarations, candidates, unknown tests, gaps, catalog counts,
configuration observations, limitations and explicit completeness axes:

| Field | Meaning |
|---|---|
| `direct_files_complete` | Inherited conservative impact enumeration claim within its supported profile |
| `traversal_complete` | Impact traversal completed to the requested depth; independent of output fitting |
| `snapshots_fresh` | Source/Git verification passed and the observed inventory/configuration did not change during discovery |
| `inventory_complete` | Within configured conventions, no observed test has missing/unsupported semantics, unsupported config, unsafe paths or unstable inventory |
| `static_links_complete` | Supported static test enumeration passed impact, source, inventory and freshness checks; false when confirmed rows are omitted or scope is selected |
| `possible_unverified` | Possible tests or inherited unverified effects remain, including findings omitted from output |
| `source_coverage_unknown` | Source diagnostics or unsupported/unindexed observed tests remain |
| `complete` | Supported enumeration checks pass, no possible effects remain, no output was omitted, and the result is not a selected subset |

These are enumeration claims within the stated profile. They never mean all
affected tests were found, complete behavioral coverage, runtime safety, compiler
breakage or permission to omit other tests. A complete supported enumeration can
coexist with a gap saying no confirmed test consumer was found.

`totals` records all discovered entries before rendering. `omitted` counts whole
entries dropped by `max_results` or byte fitting. `truncated` describes output
omission; `analysis_limited` separately records impact/work limits. Totals do not
estimate findings beyond a stopped traversal. The default plan gives inventory
counts and unknown rows; `--detail` includes the full catalog and binding chains.
`inventory_included=false` explicitly marks this default projection.

`--tests --evidence-file <test-path> --detail` keeps candidates/unknown/catalog
rows for that exact observed test. Global counts, changed declarations and
limitations remain available; `excluded_by_selection` counts intentional scope
exclusions separately from output truncation. Selected results cannot claim global
completeness. Output fitting preserves confirmed candidate order and drops whole
rows with counts. If fixed metadata alone cannot fit, Repogi returns an explicit
budget error instead of broken JSON or silently clipped paths.

CLI and MCP rendering follow these rules:

- Possible candidates are ordered by signal strength, then path: rows with static
  evidence (import-only, re-export, binding change), other non-heuristic signals,
  basename matches, then colocation-only rows.
- Default text lists the stronger signals individually and
  summarizes weak heuristics: basename-only candidates (possibly also colocated)
  in one counted line, and colocation-only candidates by directory. Summaries
  never establish a target or a confirmed relationship. Each group counts the
  candidates still represented after result/byte fitting; `totals` remains global
  and `omitted` counts candidates removed by fitting. Grouping itself is not an
  omission and does not set `truncated`. JSON, `--detail`, and selected-test
  evidence retain individual paths and original reasons. No candidate is removed
  from the underlying plan by grouping.
- Fitting drops whole rows in this order: inventory, configurations, unknown,
  gaps, possible (weakest first), limitations, changed, confirmed. Global
  limitations therefore outlast per-file rows. Each clipped text section ends with
  `… N omitted`.
- The default text gives a confirmed row only its linkage reasons; heuristic hints
  merged into it stay in JSON `reasons` and in `--detail` text. Rows observed in
  one snapshot only are marked `(baseline only; not observed in the changed state)`
  or `(changed state only)`.
- Text sections are ordered: changed, confirmed, possible, gaps, unknown. The
  footer names both the MCP arguments and the CLI flags.
