#!/bin/bash
# Builds tapbridge from source, packages it, installs it and launches it:
# release binary -> IFD reader bundle -> pkg -> sudo installer -> open.
# Each step stops the chain on failure, so a stale pkg is never installed.
#
# Usage: scripts/run-macos.sh [--no-install] [build-pkg.sh args...]
#   --no-install  stop after building dist/tapbridge-<ver>-macos.pkg
# In the dev shell: build-and-run-macos-pkg
set -euo pipefail
cd "$(dirname "$0")/.."

install=1
args=()
for a in "$@"; do
    if [[ "$a" == --no-install ]]; then install=0; else args+=("$a"); fi
done

# Builds with whatever toolchain is on PATH (the flake dev shell's Rust and
# compilers). Only the pkg step reaches for Xcode-only tools (actool,
# ictool, pkgbuild, productbuild, codesign); build-pkg.sh finds those itself.

echo "== cargo build --release"
cargo build --release -p tapbridge-desktop --locked
# Nix's toolchain links its own libiconv; the shipped app must only use
# system libraries (no /nix/store paths on other Macs). macOS has a
# compatible /usr/lib/libiconv.2.dylib, so point the binary there.
bin=target/release/tapbridge
for lib in $(otool -L "$bin" | awk '/\/nix\/store\// {print $1}'); do
    name="$(basename "$lib")"
    # System libraries live in the dyld shared cache, not as files.
    case "$name" in
        libiconv.2.dylib | libz.1.dylib | libc++.1.dylib | libresolv.9.dylib | libcharset.1.dylib) ;;
        *) echo "error: $bin links $lib (not a macOS system library)" >&2; exit 1 ;;
    esac
    install_name_tool -change "$lib" "/usr/lib/$name" "$bin"
done
if otool -L "$bin" | grep -q /nix/store/; then
    echo "error: $bin still links /nix/store" >&2
    exit 1
fi

echo "== IFD reader bundle"
crates/desktop/ifd_shim/build.sh
echo "== pkg"
scripts/build-pkg.sh ${args[@]+"${args[@]}"}

version="$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -1)"
pkg="dist/tapbridge-$version-macos.pkg"
[[ $install == 1 ]] || { echo "built $pkg"; exit 0; }

echo "== install (sudo)"
pkill -f tapbridge.app/Contents/MacOS/tapbridge || true
sudo installer -pkg "$pkg" -target /
open -a tapbridge
echo "tapbridge is running"
