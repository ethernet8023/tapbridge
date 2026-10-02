# Verification

Real results on the build machine (Windows 11 ARM64 build 28120), 2026-10-02. Gates that did not run are listed as NOT RUN.

## Passed

| Gate | Result |
|---|---|
| Rust workspace tests | 123 passed, 0 failed, on 3 consecutive runs (the earlier flake in `request_tap_sends_read_card_on_live_session` was a test dropping its phone streams; fixed) |
| clippy `-D warnings`, rustfmt | clean |
| Desktop release build | arm64 and x64 `tapbridge.exe`; PE subsystem 2 (GUI, no console window) |
| USB/IP attach (live app) | app auto-attached busid `1-1`; Windows shows `Microsoft Usbccid Smartcard Reader (WUDF)`, status OK |
| USB/IP release on app exit | after a hard kill, `usbip port` is empty within 1 s and the device is gone |
| Prompt watcher, positive | a real `WebAuthNAuthenticatorGetAssertion` dialog logged `wake: WebAuthn ceremony started` |
| Prompt watcher, negative | a real `Get-Credential` password prompt (same `CredentialUIBroker` host, title "Windows Security") logged nothing |
| Event-log subscription | test subscribes to the real `Microsoft-Windows-WebAuthN/Operational` channel without elevation |
| Single instance | a second launch exits and the first opens the pairing window (named event) |
| Pairing window | rendered and inspected: centered, complete QR with a 4-module quiet zone, no animation over the modules |
| Wake client | loopback HTTP server: 202 sent, 410 unregistered, 429/502 rejected, closed port = network error |
| Installers | `dist/tapbridge-0.1.0-{arm64,x64}-setup.exe` compiled; bundled usbip-win2 0.9.8.1 SHA-256 verified for both |
| Relay | 31/31 vitest tests, `tsc --noEmit`, `wrangler deploy --dry-run` |
| Android | 13/13 unit tests; lint 0 errors, 23 warnings; debug APK; release AAB and APK signed with the upload key (R8 on) |
| Android packaging (builder report) | `apksigner verify` OK; `.so` LOAD segments 16 KB aligned; `zipalign -c -P 16 4` OK |

## NOT RUN

- Relay deployed and real FCM delivery (needs `wrangler login` and the Firebase service-account secret).
- Anything on a phone: install, QR pairing, FCM wake, tap gate, NFC, UI screenshots. No device on adb; no Android emulator exists for a Windows ARM64 host; Robolectric screenshots cannot run on this JDK.
- End-to-end WebAuthn sign-in with a real key through the phone.
- Installer install/upgrade/uninstall on a clean machine (compiled only).
- Code signing: installers and exe are unsigned (no certificate).
- Tray menu clicks (Start at login toggle, Quit) were not clicked by automation. The registry code was rewritten and the toggle reads the item's real state, but the click path itself is unexercised.

## Preservation

The pre-rebuild sources stay at `../2fa-sync-before-rebuild`. Local git history starts at `d51d13f` (baseline before production work); nothing has been pushed.
