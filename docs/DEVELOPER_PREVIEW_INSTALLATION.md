# Install Repogi

Repogi is a developer preview. Source repository: https://github.com/thinkgrid-labs/repogi.
License: [MIT](../LICENSE). See [third-party notices](../THIRD_PARTY_NOTICES.md) for dependency licenses.

> **Release status:** no prebuilt binary release has been published yet. The installer commands below describe the release interface and start working once the first GitHub Release exists. Until then, [build from source](#build-from-source).

Repogi is one self-contained executable, `repogi`. Running it needs no Rust, Cargo, Node.js or Python.

## Supported platforms

| Platform | Target | Requirement |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | glibc 2.34 or newer (Ubuntu 22.04+, Debian 12+, RHEL/Rocky/Alma 9+, Fedora 35+; tested on Ubuntu 22.04 and 24.04) |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | glibc 2.34 or newer (tested on Ubuntu 22.04 and 24.04 ARM runners) |
| macOS Apple silicon | `aarch64-apple-darwin` | macOS 11 or newer (deployment target; tested on macOS 15 runners) |
| macOS Intel | `x86_64-apple-darwin` | macOS 11 or newer (deployment target; tested on macOS 15 Intel runners) |
| Windows x64 | `x86_64-pc-windows-msvc` | 64-bit Windows 10 or newer; static C runtime, so no Visual C++ redistributable (tested on Windows Server 2025 runners) |

The release workflow builds each target on a GitHub-hosted runner of that platform, runs the test suite and a consumer smoke test there, and checks the executable's architecture and dynamic dependencies. On Linux it checks that no symbol needs a glibc newer than 2.34. Linux binaries are dynamically linked against glibc. They do **not** run on musl-based systems such as Alpine. Use [Build from source](#build-from-source) there, and for other CPUs or older glibc.

## macOS and Linux

```sh
curl -fsSL https://github.com/thinkgrid-labs/repogi/releases/latest/download/install.sh | sh
repogi --version
```

`install.sh` does the following, and fails closed at every step:

1. Detects the OS and CPU (a Rosetta shell on Apple silicon gets the native arm64 build) and, on Linux, checks the glibc version.
2. Resolves the latest stable release, or uses `--version`. GitHub's "latest" never points at a pre-release, so previews must be pinned.
3. Downloads the matching archive and `SHA256SUMS` over HTTPS from the official GitHub Release, and rejects HTTP errors, missing assets and missing or malformed checksum entries.
4. Verifies the archive's SHA-256. Optionally, `--verify-attestation` also checks GitHub build provenance with `gh`.
5. Lists the archive and refuses absolute paths, `..`, links, special files or anything outside the expected top-level directory. It then extracts only `repogi`.
6. Copies the executable next to its destination under a temporary name, runs `--version`, and requires the exact expected version. Only then does it rename the file into place, so an existing installation survives any failure.
7. Installs into `~/.local/bin` by default. It never uses `sudo`, never edits shell profiles and never runs any other file from the archive. If the directory is not on `PATH`, it prints the line to add.

Options (also available as environment variables):

| Option | Variable | Meaning |
| --- | --- | --- |
| `--version 0.1.0` | `REPOGI_VERSION` | Install that release, e.g. `0.2.0-preview.1`. |
| `--install-dir DIR` | `REPOGI_INSTALL_DIR` | Install into `DIR` (created if missing; paths with spaces work). |
| `--verify-attestation` | | Also run `gh attestation verify` on the archive. |
| | `REPOGI_RELEASES_URL` | Mirror or test server replacing `https://github.com/thinkgrid-labs/repogi/releases`. HTTPS only; plain HTTP is accepted only for `127.0.0.1`/`localhost`. |

Pass options through the pipe with `sh -s --`, for example `... | sh -s -- --version 0.1.0`. To read the script before running it:

```sh
curl -fsSLO https://github.com/thinkgrid-labs/repogi/releases/latest/download/install.sh
less install.sh
sh install.sh --version 0.1.0
```

Rerunning the installer is safe. It replaces the executable with the requested version, which also covers upgrades and downgrades. To uninstall, delete `~/.local/bin/repogi`. Indexes stay inside each project's `.code-intel/` directory.

## Windows

```powershell
irm https://github.com/thinkgrid-labs/repogi/releases/latest/download/install.ps1 | iex
```

`install.ps1` runs on Windows PowerShell 5.1 and PowerShell 7. It applies the same checks as `install.sh`: exact asset name, `SHA256SUMS` verification, zip-entry validation, and a `--version` run before replacing anything. It installs only `repogi.exe` into `%LOCALAPPDATA%\Programs\Repogi\bin`. It needs no administrator rights and does not change the execution policy. `irm | iex` runs the script text without a policy check.

PATH is changed only on request. Without `-AddToPath`, the script prints the command that adds the directory to your user PATH. With `-AddToPath`, it appends the directory to the *user* PATH; machine-wide settings are never touched. Open a new terminal afterwards.

To pin a version or inspect the script first:

```powershell
Invoke-WebRequest https://github.com/thinkgrid-labs/repogi/releases/latest/download/install.ps1 -OutFile install.ps1
notepad install.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Version 0.1.0 -AddToPath
```

`-ExecutionPolicy Bypass` affects only that one process. Parameters: `-Version`, `-InstallDir`, `-AddToPath`, `-VerifyAttestation`; `REPOGI_VERSION`, `REPOGI_INSTALL_DIR` and `REPOGI_RELEASES_URL` work as on macOS/Linux. Windows on ARM is not a published target.

## Manual download and verification

Download an archive and `SHA256SUMS` from the [Releases page](https://github.com/thinkgrid-labs/repogi/releases). Asset names are described in [release assets](RELEASES.md).

```sh
sha256sum --check --ignore-missing SHA256SUMS           # Linux
shasum -a 256 --check --ignore-missing SHA256SUMS       # macOS
gh attestation verify repogi-0.1.0-aarch64-apple-darwin.tar.gz --repo thinkgrid-labs/repogi   # optional provenance
```

```powershell
(Get-FileHash .\repogi-0.1.0-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()
Select-String repogi-0.1.0-x86_64-pc-windows-msvc.zip SHA256SUMS
```

Unpack the archive and put `repogi` (`repogi.exe`) on your `PATH`. The archive also holds `LICENSE`, `THIRD_PARTY_NOTICES.md`, `licenses/` and a `BUILD-INFO.json` that records the version, target, source commit, Rust compiler and the executable's SHA-256. The macOS binaries are not notarized. A browser download may be quarantined; `xattr -d com.apple.quarantine repogi` clears that. The installer downloads with `curl`, so its copy is not quarantined.

## Build from source

Install Rust 1.98 or newer (the version CI tests), Git and a native C compiler/linker. Cargo retrieves the locked dependencies on first build; a populated Cargo cache supports offline builds. The build requires no Node, Python or model service.

```sh
git clone https://github.com/thinkgrid-labs/repogi.git
cd repogi
cargo install --path crates/cli --locked
repogi --version
```

For development, use `cargo build --workspace --locked` and `cargo test --workspace --locked`. Formatting and lint checks are `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings`.

## First project

```sh
repogi index /absolute/path/to/project
cd /absolute/path/to/project
repogi context "authentication flow" --format text
```

Indexing explicitly creates `.code-intel/index.db` inside that project. Rerun indexing after source changes. Navigation supports TypeScript `.ts` and TSX `.tsx`; use native search for other languages. Git is needed for impact commands. The indexer never executes the indexed project's scripts.

Each repository has its own index. Indexing `~/projects/frontend` and `~/projects/backend` creates two independent `.code-intel/` directories, and nothing is shared between them. Start one MCP server per project with `--root` (see [MCP](MCP.md)). Configure a local client using the [client examples](../examples/agents/), replacing the executable placeholder with the output of `command -v repogi`. Check [navigation limits](REFERENCE_NAVIGATION_LIMITS.md) before treating a response as complete.
