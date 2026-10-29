# Public source and proposed binary contents

`source-files.txt` is the explicit source inclusion manifest. Validate it with:

```sh
python3 scripts/prepare_source.py
python3 scripts/prepare_source.py --output /absolute/path/to/fresh-source
```

Build that source using `cargo build --workspace --locked`; install with `cargo install --path crates/cli --locked`. The exporter uses Python's standard library. Building and installing the Rust product do not require Python. Cargo needs the locked dependency sources, a Rust toolchain and native compiler; a populated dependency cache permits `--offline`. Source tests and synthetic fixtures are included. The manifest includes the source fixtures required by those tests.

Per-platform release archives follow [docs/RELEASES.md](../docs/RELEASES.md) and are built by `scripts/release/package.py`; they contain the executable, license and notice files and build information, not the source tree. `binary-files.txt` is the earlier proposal for a single binary-with-source bundle, kept in step with the source manifest; the release workflow does not use it. It proposes archive member paths for a future binary-with-source bundle. Including the public source keeps all documentation, example and license links usable. `bin/repogi` must come from a separately built and validated release executable. The other entries refer to the corresponding public source files. Preserve all required dependency/native/toolchain licenses. No binary archive is produced by the source preparation command.

License metadata is in `release.json`. The notices inventory records unresolved license assets; resolve those and review notices for the exact executable/toolchain before releasing anything. These manifests describe intended membership and do not authorize publication. Git-generated archives require the corresponding public files and export rules to be committed first; old commits retain their original contents.
