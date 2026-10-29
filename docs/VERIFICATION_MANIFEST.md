# Verification manifest

## CLI and output policy

```sh
repogi impact . --base HEAD --manifest --format json
repogi impact . --base main --head feature --manifest --format json
```

Manifest mode requires explicit JSON. It conflicts with `--tests`, `--summary`,
`--detail` and `--evidence-file`. Literal `--test-dir` values are supported with
either `--tests` or `--manifest`. Ordinary impact/test/summary defaults stay
unchanged. The CLI artifact and MCP transport share the same manifest schema.

The manifest default and maximum output budget are 8,000,000 UTF-8 bytes. A
smaller explicit `--max-bytes` is an integrity boundary: if the entire artifact
does not fit, the command fails with empty stdout and an error on stderr. No
finding is dropped to make an artifact fit. Analysis limits still apply:
`--depth`, `--max-results` (traversal/module-importer work) and
`--max-declarations`, plus the engine's 5,000 incoming-sites-per-node bound.
Their effect is recorded separately from output omissions. Discovered evidence
is retained even when those limits make discovery non-exhaustive.

Indexed-source digests alone omit JavaScript/JSON contents. The shared file
comparison also retains BLAKE3 digests of its sorted path-to-Git-blob-OID maps.
Manifest mode rehashes the relevant working files at the end to verify identity. This is a read-only integrity pass, not another diff
or graph analysis. Modes, ignored files and unsupported filesystem states remain
subject to the existing static profile; these digests are not whole-disk images.

## Payload contract

| Section | Content |
|---|---|
| `schema_version` | 1; incompatible semantic/canonicalization changes require a bump |
| `manifest_id` | `blake3:` plus the 256-bit content digest, excluding this field |
| `analysis` | Repogi/build/IR compatibility, requested refs, effective limits, static profile and inventory conventions |
| `snapshots` | commit/worktree kinds, resolved commit/tree where applicable, indexed-content, resolution and inventory digests, Git metadata fingerprint and freshness status |
| `changes` | Git file changes; declaration identity, change flags and separately labelled base/head provenance; module/configuration changes |
| `affected_code` | per-file categories and selected direct/transitive/import/re-export/possible proofs, module importers and binding changes |
| `verification` | original confirmed/possible test candidates, original reasons, selected shortest proofs, unknown tests, configurations and structured gaps |
| `coverage` | per-side syntax/skipped/diagnostic coverage, unsupported profile, limitations and behavioural coverage explicitly unknown |
| `completeness` | inherited axes, work limits, exact known analysis omissions, output omissions and intentional scope exclusions |

Declaration evidence keys identify the declaration-change record. Snapshot
symbol IDs are retained only inside explicit base/head/source evidence and are
never used as persistent cross-snapshot identities. Position-paired identities
remain uncertain. A test's `linked_changes` may contain several changed
declarations; its selected shortest proof does not pretend to prove every link.
For gained/retargeted use bindings, linked origins can name unchanged target
declarations: the change is the binding, not necessarily the target's text.
The selected proof's rule and `affected_code.binding_changes[].effect` retain
that distinction; a lost binding is never upgraded to confirmed new usage.
`affected_code.relationships[].changed_declaration` references the corresponding
declaration-change `evidence_key`. These keys hash the change record, including
its explicitly scoped source locations; they do not identify source text across
arbitrary revisions. The containing manifest supplies snapshot identity.

`snapshots.base/head.file_manifest_digest` hashes the compact JSON of the
comparison's sorted repository-relative path-to-raw-Git-blob-OID map. This
distinguishes relevant JavaScript/JSON edits as well as indexed-source changes.
It supplements `source_digest`, `test_inventory_digest` and `resolution_digest`.
`file_inputs_verified` records the final working-tree rehash; commit inputs use
their immutable trees. `git_metadata_verified` compares before/after HEAD and
index fingerprints. These endpoint checks are not an atomic filesystem lock.
Requested refs are resolved to the recorded commits; the manifest identifies
those exact commits rather than promising that a mutable branch name stays put.

Diagnostics exported in coverage contain relative paths and a generic category;
arbitrary exception messages are excluded because they can contain host paths
or source contents. Existing detailed impact output provides operational
diagnostics separately. No source snippets, runner scripts or environment
values are exported.

## Canonicalization and identity

The canonical payload is compact UTF-8 JSON, without a BOM, indentation or final
newline. Object keys are ordered lexicographically. Set-valued arrays are
recursively canonicalized, sorted by their compact canonical JSON bytes and
deduplicated. `via` and `binding_chain` arrays preserve traversal/proof order.
Integers, booleans, null and strings use serde_json serialization; the manifest
has no floating-point values. Strings preserve Unicode and case, without NFC
normalization. Paths come from the engine as repository-relative `/` paths;
there is no absolute-root prefix or machine path in the payload.

BLAKE3 hashes those exact payload bytes with `manifest_id` absent. The final
envelope inserts `manifest_id`, uses the same canonical JSON rules, and appends
one newline for stdout (not part of the payload digest). The ID identifies this
versioned content and analysis scope; it is not a signature, runtime-coverage
proof, exhaustive-analysis guarantee or safety decision. Same executable,
options and supported state must give identical bytes with cold/warm caches
and relocated copies. Ref spelling, Git metadata, supported source/configuration
state, effective options and build identity participate in the payload.

## Completeness and limitations

Manifest completeness can only be as strong as both inherited analyses and the
Git/freshness checks. It concerns enumeration within a supported static profile;
behavioural coverage is always unknown. No `safe` or
`exercises_changed_code: true` field exists. A gap explains what cannot be
established, without assigning risk or claiming that tests do not exist.

Known gaps remain explicit: dynamic imports/runtime dispatch, external packages,
JavaScript tests, excluded test paths, uninterpreted configuration and the
[import-type grammar limitation](TYPESCRIPT_GRAMMAR_LIMITATIONS.md). A work limit
can hide undiscovered findings whose number is unknown; known inherited list
omissions are counted separately. Successful serialization has no output
omissions and no selected-file scope exclusions.

`completeness.analysis_limited` and `undiscovered_due_to_work_limits` describe
work that stopped. `analysis_omissions` counts only known exclusions, such as
listed declarations that were not analysed or known capped importer entries;
it never estimates undiscovered edges. `output_truncated=false`, empty
`output_omissions` and empty `excluded_by_scope` are guaranteed for successful
schema-1 output. A byte-budget failure emits no artifact and thus no manifest
ID. Different output budgets or cache locations that admit the same complete
payload do not alter its identity.
The byte budget bounds serialized output, not total heap memory. The graph and
canonicalization also occupy memory under the existing analysis work limits;
a response byte budget is not a process-memory limit.

The Rust schema lives in [manifest.rs](../crates/retrieval/src/manifest.rs).
`Payload` is the typed identity input; `respond` derives the identifier and
emits the canonical envelope. `complete` means complete within the supported
enumeration profile and can coexist with an `unknown_behavioral_coverage` gap.


See [MCP transport](MCP_VERIFICATION_MANIFEST.md) for retained artifact retrieval.
