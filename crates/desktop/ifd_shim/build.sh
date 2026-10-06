#!/bin/bash
# Builds the tapbridge IFD bundle: compiles the IFDHandler shim (C, cdylib)
# and assembles tapbridge.bundle/Contents/{MacOS,Info.plist}.
#
# Output: crates/desktop/ifd_shim/dist/tapbridge.bundle
set -euo pipefail
cd "$(dirname "$0")"

CC="${CC:-clang}"
DIST="dist"
BUNDLE="$DIST/tapbridge.bundle"

rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS"

$CC -dynamiclib \
    -O2 -fPIC \
    -Wall -Wextra \
    -install_name @rpath/ifd_tapbridge.dylib \
    -o "$BUNDLE/Contents/MacOS/ifd_tapbridge.dylib" \
    ifd_shim.c

cp Info.plist "$BUNDLE/Contents/Info.plist"

# Sanity: the bundle must expose the IFDHandler symbols pcscd resolves.
nm -gU "$BUNDLE/Contents/MacOS/ifd_tapbridge.dylib" | grep -q IFDHCreateChannel
nm -gU "$BUNDLE/Contents/MacOS/ifd_tapbridge.dylib" | grep -q IFDHICCPresence
nm -gU "$BUNDLE/Contents/MacOS/ifd_tapbridge.dylib" | grep -q IFDHTransmitToICC
nm -gU "$BUNDLE/Contents/MacOS/ifd_tapbridge.dylib" | grep -q IFDHPowerICC
nm -gU "$BUNDLE/Contents/MacOS/ifd_tapbridge.dylib" | grep -q IFDHGetCapabilities
nm -gU "$BUNDLE/Contents/MacOS/ifd_tapbridge.dylib" | grep -q IFDHControl
nm -gU "$BUNDLE/Contents/MacOS/ifd_tapbridge.dylib" | grep -q IFDHCreateChannelByName

# Sanity: the plist must parse.
plutil -lint "$BUNDLE/Contents/Info.plist" >/dev/null

echo "built $BUNDLE"
