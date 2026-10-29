# Install Repogi from source

Repogi is a developer preview. Source repository: https://github.com/thinkgrid-labs/repogi.
License: [MIT](../LICENSE). See [third-party notices](../THIRD_PARTY_NOTICES.md) for dependency licenses.

Install Rust 1.85 or later, Git and a native C compiler/linker. Cargo retrieves the locked dependencies on first build; a populated Cargo cache supports offline builds. No Node, Python, model service or evaluation files are required by the Rust build.

```sh
git clone https://github.com/thinkgrid-labs/repogi.git
cd repogi
cargo install --path crates/cli --locked
repogi --version
```

For development, use `cargo build --workspace --locked` and `cargo test --workspace --locked`. Formatting and lint checks are `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings`.

```sh
repogi index /absolute/path/to/project
cd /absolute/path/to/project
repogi context "authentication flow" --format text
```

Indexing explicitly creates `.code-intel/index.db`. Rerun indexing after source changes. Navigation supports TypeScript `.ts` and TSX `.tsx`; use native search for other languages. Git is needed for impact commands. The indexer never executes the indexed project's scripts.

Configure a local client using [MCP](MCP.md) and the [client examples](../examples/agents/). Check [navigation limits](REFERENCE_NAVIGATION_LIMITS.md) before treating a response as complete. Build compatibility depends on the selected toolchain and platform; no prebuilt binary is promised here.
