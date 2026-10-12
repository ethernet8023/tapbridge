#!/bin/bash
# Regenerates the README screenshots in docs/screenshots/ from --preview-ui's
# sample data. gpui renders each window offscreen (the `screenshots`
# feature), so this needs no Screen Recording permission and never touches
# real tapbridge state.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p tapbridge-desktop --features screenshots
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/home" docs/screenshots

shoot() { # <preview dialog> <window title slug> <output name>
    HOME="$work/home" TAPBRIDGE_PREVIEW_DIALOG="$1" TAPBRIDGE_SCREENSHOTS="$work/$1" \
        target/debug/tapbridge --preview-ui >/dev/null 2>&1
    cp "$work/$1/$2.png" "docs/screenshots/$3.png"
    echo "docs/screenshots/$3.png"
}

shoot pairing pair-phone pairing
shoot pairing tapbridge-preferences preferences
shoot paired pair-phone login-setup
shoot about about-tapbridge about
