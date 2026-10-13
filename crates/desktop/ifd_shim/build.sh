#!/bin/bash
# Builds the tapbridge IFD bundle: compiles the IFDHandler shim (C, cdylib)
# and assembles tapbridge.bundle/Contents/{MacOS,Info.plist}.
#
# Output: crates/desktop/ifd_shim/dist/tapbridge.bundle
set -euo pipefail
cd "$(dirname "$0")"

DIST="dist"

# Linux: a plain shared object; pcscd loads it from a reader.conf.d entry
# (see docs/nixos.md), so there is no bundle or Info.plist.
if [ "$(uname -s)" = Linux ]; then
    CC="${CC:-cc}"
    mkdir -p "$DIST"
    $CC -shared -O2 -fPIC -Wall -Wextra -o "$DIST/libtapbridge_ifd.so" ifd_shim.c
    for symbol in IFDHCreateChannelByName IFDHICCPresence IFDHTransmitToICC \
        IFDHPowerICC IFDHGetCapabilities IFDHControl; do
        nm -gD "$DIST/libtapbridge_ifd.so" | grep -q "$symbol"
    done
    echo "built $DIST/libtapbridge_ifd.so"
    exit 0
fi

CC="${CC:-clang}"
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
# (plutil is macOS's; skipped where it's missing, e.g. a Nix build.)
if command -v plutil >/dev/null; then
    plutil -lint "$BUNDLE/Contents/Info.plist" >/dev/null
fi

echo "built $BUNDLE"
