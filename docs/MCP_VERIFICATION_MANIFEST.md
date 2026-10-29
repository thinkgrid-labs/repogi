# Verification manifests over MCP

The MCP transport serves complete schema-1 verification manifests in bounded
chunks. See the [manifest identity contract](VERIFICATION_MANIFEST.md).

## Request and failure contract

Enable the opt-in tool with `--tools atlas_impact`. Use `summary:true`,
`tests:true` and `evidence_file` for interactive review rather than a complete
manifest export.

Prepare a complete artifact:

```json
{"manifest":true,"base":"main","head":"feature","max_bytes":8000}
```

`manifest:true` returns JSON transport metadata, never an inline manifest. It
accepts the usual root, revision, depth, max_results and test_dirs settings.
`format` may be omitted or `json`. It rejects tests, summary, detail,
evidence_file and artifact fields, including explicitly supplied false values.
The complete artifact uses the existing 8,000,000-byte engine limit independently
of the response budget. Work limits and uncertainty remain in the artifact.

Retrieve retained bytes without another comparison:

```json
{"artifact":{"id":"blake3:<64 lowercase hex digits>","offset":0,"length":16000},"max_bytes":32000}
```

Only artifact, root and max_bytes are accepted in retrieval mode. All three
artifact fields are required; unknown fields are rejected at both levels.
On a multi-root server, repeat the same authorized root on every chunk request.
An artifact ID does not select a root; descriptors contain no absolute root path.
Length is a maximum decoded byte count, 1..16000. Offset is a nonnegative byte
offset strictly below total_bytes. Each response may reduce length to fit the
budget; consumers advance by the returned next_offset. Offsets refer to the
original UTF-8 envelope bytes, including its final newline, before base64.
UTF-8 character boundaries need not coincide with chunk boundaries.

Both modes use the existing response range 2000..32000 (default 8000). Transport
fitting accounts for base64, all metadata and JSON escaping in the MCP
CallToolResult, with an additional 128-byte framing reserve. It never fits by
dropping artifact fields. Descriptor failure returns response_too_large before
installing the artifact. Chunk fitting either returns at least one decoded byte
or fails. The global response cap is unchanged.

Errors are MCP isError text, not successful partial JSON: invalid_arguments,
invalid_revision, unsupported_repository, not_a_git_repository, state_rejected,
response_too_large, artifact_expired, artifact_corrupt, artifact_cache_busy or
artifact_transport_unavailable. Missing/evicted entries return artifact_expired;
corrupt content returns artifact_corrupt. Neither is recomputed under the old
identifier. Consumers discard incomplete reconstructions on any error, missing
range or failed digest check. Repeated requests for an existing range are
idempotent. Repeated chunks are never appended blindly.

## Descriptor and chunk

A descriptor has kind=verification_manifest_descriptor, is_manifest=false,
protocol=atlas-manifest-bytes, transport_version=1, schema_version=1,
manifest_id, artifact_id, envelope_digest, total_bytes, base/head snapshots,
availability=retained and retrieval instructions. Snapshot identities are the
manifest's original base/head snapshot objects. It describes one complete,
validated artifact; it is not evidence in place of that artifact.

artifact_id and envelope_digest are the same `blake3:<hex>` value: keyless
BLAKE3-256 of every exact envelope byte, including manifest_id and the newline.
This differs deliberately from manifest_id, which hashes only canonical payload
JSON with manifest_id absent and no trailing newline. Digests are not signatures.

A chunk has kind=verification_manifest_chunk, is_manifest=false, the same
protocol/version, artifact_id, envelope_digest, total_bytes, offset,
next_offset, decoded_bytes, encoding=base64, data and final. Base64 is standard
RFC 4648 with padding and no whitespace. final is true exactly when next_offset
equals total_bytes. No chunk, including a final chunk, is a standalone manifest.

Reconstruction requires contiguous decoded ranges from zero to total_bytes,
matching immutable identity/length metadata, exact envelope digest validation,
then schema/canonical-envelope/manifest_id validation. See the CLI verifier and
the [CI workflow](CI_ARTIFACT_INTEGRATION.md).

## Retention and confinement

Retention is disk-backed under the fixed authorized location
`<root>/.code-intel/impact/artifacts-v1`. Callers cannot select a cache path.
There is no new database. A root-binding marker rejects copied caches from a
different canonical root. Retrieval never reads another configured root's cache.

Entries are immutable, atomically installed files. Cooperating processes use an
advisory lock for installation, reads and eviction. Preparation also holds this
lock while using the snapshot cache. Separate concurrent servers can receive
artifact_cache_busy and retry; requests within one server remain serialized.
Every read validates file
type, size, canonical JSON, schema, payload ID and exact envelope digest before
serving bytes. Directory-relative no-follow file operations prevent symlink
traversal, including swapped cache entries; hardlinked regular files are rejected
before reading. Transport retention fails closed on platforms without these
Unix filesystem operations; ordinary CLI manifest output remains available.

At most 32 entries and 64,000,000 artifact bytes are retained per root. Eviction
is FIFO by persisted monotonically increasing installation sequence, with digest
as a tie-breaker. Reads and duplicate installations do not refresh age. A single
artifact is at most 8,000,000 bytes. The cache lock serializes temporary installs;
one abandoned temporary file is cleaned on the next installation. Unrecognized
cache entries are rejected instead of followed or recursively deleted.

Entries survive server restarts and executable upgrades until evicted, explicitly
removed by the owner or detected as corrupt. A working-tree edit never changes
retained bytes. Artifact creation may prepare a newer artifact, whose identity
is independent. Retention is bounded availability, not a durable archive or lease.
The descriptor promises current retention, not that future requests cannot expire.
Use the complete CLI artifact for durable CI storage.

The cache protects against accidental corruption and untrusted repository paths;
it is not an authentication boundary against an actor with control over the
Repogi process or its cache directory. Exporting a manifest shares included
repository-relative paths, names and dependency metadata with the recipient.
No source snippets, environment values, credentials or arbitrary files are added.
