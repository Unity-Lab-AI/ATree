#!/usr/bin/env bash
#
# Build release artifacts for one or all supported platforms.
# Outputs go to ./dist/atree-<version>-<target><ext>
#
# Usage:
#   scripts/build_release.sh [target]
#
# Where [target] is one of:
#   all          (default — every host-buildable target; macOS skipped on non-Mac)
#   linux-gnu    Linux x86_64, glibc-linked (broadest binary compat for distros)
#   linux-musl   Linux x86_64, statically linked (runs on any 64-bit Linux)
#   windows      Windows x86_64 MSVC console executable
#   macos-arm    macOS Apple Silicon (must run on a Mac unless you provide an SDK)
#   macos-x86    macOS Intel (must run on a Mac unless you provide an SDK)
#
# Prerequisites are documented in BUILD.md.

set -euo pipefail

# Resolve the project root regardless of where the script was invoked from.
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
DIST="$PROJECT_ROOT/dist"
mkdir -p "$DIST"

# Make the per-user Zig install (used by cargo-zigbuild for musl/macOS targets)
# discoverable without requiring the user to edit their shell rc.
if [[ -x "$HOME/.local/bin/zig" ]]; then
    export PATH="$HOME/.local/bin:$PATH"
fi

color()    { printf '\033[%sm%s\033[0m' "$1" "$2"; }
banner()   { echo; echo "$(color '1;36' '==> ')$1"; }
success()  { echo "$(color '1;32' '    ok ')$1"; }
warn()     { echo "$(color '1;33' '    !! ')$1"; }
fail()     { echo "$(color '1;31' '    -- ')$1"; }

# Build for one target.
#   $1 target triple
#   $2 human label
#   $3 builder: "cargo" | "zigbuild" | "xwin"
#   $4 extension on the source binary ("" or ".exe")
build_target() {
    local target="$1"
    local label="$2"
    local builder="$3"
    local ext="$4"

    banner "$label  ($target)"

    # Pre-flight checks per builder.
    case "$builder" in
        cargo) ;;
        zigbuild)
            if ! command -v cargo-zigbuild >/dev/null; then
                warn "cargo-zigbuild not installed — skipping. Install with:"
                warn "    cargo install cargo-zigbuild --version '^0.20' --locked"
                return 0
            fi
            if ! command -v zig >/dev/null; then
                warn "zig not on PATH — skipping. See BUILD.md for install steps."
                return 0
            fi
            ;;
        xwin)
            if ! command -v cargo-xwin >/dev/null; then
                warn "cargo-xwin not installed — skipping. Install with:"
                warn "    cargo install cargo-xwin --version '^0.18' --locked"
                return 0
            fi
            ;;
    esac

    # Ensure the rustup target is installed.
    if ! rustup target list --installed | grep -qx "$target"; then
        echo "    installing rustup target $target ..."
        rustup target add "$target"
    fi

    # Build.
    case "$builder" in
        cargo)    cargo build --release --target "$target" ;;
        zigbuild) cargo zigbuild --release --target "$target" ;;
        xwin)     XWIN_ACCEPT_LICENSE=1 cargo xwin build --release --target "$target" ;;
    esac

    local src="target/$target/release/atree$ext"
    local dst="$DIST/atree-${VERSION}-${target}${ext}"
    cp "$src" "$dst"

    # Strip ELF binaries (Mach-O / PE are already stripped by the release profile).
    if [[ "$ext" == "" && "$(file -b "$dst")" == ELF* ]]; then
        strip "$dst" 2>/dev/null || true
    fi

    success "$dst  ($(du -h "$dst" | cut -f1))"
}

# Targets that can be built from any host.
build_native_targets() {
    build_target x86_64-unknown-linux-gnu  "Linux x86_64 (glibc)"   cargo ""
    build_target x86_64-unknown-linux-musl "Linux x86_64 (static)"  zigbuild ""
    build_target x86_64-pc-windows-msvc    "Windows x86_64 (MSVC)"  xwin ".exe"
}

# macOS targets — only buildable from a Mac unless the user has provided
# a macOS SDK to Zig (see BUILD.md for the manual SDK route).
build_macos_targets() {
    if [[ "$(uname -s)" == "Darwin" ]]; then
        build_target aarch64-apple-darwin "macOS Apple Silicon" cargo ""
        build_target x86_64-apple-darwin  "macOS Intel"         cargo ""
    else
        warn "Skipping macOS targets: host is not a Mac."
        warn "  - On a Mac:        scripts/build_release.sh macos-arm"
        warn "  - Cross-compile:   provide an SDK to Zig per BUILD.md."
    fi
}

case "${1:-all}" in
    linux-gnu)  build_target x86_64-unknown-linux-gnu  "Linux x86_64 (glibc)"   cargo "" ;;
    linux-musl) build_target x86_64-unknown-linux-musl "Linux x86_64 (static)"  zigbuild "" ;;
    windows)    build_target x86_64-pc-windows-msvc    "Windows x86_64 (MSVC)"  xwin ".exe" ;;
    macos-arm)  build_target aarch64-apple-darwin      "macOS Apple Silicon"    cargo "" ;;
    macos-x86)  build_target x86_64-apple-darwin       "macOS Intel"            cargo "" ;;
    all)
        build_native_targets
        build_macos_targets
        ;;
    -h|--help)
        sed -n '3,18p' "$0" | sed 's/^# \?//'
        exit 0
        ;;
    *)
        fail "Unknown target: $1"
        echo "Run with --help for usage."
        exit 2
        ;;
esac

echo
banner "Done. Artifacts:"
ls -lh "$DIST" | tail -n +2 | awk '{ printf "    %-12s %s\n", $5, $9 }'
