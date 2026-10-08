#!/bin/bash
# Compiles the macOS app icon from the Icon Composer document
# assets/brand/macos/tapbridge.icon into a .app's Contents/Resources:
#
#   Assets.car     Liquid Glass icon with light, dark and tinted/clear
#                  (monochrome) appearances; macOS 26+ reads it through
#                  CFBundleIconName=tapbridge.
#   AppIcon.icns   flat full-colour fallback for macOS 13-15
#                  (CFBundleIconFile=AppIcon), 16-1024 px.
#
# Usage: scripts/build-macos-icon.sh <App.app/Contents/Resources>
# Requires Xcode 26+ (actool .icon support, Icon Composer's ictool).
set -euo pipefail
cd "$(dirname "$0")/.."

RES="${1:?usage: $0 <App.app/Contents/Resources>}"
# actool resolves relative input paths against its own daemon, not $PWD.
ICON="$PWD/assets/brand/macos/tapbridge.icon"
DEV="$(xcode-select -p)"
ICTOOL="$DEV/../Applications/Icon Composer.app/Contents/Executables/ictool"
if [[ ! -x "$ICTOOL" ]]; then
    echo "error: Icon Composer ictool not found; install Xcode 26 or newer" >&2
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$RES"
RES="$(cd "$RES" && pwd)"

# Assets.car (+ actool's own small icns, unused: ours covers 512@2x).
xcrun actool "$ICON" \
    --compile "$WORK" \
    --platform macosx \
    --minimum-deployment-target 13.0 \
    --app-icon tapbridge \
    --output-partial-info-plist "$WORK/partial.plist" \
    --errors --warnings >/dev/null
cp "$WORK/Assets.car" "$RES/Assets.car"

# Legacy icns: Icon Composer renders edge to edge, so inset each size onto
# Apple's macOS grid (824/1024 artwork, centred) like actool does.
ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"
render() { # <pixels> <out.png>
    local px="$1" body
    body=$(( (px * 824 + 512) / 1024 ))
    "$ICTOOL" "$ICON" --export-image --output-file "$WORK/body.png" \
        --platform macOS --rendition Default \
        --width "$body" --height "$body" --scale 1 >/dev/null
    # sips pads with transparent pixels around the centre.
    sips --padToHeightWidth "$px" "$px" "$WORK/body.png" --out "$2" >/dev/null
}
for pt in 16 32 128 256 512; do
    render "$pt" "$ICONSET/icon_${pt}x${pt}.png"
    render $((pt * 2)) "$ICONSET/icon_${pt}x${pt}@2x.png"
done
iconutil -c icns "$ICONSET" -o "$RES/AppIcon.icns"
