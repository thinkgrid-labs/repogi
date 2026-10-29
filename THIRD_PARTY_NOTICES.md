# Third-party licenses

Repogi’s own source is MIT licensed; see [LICENSE](LICENSE). Dependency and fixture licenses remain separate. This source tree does not vendor dependency implementations. Cargo obtains versions pinned by Cargo.lock.

[Dependency inventory](licenses/THIRD_PARTY_NOTICES.json) lists the declared SPDX expressions and available verbatim license/notice files under [licenses/dependencies](licenses/dependencies/), including build, development and target-conditional packages. Multiple license files preserve upstream alternatives; their presence does not dual-license Repogi.

The TypeScript source excerpts under `fixtures/real-world/checkgate` and `fixtures/real-world/recached` retain their Apache-2.0 [LICENSE](fixtures/real-world/checkgate/LICENSE), [NOTICE](fixtures/real-world/checkgate/NOTICE), [LICENSE.md](fixtures/real-world/recached/LICENSE.md) and [NOTICE](fixtures/real-world/recached/NOTICE). Keep those notices with source distributions containing the excerpts.

SQLite’s native source notice is retained in [SQLITE_HEADER.txt](licenses/native/SQLITE_HEADER.txt). The provisioned Rust standard library notice is retained in [COPYRIGHT-library.html](licenses/rust/COPYRIGHT-library.html); a binary distribution must use notices matching its actual toolchain and linked native components.

The original MIT grant and copyright notices for `r-efi 6.0.0` are retained verbatim in [AUTHORS](licenses/dependencies/r-efi-6.0.0/AUTHORS). Its upstream license alternatives remain unchanged. `r-efi` and `rsqlite-vfs` are not in the Linux CLI’s normal/build dependency graph; the inventory also covers target-conditional and development dependencies.

The MIT grants for `tree-sitter 0.24.7` and `tree-sitter-typescript 0.23.2` are retained verbatim from the upstream commits recorded in each crate's `.cargo_vcs_info.json`: `tree-sitter/tree-sitter@5e8760bf` (workspace `LICENSE`; the crate in `lib/` inherits `license = "MIT"` and version 0.24.7 from that workspace) and `tree-sitter/tree-sitter-typescript@f975a621`. The `tree-sitter` directory also keeps the Unicode/ICU notice for its embedded `src/unicode` tables. A grant text for `rsqlite-vfs 0.1.1` remains unavailable; it is not linked into any CLI target. `python3 scripts/release/check_notices.py --target <target>` checks every target's linked dependencies. Do not replace upstream notices with Repogi’s MIT notice.
