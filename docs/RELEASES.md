# Release assets and verification

This page is the contract between the release workflow, the installers and anyone downloading Repogi by hand. The machine-readable copy is [`scripts/release/contract.py`](../scripts/release/contract.py). The installers implement the same naming independently, and their tests check it.

## Versions and tags

- A release version is the Cargo workspace version: `MAJOR.MINOR.PATCH`, optionally `-PRERELEASE` (for example `0.1.1-preview.1`).
- Its Git tag is `v<version>`. The workflow refuses a tag that differs from the Cargo version.
- A version with a `-` suffix is published as a GitHub pre-release. GitHub's `releases/latest` never points at a pre-release, so the installers install a preview only when it is pinned with `--version` / `-Version`.

## Assets of one release

| Asset | Contents |
| --- | --- |
| `repogi-<version>-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64, glibc 2.34+ |
| `repogi-<version>-aarch64-unknown-linux-gnu.tar.gz` | Linux ARM64, glibc 2.34+ |
| `repogi-<version>-x86_64-apple-darwin.tar.gz` | macOS Intel, 11+ |
| `repogi-<version>-aarch64-apple-darwin.tar.gz` | macOS Apple silicon, 11+ |
| `install.sh` | The installer, from the release commit |
| `SHA256SUMS` | `sha256sum`-format digest of every other asset |

Windows (`x86_64-pc-windows-msvc`, `.zip`, with `install.ps1`) is not published yet: the test suite does not pass on Windows. The packaging scripts keep zip and PE support for when it returns.

Every archive holds a single top-level directory named like the archive without its extension:

```text
repogi-<version>-<target>/
  repogi
  LICENSE
  THIRD_PARTY_NOTICES.md
  licenses/…        (dependency, SQLite and Rust standard library notices)
  README.txt
  BUILD-INFO.json   (version, target, source commit, rustc -vV, cargo, RUSTFLAGS,
                     executable SHA-256, glibc/macOS floor, runner, workflow run)
```

Nothing else may appear. `package.py` builds archives from that allowlist, and `assemble.py` re-reads every member and rejects any other file, links, special files, `..` or absolute paths. Defence-in-depth rules also refuse docs, fixtures, benchmarks, internal records and evaluation reports by name. Archive members are sorted and get fixed timestamps, owners and modes, so packaging the same executable twice yields identical bytes. That does not make the compilation itself reproducible across machines.
