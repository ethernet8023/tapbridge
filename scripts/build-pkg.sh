#!/bin/bash
# Builds the tapbridge macOS installer package (pkg).
#
# Product layout (root:admin, as macOS expects for system components):
#   /usr/local/libexec/SmartCardServices/drivers/tapbridge.bundle   (IFD shim)
#   /usr/local/bin/tapbridge                                         (daemon)
#
# The LaunchAgent is NOT installed by the pkg: the user enables
# "Start at login" from the tray (it lives in ~/Library/LaunchAgents and
# must point at the user's daemon), or the daemon writes it itself.
#
# Usage: scripts/build-pkg.sh [--sign <SIGNER_IDENTITY>]
# Requirements: Xcode tools (clang, pkgbuild, productbuild) and a prior
#   cargo build --release -p tapbridge-desktop
# Output: dist/tapbridge-<version>-macos.pkg
#   --sign: Developer ID Installer identity for a signed product pkg
#           (e.g. "Developer ID Installer: Your Name (TEAMID)").
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -1)"
PKG_ID="computer.ethernet.tapbridge"
OUT="dist"
SIGN_IDENTITY=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sign) SIGN_IDENTITY="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

DAEMON="target/release/tapbridge"
SHIM_DIR="crates/desktop/ifd_shim"
BUNDLE="$SHIM_DIR/dist/tapbridge.bundle"

if [[ ! -x "$DAEMON" ]]; then
    echo "error: $DAEMON missing; run: cargo build --release -p tapbridge-desktop" >&2
    exit 1
fi
if [[ ! -d "$BUNDLE/Contents" ]]; then
    echo "error: $BUNDLE missing; run: $SHIM_DIR/build.sh" >&2
    exit 1
fi

# Fresh component root with the exact installed layout.
STAGE="$OUT/pkgroot"
rm -rf "$STAGE"
mkdir -p "$STAGE/usr/local/libexec/SmartCardServices/drivers"
mkdir -p "$STAGE/usr/local/bin"

cp -R "$BUNDLE" "$STAGE/usr/local/libexec/SmartCardServices/drivers/tapbridge.bundle"
cp "$DAEMON" "$STAGE/usr/local/bin/tapbridge"
chmod 755 "$STAGE/usr/local/bin/tapbridge"

# pkgbuild: component package; `recommended` records root:wheel for system
# paths without needing sudo on the build machine.
COMPONENT="$OUT/tapbridge-components.pkg"
rm -f "$COMPONENT"
pkgbuild \
    --root "$STAGE" \
    --identifier "$PKG_ID" \
    --version "$VERSION" \
    --ownership recommended \
    --install-location "/" \
    "$COMPONENT"

# productbuild: wrap the component with the distribution definition
# (read-me / choices / requirements live in scripts/Distribution.xml).
PRODUCT="$OUT/tapbridge-$VERSION-macos.pkg"
if [[ -n "$SIGN_IDENTITY" ]]; then
    productbuild \
        --distribution scripts/Distribution.xml \
        --package-path "$OUT" \
        --version "$VERSION" \
        --sign "$SIGN_IDENTITY" \
        "$PRODUCT"
else
    productbuild \
        --distribution scripts/Distribution.xml \
        --package-path "$OUT" \
        --version "$VERSION" \
        "$PRODUCT"
fi

rm -rf "$STAGE" "$COMPONENT"

echo
echo "built $PRODUCT"
echo "install with: sudo installer -pkg $PRODUCT -target /"
echo "verify the reader: system_profiler SPSmartCardsDataType"
