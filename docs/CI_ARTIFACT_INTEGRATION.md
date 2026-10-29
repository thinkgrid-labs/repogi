# CI verification artifacts

CI can save the complete [schema-1 manifest](VERIFICATION_MANIFEST.md) emitted by
the existing CLI. Repogi compares revisions and exports static evidence. It does
not run project tests, choose sufficient tests, approve changes or publish files.

## Produce and validate

Select a trusted Repogi executable explicitly. Supply existing base/head Git
objects and an output directory outside the source checkout. For example, in
Bash on an isolated CI runner:

```sh
set -euo pipefail
REPOGI_BIN=/opt/repogi/bin/repogi
REPO=/workspace/checkout
BASE_REV=0123456789012345678901234567890123456789 # replace with an existing commit
HEAD_REV=abcdef0123456789012345678901234567890123 # replace with an existing commit
ARTIFACT_DIR=$(mktemp -d /tmp/atlas-artifact.XXXXXX)

"$REPOGI_BIN" impact "$REPO" --base "$BASE_REV" --head "$HEAD_REV" \
  --manifest --format json --max-bytes 8000000 \
  > "$ARTIFACT_DIR/manifest.pending"
"$REPOGI_BIN" manifest-verify "$ARTIFACT_DIR/manifest.pending" \
  > "$ARTIFACT_DIR/atlas-verification-integrity.json"
mv "$ARTIFACT_DIR/manifest.pending" \
  "$ARTIFACT_DIR/atlas-verification-manifest.json"
```

The shell owns file creation and redirection. Repogi writes its normal authorized
snapshot cache and stdout; it never automatically creates an export in the
checkout. The pending filename prevents a failed command from being uploaded
as a successful artifact. The chosen executable must be accessible outside
untrusted repository paths; do not build or download it from a PR-controlled
script. A commit comparison is preferable to a mutable working tree in CI.

`manifest-verify <file>` (or `manifest-verify -` for stdin) reads at most
8,000,001 bytes and rejects empty, oversized, malformed, unsupported-version or
noncanonical envelopes. It checks schema_version=1, required top-level sections,
canonical object/set ordering, the exact newline-terminated envelope, absence
of output omissions and the independently recomputed BLAKE3 payload identity.
It does not perform exhaustive semantic validation of every nested field.

The successful JSON validation record contains valid, schema_version,
manifest_id, envelope_digest and total_bytes. The exact-envelope digest is
keyless BLAKE3-256 over all exported bytes including the newline. It differs
from manifest_id, which hashes canonical JSON without manifest_id or a newline.
Optional expected values bind validation to a previously received descriptor:

```sh
"$REPOGI_BIN" manifest-verify "$ARTIFACT_DIR/atlas-verification-manifest.json" \
  --expected-manifest-id 'blake3:<payload digest>' \
  --expected-envelope-digest 'blake3:<exact envelope digest>'
```

Validation success establishes integrity and supported encoding, not analysis
correctness, test execution or change safety. Digests are not signatures. Persist
the complete JSON and its integrity record together; protect that record when
authenticity matters. A separate raw-byte BLAKE3 implementation can recompute
both values using the documented contract. The
[independent digest example](../crates/cli/examples/artifact_digest.rs) hashes
stdin without importing the manifest generator or verifier.

## Explicit CI choices and failures

Configure the executable/build, checkout, base/head revisions, depth, result and
declaration work limits, optional test directories, byte budget and artifact
retention policy explicitly. The defaults are depth 2, max_results 200 and
max_declarations 100; a smaller traversal can produce a valid artifact with
analysis_limited=true. An integrity check does not erase that uncertainty.

Reproduction requires the same selected executable, options, requested refs and
recorded Git/index context, including the checked-out HEAD. Identity is not
promised solely from two compared commit IDs.

For a pull request, compare its recorded base SHA and head SHA. Decide externally
whether the desired comparison is the tip base SHA or a merge base; Repogi does
not silently choose one. Fetch the required objects in the CI checkout step.
A shallow checkout may omit them. Repogi never fetches missing objects: an unknown
revision fails with nonzero status and empty stdout. Unsupported Git layouts
also fail. A small --max-bytes fails with empty stdout if the entire manifest
will not fit; increasing work limits and increasing byte budgets are separate
decisions. Use shell fail-fast behavior and upload only the validated final file.

Known unsupported syntax, dynamic linkage, uncertain identities, configurations
and behavioral coverage remain in the exported data. A complete manifest can
still describe incomplete analysis. No execution plan or merge gate is implied.

## Optional GitHub Actions template

[verification-manifest.yml.example](../examples/ci/verification-manifest.yml.example)
is an optional consumer example, separate from the repository’s
[build and test CI](../.github/workflows/ci.yml). Copy and configure it in a
consumer repository only when you want verification-artifact export.
Before using it, replace both action placeholders with reviewed full commit
SHAs and configure repository variables ATLAS_LINUX_AMD64_URL and
ATLAS_LINUX_AMD64_SHA256 for a trusted, explicitly selected standalone binary.
The binary is downloaded outside the checkout and checked against that SHA-256
before execution. This template is not a release-download service or a Repogi
workflow runtime.

It uses pull_request or manual dispatch on a GitHub-hosted disposable runner,
contents:read, no stored checkout credentials and no project build/test command.
It checks out complete available history, then explicitly checks both full
commit IDs before comparing them. Missing objects fail; adapt the checkout/fetch
step to your hosting topology if necessary. It publishes only the selected
validated files through an ordinary artifact action, with no PR comments,
repository write permission, elevated token or approval operation.

Untrusted PR contents remain parser inputs. Do not switch this template to
pull_request_target, introduce secrets, reuse privileged caches or run scripts
from the checked-out source. Expressions containing revision/download inputs
are passed through quoted environment variables, not interpolated into shell
source. These choices follow GitHub's [secure workflow guidance](https://docs.github.com/en/actions/reference/security/secure-use).
See the official [checkout options](https://github.com/actions/checkout/blob/main/README.md)
and [artifact upload contract](https://github.com/actions/upload-artifact/blob/main/README.md)
when selecting compatible pinned actions and retention settings.

An exported manifest contains repository-relative paths, declaration names,
dependency metadata and static evidence. Choose artifact visibility and retention
for that information. No source snippets, secrets or environment values are
added by the transport, but included repository metadata may still be sensitive.
