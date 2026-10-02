# tapbridge

Use an Android phone as the NFC reader for a Windows PC. The physical FIDO card remains the authenticator. APDUs and response status words cross an authenticated, encrypted iroh connection unchanged.

## Build

Requirements: Rust 1.91+, Windows C++ build tools, LLVM clang on Windows ARM64, JDK 17, Android SDK 35, NDK 27.2.12479018, and cargo-ndk. The Android target is `aarch64-linux-android`.

- Host: `powershell -File scripts/build-host.ps1 -Release`
- Android: `powershell -File scripts/build-android.ps1`
- Rust tests: `powershell -File scripts/build-host.ps1 -Test`
- Android tests/lint: from `android`, `gradlew.bat testDebugUnitTest lintDebug`

Gradle builds the Rust phone library before packaging it; a stale prebuilt library is not a source dependency. Debug APK output is `android/app/build/outputs/apk/debug/app-debug.apk`. Host output is `target/release/tapbridge-host.exe`.

## Pair and use

1. Run `tapbridge-host pair`. Open the private HTML pairing page whose path it prints. The invitation expires after five minutes and can pair one phone.
2. Install the debug APK on your Android phone. Paste the invitation into tapbridge and connect. Keep the host running.
3. With usbip-win2 installed, attach once: `usbip attach -r 127.0.0.1 -b 1-1 --once`. Windows sees a CCID smart-card reader.
4. Open tapbridge on the phone, tap and hold an NFC-A ISO-DEP FIDO card, and use the security-key flow on Windows.

On later starts use `tapbridge-host run` and reconnect the paired phone. `tapbridge-host forget` explicitly revokes the pairing while retaining the host identity. Stop the host before changing its saved pairing. Use `usbip port` to find the attached port and `usbip detach -p N` to detach it.

Host state lives in `%LOCALAPPDATA%/tapbridge`, or the explicit `--state-dir` directory. Windows identity/pairing is DPAPI protected; Android identity is Android Keystore wrapped and excluded from backup. Keep invitations private. Logs must not contain keys, capabilities, PINs, APDUs, or credential data.

## Debugging a card exchange

`tapbridge-host run --trace-apdu` (off by default) prints one stderr line per APDU between PC and phone: CLA/INS, the CTAP2 command name, status words and CTAP status such as `NO_CREDENTIALS`. It never prints payload bytes, credential ids, or PIN material.

## Known issue: PIN-protected cards

On the tested card, Windows' PIN-authenticated flows (registering a key, or sites requiring user verification) stall: after the PIN is accepted, the card rejects Windows's second command with `PIN_AUTH_INVALID`. The trace shows no altered data; this looks like a Windows/card interaction, not proven. Workaround: register the card from the phone (Chrome with NFC, enter the PIN there), then sign in from Windows through tapbridge. A card that does not hold a site's credential makes Windows say "this security key doesn't look familiar".

## Limits and verification

- This is a development build, not a signed production release or an audited authentication product.
- NFC reader mode requires the Android Activity to be open. A foreground-service notification does not bypass the lockscreen or launch an Activity automatically.
- NFC-A ISO-DEP is supported. NFC-B is explicitly rejected. Tag loss is reported when Android NFC I/O fails or the Activity is paused; immediate physical-removal detection between commands is not guaranteed.
- USB/IP listens on loopback only. Wireless pairing uses iroh endpoint identities and public relay/discovery infrastructure; relays can observe traffic metadata, not plaintext card commands.
- Windows ARM64 release executable and Android ARM64 APK have been built. Native ELF and APK alignment have been checked for 16 KB pages.
- A real usbip-win2 attachment enumerated `Microsoft Usbccid Smartcard Reader (WUDF)` with status OK, VID 1209/PID 7A71; the verification attachment was detached afterward.
- Full wireless phone/card authentication has not been verified. No phone is currently attached to the build machine's ADB server. Protocol tests are not proof of a completed FIDO ceremony.

See `DESIGN.md` for the replacement design. The previous implementation and recovered historical notes are preserved separately rather than mixed into the new runtime.
