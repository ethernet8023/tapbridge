# tapbridge

A remote Android NFC reader, exposed to Windows as a USB/IP CCID reader. Physical card performs all authentication. APDUs and response status words are preserved byte-for-byte. No CTAP interpretation or software credentials.

## Non-destructive construction

The replacement occupies `2fa-sync`. The complete original sources, artifacts and settings are preserved at `../2fa-sync-before-rebuild`. Recovered source/session notes remain in that archive under `.hermes/plans/`; they are historical evidence, not protocol authority. No commits or pushes have been made.

## Modules

- `tapbridge-core`: bounded versioned wire frames, pairing authorization, encrypted iroh endpoints, identity/invitation encoding. No platform UI, USB or global state.
- `tapbridge-host`: USB/IP/CCID reader, host identity/pairing persistence and CLI. USB/IP loopback only. Connect peer state and APDU operations to the reader via bounded channels. No raw TCP phone listener.
- `tapbridge-phone`: Rust JNI endpoint/session runtime. Kotlin Activity/foreground service owns NFC and notifications. Single native library; no C++ wrapper. Kotlin keeps a session handle; all callbacks carry session/card generation.

## Shared wire contract (frozen for parallel implementation)

Rust `tapbridge_core::wire::Frame` variants:
- `Hello { capability: Option<[u8; 32]> }` (phone first; authenticated iroh peer supplies its identity).
- `Accepted` and `Rejected` (host; no error details or capability echoes).
- `Card { generation: u64, atr: Vec<u8>, max_apdu: u32 }` (phone; empty ATR = absent, max_apdu=0 then; each tag replacement/removal changes generation).
- `Apdu { id: u64, generation: u64, data: Vec<u8> }` (host).
- `Response { id: u64, generation: u64, data: Vec<u8> }` (phone; includes SW1/SW2).
- `Failed { id: u64, generation: u64 }` (phone, transport/card failure, not an APDU status).
- `Cancel { id: u64 }` (host; cancel pending operation).
- `ReadCard` (host user-requested reminder; not a fabricated card-presence event).

`wire::read_frame<R: tokio::io::AsyncRead + Unpin>(&mut R) -> std::io::Result<Frame>` and corresponding `write_frame<W: AsyncWrite + Unpin>(&mut W, &Frame)`. Maximum APDU 65536 bytes, ATR <=33 bytes, total frame <=65600 bytes. Reject wrong direction at session owner. Strict reject unknown version/kind, trailing fixed-frame bytes and overlength before allocation. No Debug/Display leaking Hello capability or APDU bytes. Dedicated reader owns read_frame across select events; cancellation abandons the whole stream.

ALPN constant `tapbridge_core::ALPN = b"tapbridge/1"`. One long-lived bidirectional QUIC stream per authenticated session, opened by phone sending Hello first. One paired phone, one active session, one physical tag and one in-flight APDU. Concurrent unrelated dialers must not block USB enumeration.

## Pairing and persistence

Host identity in private application state, DPAPI wrapped on Windows; Android Keystore-wrapped in app-private storage, backups disabled. A missing identity generates one; corrupt existing state fails loudly, never silently changes identity. Pairing only on explicit host request, using host endpoint ID and a 32-byte CSPRNG capability in a QR invitation. Invitation lives in host memory for 5 minutes, single-use; minting another revokes previous. Re-pair is explicit. No custom token signatures or custom encryption. Iroh authenticates endpoint identity; public relays see traffic metadata, not plaintext APDUs.

Core pairing gate is pure in-memory state; caller persists the authenticated phone ID BEFORE sending Accepted. Persist failure rejects and cannot claim pairing success. A lost acknowledgement is recoverable by pinned identity. No tokens/PINs/APDUs/credential material in logs. Reject unknown peers, wrong/expired/replayed capability; reject second active session without replacing first.

## Reader invariants

CCID bit 0 present, bit 1 changed. Event-driven interrupt IN with notification before/after pending read handled. One bounded pending request, cancel-safe complete USB/IP packet readers, UNLINK, valid ATR, strict descriptors matching capabilities. Phone connection is not card insertion. On disconnect, deactivation or tag loss: absent, retire generation, fail pending operation. Never carry response to next tag/session. CCID errors are failures, not invented 9000. USB/IP loop never waits synchronously for NFC.

## Android lifecycle

NFC reader mode requires a resumed Activity. Background foreground-service connection can notify the user to open the app, not silently bypass lockscreen/background restrictions. User starts/stops the connection explicitly; on notification tap the reader opens. Do not claim generic CCID polling identifies a WebAuthn ceremony. Rust iroh JNI DNS context must initialize with application context before endpoint creation. Worker threads run IsoDep, UI does not block. Close IsoDep to cancel, ignore late generation responses. Native runtime shutdown waits off the UI thread and frees JNI references.

## Verification

Build and test real Rust encrypted endpoints with temporary identities and loopback port 0; exact byte protocol and malformed cases; reader black-box USB/IP tests, timeouts/cancel/disconnect. Run fmt/clippy. Build real Windows ARM64 executable and Android APK, Gradle tests/lint, JNI load/ELF 16KB alignment. Real wireless phone and NFC card must separately prove PC/SC detection and WebAuthn success; currently no phone attached to ADB. Linux source portability is not a verified Linux installation.
