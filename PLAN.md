# tapbridge roadmap: multi-PC pairing, macOS PIV reader, hybrid authenticator

Three parallel tracks in git worktrees off `main` (565870d). Merge order: C → A → B (C touches phone identity code that A/B sit on top of; A and B are desktop-side / phone-side orthogonal after C).

Worktrees:
- `../tapbridge-c` — `feat/multi-pc-pairing` (do FIRST; other tracks rebase on it)
- `../tapbridge-a` — `feat/macos-piv`
- `../tapbridge-b` — `feat/hybrid-authenticator`

---

## Track C — Multi-PC pairing (phone holds many paired hosts)

Prerequisite for the other two tracks (a phone paired to a Windows PC + a Mac needs it).

**Principle: the wire contract v2 does NOT change.** Each PC is an independent host with its own identity key and pairing capability; the phone just stores N of them.

Changes:
- `android/.../IdentityStore.kt`: replace single `paired_host`/`host_name`/`cached_hint` prefs with a set of per-host records (JSON in one pref key): `host_id → {name, cached_hint, paired_at}`. One-time migration from the legacy keys; never silent re-key.
- `WakeService.kt`: FCM wake `{"t":"wake","host":ID,"hint":HINT}` routes to the record for that host id; unknown host → drop (as today for mismatched host).
- `TapBridgeService.kt` / `MainActivity.kt`: connect-by-host-id; UI gets a "Paired PCs" list with per-host rename/forget; wake notification names the requesting PC.
- `crates/phone/src/jni.rs`: JNI connect already takes an explicit hint string — thread the per-host hint through instead of "the" cached one.
- `WakeService`/docs: pairing QR security note becomes per-host ("forget" per host, revocation on minting unchanged).
- Host side: **no changes required**. Each PC's `PairingGate` is local.
- Update DESIGN.md ("one paired phone" section → "one phone may pair many hosts; host still pins one phone") and README.

Verification: Gradle unit tests for store migration + wake routing (Robolectric where available; JVM tests for logic). Host Rust tests untouched and must still pass.

## Track A — macOS PIV / smart-card login (virtual CCID reader)

Goal: phone-as-NFC-reader presents a PC/SC reader to macOS so PIV certs work: Safari client certs, OpenSC, **smart-card login to the Mac**. NOT WebAuthn (see Track B).

Architecture (vpcd pattern, proven on macOS 15 by vsmartcard):
- New IFD bundle installed to `/usr/local/libexec/SmartCardServices/drivers/tapbridge.bundle`: Info.plist claiming a USB VID/PID + IFDHandler v2 C-ABI shim (tiny C or C-ABI Rust, `cdylib`).
- The shim forwards IFD commands over a local Unix-domain socket to the tapbridge daemon (Rust).
- Desktop crate: `#[cfg(target_os = "macos")]` modules mirroring the Windows ones:
  - `state.rs`: DPAPI → macOS Keychain (`security` crate or raw APIs) — same TBID2 record, encrypted-at-rest.
  - `autostart.rs`: RunAtLogin registry → `~/Library/LaunchAgents/computer.ethernet.tapbridge.plist` (SMAppService on 13+ if feasible without extra entitlements; LaunchAgent file is fine).
  - `usbip.rs`/`passkeys.rs`/`prompt_watch.rs`: stubbed out on macOS (Windows-only: usbip-win2 attach, WebAuthN event log). macOS path has no ceremony watcher — the reader is always "present" to PC/SC; the phone connects on demand when PC/SC powers the card, OR the tray "Request tap" sends the wake.
  - Tray/host service/iroh/pairing: shared code, works as-is.
- Daemon ↔ phone session: identical wire v2. Reader presence model: expose the reader to PC/SC immediately at launch (zero cards); card insertion = phone session with tag present.
- Installer: a `scripts/install-macos.sh` (sudo cp of the bundle + daemon into place) and eventually a pkg; notarized Developer ID build.
- Wake on demand: on macOS there is no WebAuthN event to watch; instead the daemon sends the FCM wake through the existing relay when PC/SC asks for card status/ATR and no phone session is live (rate-limited), plus manual tray "Request tap".

Verification: `cargo test` on macOS; IFD bundle loads (`system_profiler SPSmartCardsDataType` / `pcsc_scan` from `brew install pcsc-tools`); daemon↔shim socket protocol unit tests; end-to-end `opensc-tool -r tapbridge --send-apdu` against a fake phone endpoint in tests. On-device: not in scope for the branch's CI — reported separately.

## Track B — Hybrid (caBLEv2) authenticator on the phone

Goal: WebAuthn ceremonies in Safari/Chrome on the Mac resolve by tapping the FIDO card on the phone. The CARD stays the cryptographic root — the phone mediates CTAP2 to ISO-7816 APDU relay to the card (via NFC, reader mode), no software credentials.

This is the largest track. Phases:
1. Protocol study spike: caBLEv2 / CTAP2.2 hybrid authenticator role (BLE advertisement with service data + adv secret, tunnel server WebSocket, the pinned `caBLE` handshake), then CTAP2 `authenticatorGetInfo/GetAssertion` over the tunnel.
2. Card mapping: CTAP2 over ISO-7816 (FIDO2 keys speak CTAP2 over contactless ISO-7816 with the SELECT AID 0xA0000006472F0002 + framing; this is standardized in the FIDO2 CTAP spec) — we already use SELECT A0000006472F0001 for identification.
3. Android: BLE advertiser (needs BLUETOOTH_ADVERTISE + runtime), tunnel client (WebSocket), and a service that, on invitation from the Mac (USB/BLE advertisement seen by Chrome/Safari), shows the tap UI and relays.
4. Security: hybrid pairing must not bypass the registered-keys tap gate; invites only for known/paired Macs (pair via the existing QR).
5. Desktop: nothing (Safari/Chrome handle the desktop side natively). Optionally the tapbridge Mac app can later surface "hybrid ready" status.

Verification: unit tests for the tunnel/adv framing (public test vectors from the spec), integration against a hybrid-client simulator (Chrome's caBLE client can be driven by a desktop browser on the same Mac with `chrome://flags` hybrid forced), on-device last.

---

## Merge / CI notes

- All branches must keep the shared crates (`crates/core`, `crates/host` platform code) compiling on Windows as before — guard new code with `#[cfg(target_os)`. Windows CI (per VERIFICATION.md: check.ps1) must be reported as NOT RUN on this machine; state that honestly in the branch notes.
- Worktrees are siblings of the main checkout: `~/tapbridge-a`, `~/tapbridge-b`, `~/tapbridge-c`. Build each in its own worktree (target dir is per-worktree).
- The repo must keep `scripts/check.ps1` green on Windows for the shared-code paths; macOS verification via `cargo test` + pcsc_tools.
