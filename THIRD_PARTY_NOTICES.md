# Third-party licenses

Repogi’s own source is MIT licensed; see [LICENSE](LICENSE). Dependency and fixture licenses remain separate. This source tree does not vendor dependency implementations. Cargo obtains versions pinned by Cargo.lock.

[Dependency inventory](licenses/THIRD_PARTY_NOTICES.json) lists the declared SPDX expressions and available verbatim license/notice files under [licenses/dependencies](licenses/dependencies/), including build, development and target-conditional packages. Multiple license files preserve upstream alternatives; their presence does not dual-license Repogi.

The TypeScript source excerpts under `fixtures/real-world/checkgate` and `fixtures/real-world/recached` retain their Apache-2.0 [LICENSE](fixtures/real-world/checkgate/LICENSE), [NOTICE](fixtures/real-world/checkgate/NOTICE), [LICENSE.md](fixtures/real-world/recached/LICENSE.md) and [NOTICE](fixtures/real-world/recached/NOTICE). Keep those notices with source distributions containing the excerpts.

SQLite’s native source notice is retained in [SQLITE_HEADER.txt](licenses/native/SQLITE_HEADER.txt). The provisioned Rust standard library notice is retained in [COPYRIGHT-library.html](licenses/rust/COPYRIGHT-library.html); a binary distribution must use notices matching its actual toolchain and linked native components.

The original MIT grant and copyright notices for `r-efi 6.0.0` are retained verbatim in [AUTHORS](licenses/dependencies/r-efi-6.0.0/AUTHORS). Its upstream license alternatives remain unchanged. `r-efi` and `rsqlite-vfs` are not in the Linux CLI’s normal/build dependency graph; the inventory also covers target-conditional and development dependencies.

Applicable grant texts remain unavailable locally for `tree-sitter 0.24.7`, `tree-sitter-typescript 0.23.2` and `rsqlite-vfs 0.1.1`. The `tree-sitter 0.24.7` entry holds only the Unicode/ICU notice for its embedded `src/unicode` tables, not the runtime's own MIT grant. Declared SPDX expressions do not replace upstream grants and notices. Release remains blocked wherever those assets apply: the parser runtime and TypeScript grammar are linked into every CLI binary. `python3 scripts/release/check_notices.py --target <target>` reports each target's gaps. Do not replace them with Repogi’s MIT notice.
