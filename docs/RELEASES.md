# Release assets and publication

This page is the contract between the release workflow, the installers and anyone downloading Repogi by hand. The machine-readable copy is [`scripts/release/contract.py`](../scripts/release/contract.py). The installers implement the same naming independently, and their tests check it.

## Versions and tags

- A release version is the Cargo workspace version: `MAJOR.MINOR.PATCH`, optionally `-PRERELEASE` (for example `0.2.0-preview.1`).
- Its Git tag is `v<version>`. The workflow refuses a tag that differs from the Cargo version.
- A version with a `-` suffix is published as a GitHub pre-release. GitHub's `releases/latest` never points at a pre-release, so the installers install a preview only when it is pinned with `--version` / `-Version`.

## Assets of one release

| Asset | Contents |
| --- | --- |
| `repogi-<version>-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64, glibc 2.34+ |
| `repogi-<version>-aarch64-unknown-linux-gnu.tar.gz` | Linux ARM64, glibc 2.34+ |
| `repogi-<version>-x86_64-apple-darwin.tar.gz` | macOS Intel, 11+ |
| `repogi-<version>-aarch64-apple-darwin.tar.gz` | macOS Apple silicon, 11+ |
| `repogi-<version>-x86_64-pc-windows-msvc.zip` | Windows x64 |
| `install.sh`, `install.ps1` | The installers, from the release commit |
| `SHA256SUMS` | `sha256sum`-format digest of every other asset |

Every archive holds a single top-level directory named like the archive without its extension:

```text
repogi-<version>-<target>/
  repogi            (repogi.exe on Windows)
  LICENSE
  THIRD_PARTY_NOTICES.md
  licenses/…        (dependency, SQLite and Rust standard library notices)
  README.txt
  BUILD-INFO.json   (version, target, source commit, rustc -vV, cargo, RUSTFLAGS,
                     executable SHA-256, glibc/macOS floor, runner, workflow run)
```

Nothing else may appear. `package.py` builds archives from that allowlist, and `assemble.py` re-reads every member and rejects any other file, links, special files, `..` or absolute paths. Defence-in-depth rules also refuse docs, fixtures, benchmarks, internal records and evaluation reports by name. Archive members are sorted and get fixed timestamps, owners and modes, so packaging the same executable twice yields identical bytes. That does not make the compilation itself reproducible across machines.

## What the workflow checks

[`.github/workflows/release.yml`](../.github/workflows/release.yml) runs on `v*` tags and on manual dispatch, never on pull requests:

| Job | Checks |
| --- | --- |
| `prepare` | The tag matches the Cargo version, and the public source manifest is valid. |
| `notices` | `check_notices.py` walks every target's linked dependency closure and requires each package's own license grant. It also requires the SQLite notice and a Rust standard-library notice that matches the toolchain. **Any gap blocks the release.** |
| `build` (×5, native runner per target) | Full test suite on that platform, release build, `inspect_binary.py` (format, CPU, allowed dynamic libraries, glibc floor 2.34, static Windows CRT), `smoke.py` (version, indexing, MCP stdio with exactly three Navigation tools, two-repository root isolation, no project code executed), then packaging. |
| `assemble` | Exactly one archive per target, member allowlist, a single source commit, `SHA256SUMS`. The checksum manifest becomes a job output that later jobs compare downloads against. |
| `verify-install` (Ubuntu 22.04/24.04 x64 and ARM, macOS Intel/arm64, Windows) | Installer regression suite against a local mock release: platform detection, asset selection, checksum failures, HTTP errors, unsafe archives, version pinning, repeat installs, rollback, paths with spaces. On each host the real archive is installed and smoke-tested. |
| `mcp-sdk` | The Linux x86_64 executable against the official MCP Python SDK. This job installs a third-party package, so it uploads nothing. |
| `publish` | Runs only for a tag, after every job above passed, in the `release` environment. Re-verifies all assets against the `assemble` output, attests build provenance for every asset in `SHA256SUMS`, and creates a **draft** release. |

Permissions start empty for every job. Only `publish` gets `contents: write`, `id-token: write` and `attestations: write`. Checkouts do not persist credentials, actions are pinned to full commit SHAs, and no caches are restored.

## Publication procedure (owner)

Nothing is published automatically. Before the first release:

1. In repository settings, create the `release` environment with yourself as required reviewer, and allow only `v*` tags to deploy to it.
2. Add a tag ruleset so that only maintainers can create `v*` tags.
3. Resolve every problem `python3 scripts/release/check_notices.py --target <target>` reports, for all five targets.

For each release:

1. Set the workspace version in `Cargo.toml`, commit, and push the tag `v<version>`. A manual dispatch from a branch is a dry run; it builds and tests but never publishes.
2. Wait for every job, then approve the `release` environment deployment. The workflow creates a draft release with all assets.
3. Review the draft, download an archive and check `sha256sum --check --ignore-missing SHA256SUMS` and `gh attestation verify <archive> --repo thinkgrid-labs/repogi`, then publish the draft.
4. Only after publishing, update the README release-status note.

## Local tools

All scripts use the Python standard library (the MCP SDK check also needs `pip install mcp`):

```sh
python3 scripts/release/check_notices.py --target x86_64-unknown-linux-gnu
python3 scripts/release/inspect_binary.py --target x86_64-unknown-linux-gnu target/release/repogi
python3 scripts/release/smoke.py --version 0.1.0 target/release/repogi
python3 scripts/release/package.py --target x86_64-unknown-linux-gnu --version 0.1.0 --binary target/release/repogi \
  --source-commit "$(git rev-parse HEAD)" --source-date-epoch "$(git log -1 --format=%ct)" --out-dir /tmp/repogi-dist
python3 scripts/release/tests/test_installers.py -v
REPOGI_TEST_ARCHIVES=/tmp/repogi-dist REPOGI_TEST_VERSION=0.1.0 python3 scripts/release/tests/test_installers.py -v
```
