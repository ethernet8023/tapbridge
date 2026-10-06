#!/bin/bash
# tapbridge macOS installer (Track A: PC/SC PIV reader).
#
# Installs, with sudo:
#   1. The IFD bundle  -> /usr/local/libexec/SmartCardServices/drivers/tapbridge.bundle
#      (claims USB VID 0x1209 / PID 0x7A71; com.apple.ifdreader loads it and
#      calls the shim, which forwards to the daemon's Unix socket)
#   2. The daemon      -> /usr/local/bin/tapbridge
#
# Then restarts the PC/SC reader service so the bundle is picked up, and
# prints verification commands.
#
# Usage: sudo scripts/install-macos.sh
# (build first: cargo build --release -p tapbridge-desktop &&
#               crates/desktop/ifd_shim/build.sh)
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "${EUID:-$(id -u)}" != 0 ]]; then
    echo "error: run with sudo: sudo scripts/install-macos.sh" >&2
    exit 1
fi

DRIVERS_DIR="/usr/local/libexec/SmartCardServices/drivers"
BUNDLE_SRC="crates/desktop/ifd_shim/dist/tapbridge.bundle"
DAEMON_SRC="target/release/tapbridge"
BUNDLE_DST="$DRIVERS_DIR/tapbridge.bundle"
DAEMON_DST="/usr/local/bin/tapbridge"

if [[ ! -d "$BUNDLE_SRC/Contents" ]]; then
    echo "error: $BUNDLE_SRC missing; run crates/desktop/ifd_shim/build.sh first" >&2
    exit 1
fi
if [[ ! -x "$DAEMON_SRC" ]]; then
    echo "error: $DAEMON_SRC missing; run: cargo build --release -p tapbridge-desktop" >&2
    exit 1
fi

# 1. IFD bundle
mkdir -p "$DRIVERS_DIR"
rm -rf "$BUNDLE_DST"
cp -R "$BUNDLE_SRC" "$BUNDLE_DST"
chown -R root:wheel "$BUNDLE_DST"
# pcscd must be able to load the dylib.
chmod -R a+rX "$BUNDLE_DST"

# 2. daemon binary
install -m 0755 "$DAEMON_SRC" "$DAEMON_DST"

# 3. Restart the PC/SC reader daemon so it re-enumerates drivers.
killall -SIGKILL com.apple.ifdreader 2>/dev/null || true

echo
echo "Installed:"
echo "  $BUNDLE_DST"
echo "  $DAEMON_DST"
echo
echo "Next:"
echo "  1. Start the daemon as your user:   $DAEMON_DST"
echo "     (start at login: enable it from the tray menu)"
echo "  2. Verify the reader is registered:"
echo "       system_profiler SPSmartCardsDataType"
echo "     or (brew install pcsc-tools):"
echo "       pcsc_scan"
echo "  3. Inspect driver loading problems:"
echo "       log show --predicate '(subsystem == \"com.apple.CryptoTokenKit\")' --info --debug --last 5m"
