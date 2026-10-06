# tapbridge

An Android phone is a remote NFC reader for a Windows PC. Windows sees a USB/IP CCID smart-card reader. The physical FIDO card performs all authentication. APDUs and status words cross an authenticated, encrypted iroh connection byte-for-byte. No CTAP interpretation on the bridge path; no software credentials.

Earlier sources stay at `../2fa-sync-before-rebuild` (historical evidence, not protocol authority). Local git baseline: `d51d13f`.

## Product shape

- **Windows desktop app** (`crates/desktop`, binary `tapbridge.exe`): tray icon (`tray-icon`), gpui windows only where UI is needed (pairing QR, status). Runs the host service, attaches the bundled usbip-win2 client to `127.0.0.1:3240` busid `1-1`, detaches on quit. Starts at login. Single instance. Supersedes the old `tapbridge-host` CLI.
- **Host library** (`crates/host`): USB/IP/CCID reader, phone session service, DPAPI-protected state. No CLI.
- **Android app** (`android/`, package `computer.ethernet.tapbridge`, Play Store): Jetpack Compose + Material 3, motion graphics, CameraX + ML Kit QR pairing, FCM wake, registered keys.
- **Phone native** (`crates/phone`): Rust JNI endpoint/session runtime. Kotlin owns NFC, lifecycle and notifications. One native library, no C++ wrapper.
- **Relay** (`relay/`): Cloudflare Worker that turns a desktop wake request into an FCM high-priority data message. It holds the FCM service account; the desktop never does.
- **Installer** (`installer/`): Inno Setup, x64 + arm64, bundles the pinned, hash-checked official usbip-win2 installer (BSD-2, WHLK-signed drivers) and runs it silently when the driver is missing.

## Nothing runs at rest

No always-on phone connection: iroh heartbeats every 5 s (`iroh/src/socket.rs` `HEARTBEAT_INTERVAL`) would keep the cellular radio awake. The phone connects on demand:

1. Trigger on the PC: Windows starts a WebAuthn ceremony, or the user picks "Request tap" in the tray. Generic CCID polling does not identify a ceremony, so the desktop subscribes (`EvtSubscribe`, no elevation) to the `Microsoft-Windows-WebAuthN/Operational` log for event 1000 (MakeCredential started) and 1003 (GetAssertion started). Measured on build 28120. The dialog window is not used as a signal: `CredentialUIBroker.exe` (class `Credential Dialog Xaml Host`, title "Windows Security") also hosts password prompts, and its content is not readable by UI Automation from another process.
2. If a phone session is already live, the host sends `ReadCard` (phone shows "tap now"). Otherwise, if a push token is stored, the desktop POSTs `{token, host, hint}` to the relay.
3. Relay sends an FCM data message (`priority: high`, `ttl: 60s`): `{"t":"wake","host":ID,"hint":HINT}`.
4. Phone routes the wake by host id: one phone may be paired with MANY hosts at once, and each wake names exactly one. The `host` id must match one of the paired hosts (anything else is dropped); the service connects using that host's hint (pushed hint, else its cached hint, else the bare host id), posts a heads-up "Tap your key for <PC>" notification naming the requesting PC, and enables the NFC tap gate for 120 s.
5. Session idles out 60 s after the last card/APDU activity (or after connect, if nothing happens) while the app is not visible; the service stops and the gate is disabled. A visible app keeps its session.

The wake message is an unauthenticated hint. Authority is always the iroh-authenticated pinned identity. A forged wake can at most make the phone dial one of its own paired PCs.

Opening the app while paired connects immediately (to the last-used host, cached hint) and, while the UI is visible, retries on drop: 0.25, 0.5, 1, 2, 3 s cap, plus immediate retry on network change.

## Registered keys and the tap gate

Android NFC dispatch filters by technology only. So: activity-alias `TapGate` (translucent, no UI) has a `TECH_DISCOVERED` IsoDep filter and is **disabled by default**. It is enabled only during a wake window. On dispatch it compares the card with registered keys, then forwards the `Tag` to `MainActivity` on match or `finish()`es silently otherwise.

A registered key stores: label, NFC UID if stable (a 4-byte UID starting `0x08` is random and is not stored), and the CTAP2 `authenticatorGetInfo` AAGUID (SELECT `A0000006472F0001`, then `80 10 00 00 01 04 00`). Match = stable UID equal, otherwise AAGUID equal. This identification exchange is local to the phone and is not part of the bridge path. Inside the open app, reader mode accepts any ISO-DEP card.

## Wire contract v2 (ALPN `tapbridge/2`, frame version byte 2)

`tapbridge_core::wire::Frame`:
- `Hello { capability: Option<[u8;32]>, name: String, push: Option<String> }` — phone first, on every connect. `name`: display name, 1..=64 UTF-8 bytes. `push`: current FCM token, 1..=4096 bytes, absent when FCM is unavailable. Host stores the newest name/token for the pinned phone. Authenticated iroh peer supplies identity.
- `Accepted { name: String }` — host display name, 1..=64 bytes. `Rejected` — no details.
- `Card { generation: u64, atr: Vec<u8>, max_apdu: u32 }` — phone; empty ATR = absent, max_apdu=0 then; each tag replacement/removal changes generation.
- `Apdu { id, generation, data }` (host), `Response { id, generation, data }` (phone; includes SW1/SW2), `Failed { id, generation }` (phone; transport/card failure, not an APDU status), `Cancel { id }` (host), `ReadCard` (host; user-visible reminder, not a fabricated presence event).

Max APDU 65536 bytes, ATR <=33, total frame <=65600. Strict decode: unknown version/kind, trailing bytes, overlength and invalid UTF-8 rejected before use. Debug redacts Hello/Apdu/Response. Wrong direction rejected at session owner. A dedicated reader owns `read_frame`; cancellation abandons the stream. One long-lived bidirectional QUIC stream per session, opened by the phone. One paired phone, one active session, one tag, one in-flight APDU. Concurrent dialers never block USB enumeration.

`tapbridge_core::wake::Hint { host: EndpointId, relay: Option<RelayUrl>, direct: Vec<SocketAddr> }`. Text form `HOSTID[;r=RELAYURL][;d=IP:PORT]*`, at most 8 direct addresses, at most 1024 bytes. `Hint::endpoint_addr()` builds the iroh `EndpointAddr`. The phone caches the last hint.

## Pairing and persistence

Invitation (QR) `tapbridge:1:HOSTID:CAPHEX`: 32-byte CSPRNG capability, 5 min, single use, minting another revokes the previous one, only on explicit user request. The core pairing gate is pure in-memory state; the caller persists the authenticated phone id BEFORE `Accepted`. Persist failure rejects. A lost acknowledgement is recoverable by pinned identity. Reject unknown peers, wrong/expired/replayed capability, and a second active session without replacing the first.

Host state is DPAPI-protected (`%LOCALAPPDATA%\tapbridge`): host key, paired phone id, phone name, push token. Phone identity is Android Keystore-wrapped, backups disabled. Missing identity generates one; corrupt state fails loudly and never silently re-keys. No keys, capabilities, push tokens, PINs or APDU bytes in logs.

**Many hosts per phone.** A phone may hold several paired hosts at once (a Windows PC and a Mac, home and office); each host is an independent peer with its own identity key and pairing capability, and each host still pins exactly ONE phone. The phone stores per-host records `host_id → {name, cached_hint, paired_at}` in one pref key; the legacy single-host prefs migrate once on first read. Forgetting is per host (Settings → Paired PCs); revocation on capability minting is unchanged. The phone's identity key and FCM token belong to the install and are shared by all hosts. The wire contract does not change.

## Relay API

`POST /v1/wake`, JSON `{"token": str<=4096, "host": str<=128, "hint": str<=1024}`. Responses: `202` FCM accepted, `400` malformed, `410` token unregistered at FCM (the phone app must connect once to send a fresh token), `429` rate limited (10/min per token; the limiter key is the SHA-256 of the token), `502` FCM/OAuth failure. Tokens are never logged. Secret: `FCM_SERVICE_ACCOUNT` (JSON); project id comes from it.

## Reader invariants

CCID bit 0 present, bit 1 changed. Event-driven interrupt IN with notification before/after pending read handled. One bounded pending request, cancel-safe complete USB/IP packet readers, UNLINK, valid ATR, strict descriptors. Phone connection is not card insertion. On disconnect, deactivation or tag loss: absent, retire generation, fail pending operation. Never carry a response to the next tag/session. CCID errors are failures, not invented 9000. The USB/IP loop never waits synchronously for NFC. See `crates/host/IMPLEMENTATION-NOTES.md`.

## Android lifecycle

Reader mode requires a resumed Activity. Background work can notify, never silently bypass the lockscreen; most devices disable NFC while locked, so the flow is wake → unlock → tap. Rust iroh JNI DNS context initializes with the application context before endpoint creation. IsoDep runs on a worker; the UI never blocks. Close IsoDep to cancel; ignore late-generation responses. Native shutdown runs off the UI thread and frees JNI references.

## Verification gates (reported separately)

Rust tests/clippy/fmt; desktop release build x64 + arm64; installer build; real usbip-win2 attach; APK/AAB build, Gradle tests, lint, 16 KB ELF alignment; relay deployed + real FCM delivery; on-device wake → tap → WebAuthn success. Gates not yet run are reported as not run.
