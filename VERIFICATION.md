# Verified development build

- Rust: 93 tests pass; `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo fmt --all -- --check` pass.
- Android: 6 JVM tests pass; `testDebugUnitTest lintDebug assembleDebug` succeeds. Lint has non-fatal warnings; no lint errors. Tests exercise real invitation/ATR/gate code, not mirrored toy lifecycle models.
- Release Windows ARM64 host: built and run after moving to the final project path.
- Real usbip-win2 attach: succeeded; Windows reports `Microsoft Usbccid Smartcard Reader (WUDF)` / Status OK. Detached exact temporary port; `usbip port` empty afterward.
- Android ARM64 APK: one native `.so`; ELF LOAD alignment 16384; `zipalign -c -P 16 4` succeeds.
- Build artifacts are in `dist/`. Debug APK is development-signed, not a production release.

## Not verified

No Android phone is connected to this build machine's ADB server. Actual on-device JNI loading, wireless pairing, NFC reads and full FIDO registration/assertion have not been exercised together. Reader enumeration and protocol fixtures do not establish that result. NFC-B is unsupported; NFC reader mode requires the Activity to remain resumed. Physical removal between commands may not be reported until NFC I/O fails or the Activity pauses.

## Preservation

Original project contents are retained at `../2fa-sync-before-rebuild`. The replacement was built and tested again after the approved cutover. No git history existed and no commits/pushes were made. Old unshipped agent scratch notes remain in `../2fa-sync-rebuild/.hermes`, outside the replacement.
