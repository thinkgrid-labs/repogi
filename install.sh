#!/bin/sh
# Repogi installer for macOS and Linux.
#
#   curl -fsSL https://github.com/thinkgrid-labs/repogi/releases/latest/download/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version 0.1.0 --install-dir "$HOME/bin"
#
# Downloads the release archive for this OS and CPU from the official GitHub
# Release, verifies it against the release's SHA256SUMS, and installs only the
# `repogi` executable into a user directory (default: $HOME/.local/bin). It never
# uses sudo, never edits shell profiles, and leaves any existing installation
# untouched unless the new binary verified and ran successfully.
set -eu

REPOSITORY="thinkgrid-labs/repogi"
OFFICIAL_RELEASES="https://github.com/${REPOSITORY}/releases"
# Must match GLIBC_FLOOR in scripts/release/contract.py.
GLIBC_MIN="2.34"

usage() {
    cat <<EOF
Install the prebuilt repogi executable.

Usage: install.sh [--version VERSION] [--install-dir DIR] [--verify-attestation]

  --version VERSION     Release to install, e.g. 0.1.0 or 0.2.0-preview.1
                        (default: latest stable release). Env: REPOGI_VERSION
  --install-dir DIR     Directory for the executable (default: \$HOME/.local/bin).
                        Env: REPOGI_INSTALL_DIR
  --verify-attestation  Also verify GitHub build provenance with the gh CLI.
  -h, --help            Show this help.

REPOGI_RELEASES_URL replaces ${OFFICIAL_RELEASES} for mirrors and
tests; it must use https (plain http is accepted only for 127.0.0.1/localhost).
EOF
}

say() { printf 'repogi-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

version="${REPOGI_VERSION:-}"
install_dir="${REPOGI_INSTALL_DIR:-}"
releases="${REPOGI_RELEASES_URL:-$OFFICIAL_RELEASES}"
verify_attestation=0

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
        --version=*) version="${1#*=}"; shift ;;
        --install-dir) [ $# -ge 2 ] || die "--install-dir needs a value"; install_dir="$2"; shift 2 ;;
        --install-dir=*) install_dir="${1#*=}"; shift ;;
        --verify-attestation) verify_attestation=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done

# --- Download source --------------------------------------------------------

releases="${releases%/}"
case "$releases" in
    https://*) proto="=https" ;;
    http://127.0.0.1/*|http://127.0.0.1:*|http://localhost/*|http://localhost:*) proto="=http,https" ;;
    *) die "REPOGI_RELEASES_URL must be an https URL: $releases" ;;
esac
[ "$releases" = "$OFFICIAL_RELEASES" ] || say "warning: downloading from $releases instead of the official GitHub Release"

if command -v curl >/dev/null 2>&1; then
    downloader=curl
elif command -v wget >/dev/null 2>&1; then
    downloader=wget
else
    die "curl or wget is required"
fi

fetch() {
    if [ "$downloader" = curl ]; then
        curl --proto "$proto" --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"
    else
        wget -q --tries=3 -O "$2" "$1"
    fi
}

resolve_latest() {
    if [ "$downloader" = curl ]; then
        final=$(curl --proto "$proto" --tlsv1.2 -fsSLI -o /dev/null -w '%{url_effective}' "$releases/latest") || final=""
    else
        # GNU Wget prints headers on stderr, Wget2 on stdout; GitHub's first Location is the tag URL.
        final=$(wget -S --spider --max-redirect=0 "$releases/latest" 2>&1 | sed -n 's/^ *[Ll]ocation: *//p' | head -n 1 | tr -d '\r') || final=""
    fi
    case "$final" in
        */releases/tag/v*) printf '%s\n' "${final##*/tag/v}" ;;
        *) die "no stable Repogi release is published yet (or GitHub could not be reached). Install a specific release with --version VERSION; see ${OFFICIAL_RELEASES}" ;;
    esac
}

# --- Platform ---------------------------------------------------------------

version_ge() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        n = split(a, x, "."); m = split(b, y, "."); if (m > n) n = m
        for (i = 1; i <= n; i++) { if (x[i] + 0 > y[i] + 0) exit 0; if (x[i] + 0 < y[i] + 0) exit 1 }
        exit 0 }'
}

check_glibc() {
    libc=$(getconf GNU_LIBC_VERSION 2>/dev/null || true)
    case "$libc" in
        glibc\ *) ;;
        *)
            if ldd --version 2>&1 | grep -qi musl; then
                die "musl-based Linux (for example Alpine) is not supported by the prebuilt binaries, which need glibc $GLIBC_MIN or newer. Build from source instead: https://github.com/${REPOSITORY}#build-from-source"
            fi
            die "could not determine the C library version; the prebuilt binaries need glibc $GLIBC_MIN or newer. Build from source instead: https://github.com/${REPOSITORY}#build-from-source" ;;
    esac
    glibc="${libc#glibc }"
    version_ge "$glibc" "$GLIBC_MIN" || die "glibc $glibc is older than $GLIBC_MIN, which the prebuilt binaries need (Ubuntu 22.04+, Debian 12+, RHEL/Rocky/Alma 9+, Fedora 35+). Build from source instead: https://github.com/${REPOSITORY}#build-from-source"
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux)
            if [ "$(getconf LONG_BIT 2>/dev/null || echo 64)" = 32 ]; then
                die "32-bit Linux userland detected ($arch kernel); prebuilt binaries are 64-bit only"
            fi
            case "$arch" in
                x86_64|amd64) target=x86_64-unknown-linux-gnu ;;
                aarch64|arm64) target=aarch64-unknown-linux-gnu ;;
                *) die "unsupported Linux CPU architecture: $arch (prebuilt: x86_64, aarch64). Build from source instead: https://github.com/${REPOSITORY}#build-from-source" ;;
            esac
            check_glibc ;;
        Darwin)
            case "$arch" in
                arm64|aarch64) target=aarch64-apple-darwin ;;
                x86_64)
                    # A shell under Rosetta reports x86_64 on Apple silicon; prefer the native build.
                    if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
                        target=aarch64-apple-darwin
                    else
                        target=x86_64-apple-darwin
                    fi ;;
                *) die "unsupported macOS CPU architecture: $arch (prebuilt: arm64, x86_64)" ;;
            esac ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT)
            die "no prebuilt Windows binary yet; build from source: https://github.com/${REPOSITORY}#build-from-source" ;;
        *)
            die "unsupported operating system: $os (prebuilt: Linux, macOS). Build from source instead: https://github.com/${REPOSITORY}#build-from-source" ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 -r "$1" | awk '{print $1}'
    else
        die "sha256sum, shasum or openssl is required to verify the download"
    fi
}

# --- Install ----------------------------------------------------------------

tmp=""
staged=""
created_dir=""
cleanup() {
    [ -z "$staged" ] || rm -f "$staged"
    [ -z "$tmp" ] || rm -rf "$tmp"
    # Remove only directories this run created, and only while they are empty.
    if [ -n "$created_dir" ]; then
        d="$install_dir"
        while rmdir "$d" 2>/dev/null && [ "$d" != "$created_dir" ]; do
            d=$(dirname "$d")
        done
    fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

detect_target

if [ -z "$version" ]; then
    version=$(resolve_latest)
    say "latest stable release: $version"
fi
version="${version#v}"
printf '%s\n' "$version" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$' \
    || die "invalid version: $version (expected MAJOR.MINOR.PATCH, optionally -PRERELEASE)"

if [ -z "$install_dir" ]; then
    [ -n "${HOME:-}" ] || die "HOME is not set; pass --install-dir"
    install_dir="$HOME/.local/bin"
fi
case "$install_dir" in
    /*) ;;
    *) install_dir="$(pwd)/$install_dir" ;;
esac

stem="repogi-${version}-${target}"
archive="${stem}.tar.gz"
base_url="${releases}/download/v${version}"

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t repogi-install) || die "could not create a temporary directory"
say "downloading $archive"
fetch "$base_url/SHA256SUMS" "$tmp/SHA256SUMS" || die "could not download SHA256SUMS for release $version from $base_url (does the release exist?)"
fetch "$base_url/$archive" "$tmp/$archive" || die "could not download $archive from $base_url (is $target published for $version?)"

expected=$(awk -v n="$archive" '$2 == n || $2 == "*" n { print $1 }' "$tmp/SHA256SUMS")
case "$expected" in
    *[!0-9a-f]*|"") die "SHA256SUMS has no valid entry for $archive" ;;
esac
[ "${#expected}" -eq 64 ] || die "SHA256SUMS has no valid entry for $archive"
actual=$(sha256_of "$tmp/$archive")
[ "$actual" = "$expected" ] || die "checksum mismatch for $archive (expected $expected, got $actual); nothing was installed"
say "verified SHA-256 $actual"

if [ "$verify_attestation" = 1 ]; then
    command -v gh >/dev/null 2>&1 || die "--verify-attestation needs the GitHub CLI (gh)"
    gh attestation verify "$tmp/$archive" --repo "$REPOSITORY" >&2 || die "build provenance verification failed for $archive"
fi

# Refuse anything but plain files and directories under the expected top-level directory.
tar -tzf "$tmp/$archive" > "$tmp/members" || die "$archive is not a readable gzip tar archive"
tar -tvzf "$tmp/$archive" > "$tmp/types" || die "$archive is not a readable gzip tar archive"
while IFS= read -r member; do
    case "$member" in
        /*|*\\*|..|../*|*/..|*/../*) die "unsafe path in archive: $member" ;;
        "$stem"/|"$stem"/*) ;;
        *) die "unexpected path in archive: $member" ;;
    esac
done < "$tmp/members"
while IFS= read -r line; do
    case "$line" in
        -*|d*) ;;
        *) die "archive contains a link or special file: $line" ;;
    esac
    case "$line" in
        *" -> "*|*" link to "*) die "archive contains a link: $line" ;;
    esac
done < "$tmp/types"
grep -Fqx "$stem/repogi" "$tmp/members" || die "$archive does not contain $stem/repogi"

mkdir "$tmp/extract"
tar -xzf "$tmp/$archive" -C "$tmp/extract" "$stem/repogi" || die "could not extract repogi from $archive"
binary="$tmp/extract/$stem/repogi"
[ -f "$binary" ] && [ ! -L "$binary" ] || die "extracted repogi is not a regular file"

if [ ! -d "$install_dir" ]; then
    parent="$install_dir"
    while [ ! -d "$parent" ]; do
        created_dir="$parent"
        parent=$(dirname "$parent")
    done
    mkdir -p "$install_dir" || die "could not create $install_dir"
fi
[ -w "$install_dir" ] || die "$install_dir is not writable; choose another directory with --install-dir (no sudo needed)"
destination="$install_dir/repogi"
if [ -d "$destination" ] && [ ! -L "$destination" ]; then
    die "$destination is a directory; refusing to replace it"
fi

staged="$install_dir/.repogi.install.$$"
cp "$binary" "$staged" || die "could not write to $install_dir"
chmod 755 "$staged"
reported=$("$staged" --version 2>/dev/null) || die "the downloaded repogi does not run on this system; nothing was installed"
[ "$reported" = "repogi $version" ] || die "the downloaded executable reports '$reported', expected 'repogi $version'; nothing was installed"
mv -f "$staged" "$destination" || die "could not install to $destination"
staged=""
created_dir=""

say "installed $reported to $destination"
case ":${PATH:-}:" in
    *":$install_dir:"*)
        found=$(command -v repogi 2>/dev/null || true)
        if [ -n "$found" ] && [ "$found" != "$destination" ]; then
            say "note: $found comes first on PATH and will run instead of $destination"
        fi ;;
    *)
        say "$install_dir is not on your PATH. Add it, for example:"
        case "${SHELL:-}" in
            */zsh) say "  echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.zshrc" ;;
            */fish) say "  fish_add_path '$install_dir'" ;;
            *) say "  echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.bashrc   # or your shell's profile" ;;
        esac
        say "then open a new shell and run: repogi --version" ;;
esac
