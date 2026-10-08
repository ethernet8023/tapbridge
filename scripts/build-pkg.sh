#!/bin/bash
# Builds the tapbridge macOS installer package (pkg).
#
# Product layout:
#   /Applications/tapbridge.app                                       (tray app)
#   /usr/local/libexec/SmartCardServices/drivers/tapbridge.bundle   (IFD shim)
#
# /usr/local/bin/tapbridge is NOT installed: the .app's inner binary is the
# daemon; the tray's "start at login" writes a LaunchAgent pointing at it.
#
# Usage: scripts/build-pkg.sh [--sign <SIGNER_IDENTITY>]
# Requirements: Xcode 26+ tools (clang, pkgbuild, productbuild, actool,
#   Icon Composer) and a prior
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
mkdir -p "$STAGE/Applications"

# --- tapbridge.app -------------------------------------------------------
APP="$STAGE/Applications/tapbridge.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$DAEMON" "$APP/Contents/MacOS/tapbridge"
chmod 755 "$APP/Contents/MacOS/tapbridge"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>tapbridge</string>
    <key>CFBundleDisplayName</key><string>tapbridge</string>
    <key>CFBundleIdentifier</key><string>computer.ethernet.tapbridge</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleExecutable</key><string>tapbridge</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundleIconName</key><string>tapbridge</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
# App icon: Liquid Glass Assets.car (light/dark/tinted, macOS 26+) plus a
# flat AppIcon.icns fallback, both from assets/brand/macos/tapbridge.icon.
scripts/build-macos-icon.sh "$APP/Contents/Resources"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# --- IFD shim bundle ------------------------------------------------------
cp -R "$BUNDLE" "$STAGE/usr/local/libexec/SmartCardServices/drivers/tapbridge.bundle"

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
    --scripts scripts/pkg-scripts \
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
