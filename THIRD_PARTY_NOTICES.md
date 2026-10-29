# Third-party licenses

Repogi’s own source is MIT licensed; see [LICENSE](LICENSE). Dependency and fixture licenses remain separate. This source tree does not vendor dependency implementations. Cargo obtains versions pinned by Cargo.lock.

[Dependency inventory](THIRD_PARTY_NOTICES.json) lists the declared SPDX expressions and available verbatim license/notice files under [licenses/dependencies](licenses/dependencies/), including build, development and target-conditional packages. Multiple license files preserve upstream alternatives; their presence does not dual-license Repogi.

The TypeScript source excerpts under `fixtures/real-world/checkgate` and `fixtures/real-world/recached` retain their Apache-2.0 [LICENSE](fixtures/real-world/checkgate/LICENSE), [NOTICE](fixtures/real-world/checkgate/NOTICE), [LICENSE.md](fixtures/real-world/recached/LICENSE.md) and [NOTICE](fixtures/real-world/recached/NOTICE). Keep those notices with source distributions containing the excerpts.

SQLite’s native source notice is retained in [SQLITE_HEADER.txt](licenses/native/SQLITE_HEADER.txt). The provisioned Rust standard library notice is retained in [COPYRIGHT-library.html](licenses/rust/COPYRIGHT-library.html); a binary distribution must use notices matching its actual toolchain and linked native components.

License texts are currently unavailable for `rsqlite-vfs 0.1.1`, `tree-sitter-typescript 0.23.2` and target-conditional `r-efi 6.0.0` in the cached dependency sources. Their declared SPDX expressions are recorded but are not substitutes for applicable upstream grants and notices. Release remains blocked until the applicable assets and complete notice review are resolved. Do not replace them with Repogi’s MIT notice.
