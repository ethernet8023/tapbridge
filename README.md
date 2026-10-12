# tapbridge

**Use your phone as the card reader for your computer.**

Some security keys and smart cards come as NFC cards, rings or implants: you use them by tapping them on something. Computers almost never have a place to tap. Phones do.

With tapbridge you tap your card on your Android phone, and your computer acts as if the card were plugged right in. Use it to sign in to websites with a passkey. On a Mac you can even log in to the computer itself with your card.

Your card does all the secret work. Your phone and computer only pass messages between each other, over a private, encrypted connection. Nothing stays connected while you aren't using it, so it won't drain your phone's battery.

![The tapbridge pairing window, showing a card and its login setup steps](docs/screenshot.png)

## Features

- **Tap to sign in.** When a website asks for a security key, your phone asks for your card. Hold it to the back of the phone and you're in.
- **Wakes your phone for you.** The phone app doesn't need to be open. Your computer sends a notification when it needs your card.
- **Log in to your Mac with your card.** tapbridge walks you through it step by step: check the card, create its login certificate on your phone, then link it to your Mac account.
- **Your cards stay in sync.** Name a card on your phone or your computer and both know it. Both show what each card can do.
- **Easy pairing.** Scan a code on your computer with your phone, then tap a card to check that everything works.
- **One phone, many computers.** Pair the same phone with your desktop and your laptop.
- **Careful with your card.** tapbridge never replaces certificates already on your card unless you clearly ask it to, and it never guesses your PIN.
- **Mac security settings in one place.** Use your card for `sudo`, `su` and SSH, lock the screen when the card goes away, or turn off passwords entirely.
- **Install the PC app from your phone.** Plug the phone into a Windows PC, turn on USB tethering, and download the installer from the phone in the PC's browser (Settings → Install tapbridge on a PC).
- **Works on macOS and Windows.** The phone app is for Android phones with NFC.

## Development

tapbridge has these parts:

| Part | Where | What it is |
|---|---|---|
| Desktop app | `crates/desktop` | Menu bar / tray app (Rust, gpui) |
| Phone app | `android/`, `crates/phone` | Android app (Kotlin) with a Rust library |
| Shared code | `crates/core`, `crates/host` | Connection, pairing, card records, the host service |
| Wake relay | `relay/` | Small Cloudflare Worker that sends the wake-up push message |

`DESIGN.md` explains how it fits together; `VERIFICATION.md` lists what has been tested.

### Setup (macOS and Linux)

The Nix flake provides everything: Rust, the Android SDK and NDK, JDK 17 and adb.

```sh
nix develop
```

### Common tasks

| What | Command |
|---|---|
| Rust tests | `cargo test --workspace` |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` |
| Build, install and launch the Mac app | `build-and-run-macos-pkg` (asks for your password to install) |
| Build the Mac installer only | `build-and-run-macos-pkg --no-install` → `dist/tapbridge-<version>-macos.pkg` |
| Desktop windows with sample data | `cargo run -p tapbridge-desktop -- --preview-ui` (set `TAPBRIDGE_PREVIEW_DIALOG` to `new`, `paired` or `about` for other screens) |
| Android tests and debug APK | `build-android testDebugUnitTest assembleDebug` |
| Install the APK on a phone | `adb install -r android/app/build/outputs/apk/debug/app-debug.apk` |
| Play bundle (AAB) | in `android/`: `./gradlew bundleRelease` |

### nix-darwin

You can install tapbridge and set all its options from your nix-darwin config instead of using the installer: see [docs/nix-darwin.md](docs/nix-darwin.md). `nix build .#tapbridge` builds the app on its own.

### Windows

You need Rust 1.91+, LLVM clang, the MSVC build tools (x64 and arm64 libraries) and Inno Setup 6.

| What | Command |
|---|---|
| Tests, lints, formatting | `powershell -File scripts\check.ps1` |
| Installers (into `dist\`) | `$env:TAPBRIDGE_RELAY_URL='https://...'; powershell -File scripts\build-desktop.ps1 -Arch both` |

`tapbridge.exe --trace-apdu` logs one line per card command (command name and status only, never card data).

### Secrets and signing

- Wake-up notifications need `android/app/google-services.json` (Firebase). It is not in the repository. Debug builds fall back to a placeholder, so wake-ups won't work; release builds fail without it.
- Android release signing reads `TAPBRIDGE_UPLOAD_*` from `~/.gradle/gradle.properties`.
- To sign the Mac installer, pass `--sign "Developer ID Installer: NAME (TEAMID)"` to the build.
- To deploy the wake relay, see `relay/README.md`.

### CI

`.github/workflows/build.yml` runs the tests and builds the Mac installer, the Windows installers (x64 and arm64) and the Android APK on every push.
