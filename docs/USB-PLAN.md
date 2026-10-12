# USB link (no network) + installer served by the phone

Base branch: `feat/usb` (worktree `~/tapbridge-installer`). The shared seam is
already committed: `crates/core/src/usb.rs` (AOA identity constants, the
signed handshake, and `pump()` that bridges a blocking USB endpoint pair to a
tokio stream). Read its module docs first; they are the contract.

## Why

tapbridge connects phone↔PC over iroh (network). Some PCs have no usable
network (offline, locked-down Wi-Fi, no relay reachability). Plugging the phone
in with a USB cable must work with no network at all, using Android Open
Accessory (AOA). No root on the phone. The PC still runs tapbridge.
Reference implementation (MIT, may borrow with credit):
`~/.hermes/cache/scratch/android-nfc-bridge` (`src/usb.rs`,
`android/.../AccessorySession.kt`, `BridgeActivity.kt`,
`platform/windows/accessory-driver.ps1`, `docs/protocol.md`, `docs/hardware-test.md`).

## The link, end to end

1. PC (USB host) sees a phone-like USB device (MTP/PTP/ADB interface), sends AOA
   requests 51/52/53 with `tapbridge_core::usb::aoa::STRINGS`. The phone
   re-enumerates as 18d1:2d00/2d01(/2d04/2d05).
2. Android launches tapbridge via `USB_ACCESSORY_ATTACHED` (accessory filter
   matches manufacturer/model/version = `aoa::MANUFACTURER/MODEL/VERSION`).
3. PC claims the accessory bulk IN/OUT interface, wraps it as a
   `tapbridge_core::usb::Link`, `pump()`s it, runs `host_handshake`. Phone runs
   `phone_handshake` on the accessory fd.
4. Then ordinary `wire` frames, identical to the iroh session: phone sends
   `Hello`, host replies `Accepted`/`Rejected`, then Card/Apdu/Response/... .
   The phone's verified key from the handshake is the peer id (= its iroh
   endpoint id), so pairing state is shared with the network path.
5. Pairing over the cable: if the PC's pairing window is open, `HostProof`
   carries the capability. An UNPAIRED phone shows "Pair with <name>?" and on
   confirm sends `Hello{capability}`. The paired host record it saves uses the
   bare host id as hint, so network reconnects/wakes work afterwards too.
   A paired phone sends `Hello{capability: None}` (pinned reconnect).

Rules: one session at a time on the host. A USB session REPLACES a live iroh
session from the same pinned phone (same rule as a redial). Unplug = session
ends = card absent. Never log APDUs, keys, capabilities.

## Tracks

### Track H — host/desktop (`crates/host`, `crates/desktop`, installer)
- Service: abstract the live session so `active` can be iroh OR usb (a boxed
  `AsyncWrite` sink + close hook + reader task). Add a way for the desktop to
  hand the service a new USB stream (e.g. `ServiceHandle::attach_usb(...)` or a
  channel of accepted USB links); the service runs `host_handshake` (with the
  current outstanding capability only if the pairing gate has one — add a
  read-only `PairingGate::outstanding_capability()` in core if needed), then the
  same Hello authorization as for iroh (`remote_id` = handshake result).
- USB watcher (new module, desktop or host): `nusb` (pure Rust, preferred; check
  Windows/arm64 + macOS support) or `rusb`. Poll/hotplug; probe a phone-like
  device ONCE per plug-in (remember by location/serial until it unplugs; never
  re-probe one that failed/declined), switch to AOA, open the accessory, pump it.
  Reads must offer ≥16 KiB (`usb::CHUNK`); writes ≤16 KiB per transfer.
  Read timeouts map to `Ok(0)`.
- Windows: the accessory interface needs WinUSB. Port the reference's
  `accessory-driver.ps1` approach (Microsoft inbox usb.inf/winusb.inf via
  SetupDi, UAC prompt, scoped to the accessory device) — triggered from the app
  with a clear one-time prompt, not a terminal y/N. Untested on Windows is OK
  ("fix windows later") but it must compile for `x86_64-pc-windows-msvc` if a
  target is installed; otherwise report NOT RUN.
- Tray/status: show "Phone connected (USB)" vs network. Settings: a "Use USB
  cable" toggle (default on) persisted in host state.
- Tests: service-level test that drives a USB session over an in-memory duplex
  (no real USB): pairing via capability over USB, pinned reconnect, USB
  replaces an iroh session, unplug → card absent.

### Track P — phone (`crates/phone`, `android/`)
- `res/xml/accessory_filter.xml` + `USB_ACCESSORY_ATTACHED` intent-filter and
  meta-data on MainActivity (launchMode compatible with existing intents).
- Rust: `Session` gains a USB variant: `Session::connect_usb(fd, key, ...)`.
  Two-step so the UI can confirm pairing: `usbOpen(fd, key) -> handle` runs
  `phone_handshake` and exposes host id/name/pairingOpen; `usbHello(handle,
  usePairing, name, push)` sends Hello and awaits Accepted. After that the
  existing `read`/`card`/`response`/... JNI calls work unchanged on the handle.
  Fd ownership: Kotlin keeps the `ParcelFileDescriptor`; closing it is how
  reads get interrupted (see reference AccessorySession). Reads use a
  ≥16 KiB buffer.
- Kotlin: `TapBridgeService.connectUsb(accessory)` path alongside
  `connectAsync()`: no idle timeout while the accessory is attached; detach
  (`ACTION_USB_ACCESSORY_DETACHED`) ends the session; network retry is
  suspended while USB is live. Unknown host + pairing open → the existing
  "Pair with this computer?" dialog (name the PC); unknown host + no pairing
  window → message "Open Pair phone on <PC> first". Status shows "via USB".
- Tests: Rust unit test of the USB session against a fake host using
  `host_handshake` over a socketpair/duplex; Kotlin JVM tests for any pure logic.

### Track I — installer bundled in the APK (CI only) + served by the phone
- CI (`.github/workflows/build.yml`): android job `needs: windows`, downloads
  `tapbridge-windows-x64` and `-arm64` artifacts, copies the two
  `*-setup.exe` into `android/app/src/main/assets/installer/` before Gradle.
  That dir is gitignored. Local builds without it simply hide the feature.
- Phone: Settings → "Install tapbridge on a PC". Screen tells the user to plug
  in and turn on USB tethering (button → tether settings), then shows
  `http://<tether ip>:<port>/` (and Wi-Fi IP as fallback). A tiny HTTP server
  (no new deps; `ServerSocket`) serves a minimal page linking the installers
  present in assets, only while that screen is open. GET only, fixed paths,
  no directory listing, bind to all interfaces only while open.
- No tokens, no auto-pair, no downloads from the internet.

## Gates (each track reports separately; NOT RUN for anything not executed)
`export PATH=/opt/homebrew/bin:$PATH`; `cargo fmt --check`;
`cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.
Android: `nix develop -c scripts/build-android.sh testDebugUnitTest assembleDebug`
(needs `android/app/google-services.json`, present locally in the main checkout
at `~/tapbridge/android/app/google-services.json` — copy, never commit).
