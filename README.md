# tapbridge

Use your Android phone as the NFC reader for your Windows PC. You tap your FIDO security key (card) on the phone, and Windows signs you in as if the key were plugged into the PC. The key does all the cryptography. tapbridge only carries the card commands, unchanged, over an encrypted connection.

## How it works

- **PC:** the tapbridge tray app makes Windows see a smart-card reader (USB/IP, loopback only). When Windows asks for a security key, the app wakes your phone.
- **Phone:** the app is closed until it is needed. A push message (FCM) wakes it. It connects to your PC, shows "Tap your key", and sends the card commands both ways.
- **Relay:** a small Cloudflare Worker sends the push message. It sees only "wake phone X for PC Y", never card data.

Nothing stays connected while you are not signing in, so tapbridge does not drain the phone battery.

## Install

1. On the PC, run `tapbridge-<version>-<arch>-setup.exe` (arm64 or x64). It also installs the usbip-win2 USB/IP driver if it is missing. tapbridge starts at login and shows a tray icon.
2. On the phone, install tapbridge from Google Play.
3. On the PC, select tray icon → **Pair phone...**. On the phone, scan the QR code. The code works once and expires after 5 minutes.
4. On the phone, open **Keys** → **Add key** and tap your security key. Only registered keys open the app when the PC asks for a tap.

## Use

1. Start a sign-in that asks for a security key (passkey) on the PC.
2. Your phone shows "Tap your security key". Unlock the phone (most phones turn NFC off while locked).
3. Hold the key against the back of the phone until Windows finishes.

You can also open the phone app first, or select tray icon → **Request tap**.

## Security

- Each PC and phone has its own identity key. The PC key is protected by Windows DPAPI. The phone key is wrapped by Android Keystore and is not backed up.
- One phone can be paired with several PCs at once (say a desktop and a laptop). Each PC still accepts exactly one phone: to pair a PC with a different phone, select **Forget phone** on that PC first. On the phone, Settings → **Paired PCs** lists every paired PC with per-PC rename and **Forget**.
- The iroh connection is end-to-end encrypted. Public iroh relays and the wake relay see connection metadata only.
- Logs never contain keys, invitations, push tokens, PINs or card data.

## Limits

- Windows PCs only (usbip-win2). Android phones with NFC only.
- NFC-A ISO-DEP keys are supported. NFC-B is not.
- Android shows the tap request as a notification. It cannot open an app over the lockscreen by itself.
- A wake needs Google Play services on the phone. Without it, open the app before you sign in.
- PIN-protected keys: on the tested key, Windows PIN flows fail with `PIN_AUTH_INVALID` after the PIN is accepted (cause not proven). Workaround: register the key with the phone's browser, then sign in from Windows.

## Build

Requirements: Rust 1.91+, LLVM clang and the MSVC build tools (arm64 and x64 libraries), Inno Setup 6, JDK 17, Android SDK 36 with NDK 27.2.12479018, cargo-ndk, and Node.js for the relay.

| What | Command |
|---|---|
| Rust tests, clippy, fmt | `powershell -File scripts\check.ps1` |
| Desktop installers (`dist\`) | `$env:TAPBRIDGE_RELAY_URL='https://...'; powershell -File scripts\build-desktop.ps1 -Arch both` |
| Android tests, lint, debug APK | in `android\`: `gradlew.bat testDebugUnitTest lintDebug assembleDebug` |
| Play bundle (AAB) | in `android\`: `gradlew.bat bundleRelease` |
| Relay | see `relay\README.md` |

Release signing reads `TAPBRIDGE_UPLOAD_*` from your user `~/.gradle/gradle.properties`. A release build fails without `android/app/google-services.json`. Neither file is in the repository.

`tapbridge.exe --trace-apdu` logs one line per card command (command name and status only, never payload bytes) for debugging.

See `DESIGN.md` for the protocol and design, and `VERIFICATION.md` for what was tested and what was not.
