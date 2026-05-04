# Building `file_system_a_star`

This document explains how to build release artifacts for every supported platform. The simplest path is `scripts/build_release.sh`, which handles target installation, cross-compilers, and stripping.

## Contents

- [Supported platforms](#supported-platforms)
- [Quick start](#quick-start)
- [Prerequisites](#prerequisites)
- [Per-platform native builds](#per-platform-native-builds)
- [Cross-compilation matrix](#cross-compilation-matrix)
- [Verifying a build](#verifying-a-build)
- [Cleaning up](#cleaning-up)

## Supported platforms

| Target triple                  | Platform                  | Compatibility                          |
|--------------------------------|---------------------------|----------------------------------------|
| `x86_64-unknown-linux-gnu`     | Linux x86_64 (glibc)      | glibc ≥ 2.17 (CentOS 7+, Ubuntu 14.04+)|
| `x86_64-unknown-linux-musl`    | Linux x86_64 (static)     | Any 64-bit Linux kernel                |
| `x86_64-pc-windows-msvc`       | Windows x86_64 (MSVC)     | Windows 7 SP1 through Windows 11       |
| `aarch64-apple-darwin`         | macOS Apple Silicon       | macOS 11+                              |
| `x86_64-apple-darwin`          | macOS Intel               | macOS 10.12+                           |

## Quick start

```bash
# Build everything that can be built on the current host
scripts/build_release.sh

# Or a specific target
scripts/build_release.sh linux-musl
scripts/build_release.sh windows
scripts/build_release.sh macos-arm
```

Artifacts land in `./dist/` with versioned names:

```
file_system_a_star-0.6.0-alpha-x86_64-unknown-linux-gnu
file_system_a_star-0.6.0-alpha-x86_64-unknown-linux-musl
file_system_a_star-0.6.0-alpha-x86_64-pc-windows-msvc.exe
```

## Prerequisites

The native target (your host) needs only Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Cross-compilation needs additional tooling depending on which targets you want:

### `cargo-xwin` — for Windows from Linux/macOS

```bash
cargo install cargo-xwin --version '^0.18' --locked
```

On first use it downloads the Microsoft CRT and Windows SDK headers (~700 MB) into `~/.xwin/`. License acceptance is required by Microsoft; pass `XWIN_ACCEPT_LICENSE=1` to auto-accept (the script does this for you).

### `cargo-zigbuild` + Zig — for Linux musl, and for macOS-from-elsewhere

```bash
cargo install cargo-zigbuild --version '^0.20' --locked
```

Zig itself is a separate download (45 MB). Two options:

**Option 1 — standalone tarball** (recommended; no system Python/pip dependency):

```bash
mkdir -p ~/.local/share/zig ~/.local/bin
cd /tmp
wget https://ziglang.org/download/0.13.0/zig-linux-x86_64-0.13.0.tar.xz
tar -xJf zig-linux-x86_64-0.13.0.tar.xz
cp -r zig-linux-x86_64-0.13.0/* ~/.local/share/zig/
ln -sf ~/.local/share/zig/zig ~/.local/bin/zig
# add ~/.local/bin to PATH if it isn't already
```

**Option 2 — pip** (if you have a non-system-managed Python):

```bash
pip install --user ziglang
```

Verify:

```bash
zig version
```

## Per-platform native builds

These commands work on the matching host with no extra cross-compile tooling:

| Host                        | Command                                                      |
|-----------------------------|--------------------------------------------------------------|
| Linux                       | `cargo build --release`                                      |
| macOS Apple Silicon         | `cargo build --release --target aarch64-apple-darwin`        |
| macOS Intel                 | `cargo build --release --target x86_64-apple-darwin`         |
| Windows (MSVC) on Windows   | `cargo build --release` (with the MSVC build tools installed)|

The release profile uses `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, and `strip = true` for maximum runtime performance and minimum binary size.

## Cross-compilation matrix

What can be built from each host without manual SDK fetching:

| From host \ to target          | linux-gnu | linux-musl | windows | macos-arm | macos-x86 |
|--------------------------------|:---------:|:----------:|:-------:|:---------:|:---------:|
| **Linux x86_64**               |    ✅     |    ✅      |   ✅    |    ❌     |    ❌     |
| **macOS** (any)                |    ✅¹    |    ✅¹     |   ✅²   |    ✅     |    ✅     |
| **Windows x86_64**             |    ✅¹    |    ✅¹     |   ✅    |    ❌     |    ❌     |

¹ via `cargo-zigbuild`
² via `cargo-xwin`

**macOS targets from non-Mac hosts** require Apple's SDK (specifically `CommonCrypto/`, `mach/`, and other system headers used transitively by `mimalloc`). The SDK is not bundled with Zig; you'd need to either build natively on a Mac or fetch the Xcode Command Line Tools onto your build host. The terms under which you may copy the SDK between machines are governed by Apple's developer agreement; consult Apple's licensing before doing this.

The build script automatically skips macOS targets on non-Mac hosts and tells you what to do.

## Verifying a build

After building, the artifact in `dist/` should:

```bash
# Linux GNU — dynamically linked
file dist/file_system_a_star-*-x86_64-unknown-linux-gnu
# expect: ELF 64-bit LSB ... dynamically linked ... stripped

# Linux musl — statically linked, runs anywhere
file dist/file_system_a_star-*-x86_64-unknown-linux-musl
# expect: ELF 64-bit LSB ... statically linked ... stripped

# Windows
file dist/file_system_a_star-*-windows-msvc.exe
# expect: PE32+ executable for MS Windows ... x86-64

# Smoke-test (host-runnable artifacts only)
./dist/file_system_a_star-*-x86_64-unknown-linux-gnu --version
# expect: file_system_a_star 0.6.0-alpha — UnityAILab (contact@unityailab.com)
```

Generate SHA-256 checksums for distribution:

```bash
sha256sum dist/* > dist/SHA256SUMS
```

## Cleaning up

```bash
cargo clean                  # clears target/ for all targets
rm -rf dist/                 # release artifacts
rm -rf ~/.xwin/              # Microsoft CRT / SDK cache (~700 MB)
rm -rf ~/.cache/cargo-zigbuild/  # zigbuild's cached scripts
cargo uninstall cargo-xwin cargo-zigbuild  # remove the cross-compile wrappers
```

Zig itself, if you installed it via the standalone tarball:

```bash
rm -rf ~/.local/share/zig ~/.local/bin/zig
```
