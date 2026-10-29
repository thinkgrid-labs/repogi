# Consuming verification manifests

The independent [reference consumer](../examples/manifest_consumer/consumer.py)
reads schema-1 files or retrieves them through the official Python MCP SDK.
It imports no Repogi generator, canonicalizer, cache implementation or validator.
The existing [transport](MCP_VERIFICATION_MANIFEST.md) and
[manifest](VERIFICATION_MANIFEST.md) contracts remain unchanged.

## Run the consumer

Use a separate Python environment with the dependencies in
[requirements.txt](../examples/manifest_consumer/requirements.txt). Fetch mode
uses the official SDK version pinned in requirements.txt and its legacy
initialization lifecycle.
Saved-file verification needs only Python and a BLAKE3 provider. It does not need
Repogi, Git, a repository, snapshot cache or MCP server.

The default hash provider is the separately installed Python `blake3` package.
Alternatively, `--digest-command` selects a trusted executable that reads raw
stdin and emits exactly `blake3:` followed by 64 lowercase hexadecimal digits.
It executes directly, without a shell. `ATLAS_BLAKE3_COMMAND` supplies the same
choice to the CI helper. Never select a program from an
untrusted repository.

For an independent BLAKE3 provider, compile the upstream C implementation with
the [stdin adapter](../examples/manifest_consumer/blake3_stdin.c), which contains
no manifest logic. Set `BLAKE3_C_DIR` to your trusted upstream C source directory:

```sh
cc -O2 -DBLAKE3_NO_SSE2 -DBLAKE3_NO_SSE41 -DBLAKE3_NO_AVX2 \
  -DBLAKE3_NO_AVX512 -I "$BLAKE3_C_DIR" \
  examples/manifest_consumer/blake3_stdin.c \
  "$BLAKE3_C_DIR/blake3.c" "$BLAKE3_C_DIR/blake3_dispatch.c" \
  "$BLAKE3_C_DIR/blake3_portable.c" -o /tmp/atlas-blake3-stdin

python examples/manifest_consumer/consumer.py \
  --digest-command /tmp/atlas-blake3-stdin fetch \
  --binary /trusted/repogi --root /checkout --base BASE_SHA --head HEAD_SHA \
  --budget 8000 --output /artifacts/manifest.json

python examples/manifest_consumer/consumer.py \
  --digest-command /tmp/atlas-blake3-stdin verify /artifacts/manifest.json --extract
```

Use an output outside the analysed root. Publication refuses an existing file
and atomically links a fully validated temporary file into place on the same
filesystem. Temporary files are removed on failure. Fetch mode prints a small
validation/transport record only after successful publication; the complete
manifest stays in the selected file. Nothing is sent to an LLM.

## Validate the whole transfer

Preparation explicitly requests `manifest:true` from `atlas_impact`. The consumer
checks every required descriptor field, exact integer versions (booleans are
rejected), digest syntax/algorithm, total length, snapshot objects and retrieval
instructions. It records both identities. It does not execute instructions or
select tools supplied by arbitrary descriptor extensions.

Retrieval uses decoded-byte offsets, the fixed authorized root and caller-selected
2–32 KB response budget. Each decoded range must start at the current end, advance
by its exact positive length, remain within the advertised total, and preserve
all transport identity metadata. Base64 padding must be canonical. UTF-8 is
decoded only after joining bytes; a chunk can end inside a character. Duplicate,
overlapping, reordered, missing and post-final ranges fail. Final means exactly
the advertised byte count, not merely a server-provided success flag.

The consumer independently checks:

1. BLAKE3 of every envelope byte against `envelope_digest` and `artifact_id`.
2. Strict UTF-8 JSON, duplicate-key rejection, schema-1 required sections/types,
   supported classification enums and absence of output/scope omissions.
3. Exact compact serialization with lexicographic keys and one final newline.
   Set arrays must be sorted and unique; `via` and `binding_chain` retain order.
   Floats, nonfinite numbers and out-of-range schema-1 integers are rejected.
4. BLAKE3 of canonical payload JSON without `manifest_id` or newline against the
   payload ID, plus descriptor snapshot equality when fetching.

Reads and reconstruction are capped at 8,000,000 bytes. A descriptor, final chunk,
parseable prefix or partial file is never accepted as a manifest. A standalone
file can optionally be bound to previously trusted `--expected-manifest-id` and
`--expected-envelope-digest` values. Schema checks validate the structure needed
by this consumer; they are not exhaustive semantic verification of every proof.

Digests establish content integrity, **not authenticity**. An actor controlling
both descriptor and manifest can invent self-consistent evidence and recompute
both hashes. Protect the expected digest or integrity record through a trusted
channel when origin matters. Neither a digest nor successful validation proves
that analysis is exhaustive, tests execute changed behavior, or a change is safe.

## Expiry, retry and recovery

`artifact_expired`, `artifact_corrupt`, `artifact_cache_busy` and argument/range
errors remain distinguishable. Consumer validation errors have separate codes
such as `range`, `chunk_metadata`, `envelope_digest`, `manifest_id`, `canonical`
and `schema_version`.

Only explicit `artifact_cache_busy` is retried: at most two retries after the
first attempt, with 50 ms then 100 ms backoff. Other failures immediately end
the transfer without publishing an artifact. There is no background polling,
automatic preparation, or automatic state substitution. SDK connection failures
also fail the transfer rather than producing a partial successful file.

On expiry, discard the failed reconstruction. If desired, explicitly prepare
again and verify a new transfer. Compare **both identities and exact bytes**
before treating it as the earlier artifact. A working-tree edit can change the
new manifest even when requested refs/options are unchanged. Different bytes or
identities mean a new artifact; do not claim it represents the earlier state.
Retention survives restarts but remains a bounded cache, not a lease or archive.

## Consume evidence without executing it

`verify --extract` copies recorded Git states, declaration changes, affected
relationships/files, confirmed test links, possible candidates, unknown tests,
gaps, coverage and completeness into a separate JSON result. It retains the
original evidence and classifications. Known direct/reference/JSX/re-export and
transitive categories can identify affected-code files; unfamiliar categories
are also exposed as opaque data and never establish a confirmed relationship.

Possible candidates remain possible. Behavioral coverage stays `unknown` and
execution stays `not_performed`. A complete serialized artifact can still have
`completeness.complete=false`, work limits, unsupported syntax and unknown
coverage. Extraction does not choose a sufficient test set or reinterpret the
grammar-gap and dynamic-import cases as confirmed usage.

## Local CI publication

The independent [CI helper](../examples/manifest_consumer/ci_artifact.py) selects
an executable outside the checkout and checks its caller-supplied SHA-256. It
resolves explicit base/head arguments to local commit objects and confirms their
availability. Repogi never fetches objects or runs checked-out scripts.

```sh
export ATLAS_BLAKE3_COMMAND=/trusted/blake3-stdin
python examples/manifest_consumer/ci_artifact.py \
  --binary /trusted/repogi --sha256 TRUSTED_EXECUTABLE_SHA256 \
  --root /checkout --base BASE_SHA --head HEAD_SHA \
  --output /artifacts/new-unique-bundle
```

Generation writes into a private pending directory outside the checkout. The
helper checks the exit status, runs `manifest-verify`, independently validates
schema and both identities, checks resolved commit provenance, and starts a
separate saved-file consumer. Only then does it rename the complete directory
into place. The bundle contains the manifest and a small integrity record with
both identities, total bytes, executable checksum and resolved commits. Use a
unique caller-owned output destination on one filesystem.

Budget, missing-object, executable-checksum, CLI-verifier and independent-verifier
failures remove pending output and never publish the bundle. Saved artifacts
can be verified independently of the original repository or cache.
Reproduction requires the same executable, options, requested resolved refs and
Git/index context, not solely the pair of compared tree contents.

The optional [Actions example](../examples/ci/verification-manifest.yml.example)
contains action-pin placeholders. Before copying it into a consumer repository,
select trusted action revisions and configure the binary URL and checksum.
The template checks object availability, verifies the artifact before upload and
uses seven-day retention. Choose visibility and retention appropriate to the
repository metadata in the artifact. Full-history checkout alone does not prove
that both compared commits are available; missing objects fail explicitly.

## Schema compatibility policy

| Change | Policy |
|---|---|
| Missing required section/field or wrong type | Reject; never supply optimistic defaults. |
| Unknown manifest version | Reject clearly before interpreting evidence. No schema 2 implemented. |
| Unknown top-level schema-1 field | Reject, matching the existing CLI verifier's closed nine-field envelope. |
| Additional nested descriptive field | May be additive within schema 1 if semantics and canonical rules stay unchanged. This consumer retains it opaquely; it participates in identity. |
| Unknown candidate priority, snapshot kind, execution or behavioral coverage enum | Reject because interpretation affects evidence claims. |
| Unknown affected-code category or gap kind | Preserve opaquely; never infer confirmation or completeness from the unfamiliar value. |
| Removed fields, changed meaning/type/defaults, classification rules or canonicalization | Require a manifest-schema increment and a separately validated consumer. |
| Added set member or ordered proof step | Changes the content and ID; existing canonical rules still apply. |
| Changed schema version | Version participates in payload hashing; no identity equivalence across migrations is implied. Preserve old bytes/IDs. |
| Transport framing, range or digest interpretation changes | Increment transport version; this is independent of manifest schema version. |

Unknown descriptor/chunk extension fields may be ignored when all required
version-1 fields and invariants remain valid. Unsupported versions, tools,
encoding or offset semantics are rejected. A future transport can carry old
schema-1 bytes only through an explicit supported transport contract. Keep the
original artifact when migrating; do not rewrite historical manifests or reuse
their IDs for transformed content.
