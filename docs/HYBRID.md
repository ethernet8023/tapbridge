# Hybrid (caBLEv2) authenticator — design study

**Status: phase 1 (study + protocol skeleton). Not a finished feature.** This
document specifies how the tapbridge phone becomes a CTAP 2.2 hybrid
(caBLEv2) authenticator so Safari/Chrome WebAuthn on the Mac resolves by
tapping the FIDO card on the phone. The card remains the cryptographic root;
the phone mediates CTAP2 ↔ ISO-7816, byte-for-byte.

Scope per PLAN.md Track B: this phase delivers the protocol study, the pure
Rust protocol crate (`crates/hybrid`), and the Android integration plan.
BLE advertising, the Noise handshake, WebSocket transport and Android UI are
later phases.

---

## 1. Normative sources

| Subject | Citation |
|---|---|
| Hybrid transport: QR-initiated + state-assisted, BLE advert, tunnel, Noise, framing | FIDO CTAP 2.2, §11.5 Hybrid transports — <https://fidoalliance.org/specs/fido-v2.2-rd-20230321/fido-client-to-authenticator-protocol-v2.2-rd-20230321.html#hybrid> (review draft; §11.5 is normative for hybrid transport in the CTAP2.2 spec series) |
| CTAP2 commands (getInfo 0x04, getAssertion 0x02, getNextAssertion 0x08, makeCredential 0x01) | CTAP 2.2 §6 — <https://fidoalliance.org/specs/fido-v2.2-rd-20230321/fido-client-to-authenticator-protocol-v2.2-rd-20230321.html#authenticatorGetAssertion> |
| NFC/ISO-7816 transport binding: SELECT AID, NFCCTAP_MSG/GETRESPONSE/CONTROL, chaining | CTAP 2.2 §11.3 ISO7816, ISO14443 and NFC — <https://fidoalliance.org/specs/fido-v2.2-rd-20230321/fido-client-to-authenticator-protocol-v2.2-rd-20230321.html#nfc> |
| Canonical CBOR (CTAP2 message encoding) | CTAP 2.2 §6 message encoding — <https://fidoalliance.org/specs/fido-v2.2-rd-20230321/fido-client-to-authenticator-protocol-v2.2-rd-20230321.html#message-encoding> |
| `hybrid` transport enum, `hybridTransport` client capability, `hybrid` RP hint | W3C WebAuthn L3, §5.8.4 AuthenticatorTransport, §5.8.7 ClientCapability, §5.8.8 PublicKeyCredentialHint — <https://www.w3.org/TR/webauthn-3/#enum-transport> |
| HKDF-SHA256 | RFC 5869 — <https://www.rfc-editor.org/rfc/rfc5869> |
| Noise KNpsk0 / NKpsk0 | Noise Protocol Framework — <http://noiseprotocol.org/noise.html>; patterns: <https://noiseexplorer.com/patterns/KNpsk0/>, <https://noiseexplorer.com/patterns/NKpsk0/> |
| WebSockets | RFC 6455 — <https://www.rfc-editor.org/rfc/rfc6455> |

The CTAP 2.2 §11.5 text is written as Go pseudo-code with worked byte
layouts; our crate follows it exactly, and the unit tests pin outputs from an
**independent** Python+OpenSSL implementation (see §4.3).

### Open-source hybrid implementations worth referencing

- **Chromium** `device/fido/cable/` — the reference client-side
  implementation (Chrome's caBLE client): tunnel device, handshaker,
  discovery data. <https://source.chromium.org/chromium/chromium/src/+/main:device/fido/cable/>
  — the issue tracker for caBLE v2 is <https://issues.chromium.org/40098080>.
  The phone-side (authenticator role) is NOT implemented in Chromium; only
  the client role is.
- **Yubico python-fido2** <https://github.com/Yubico/python-fido2> — CTAP2
  and transports, no caBLEv2 hybrid support (their maintainers note hybrid
  transport is out of scope: <https://github.com/Yubico/libfido2/discussions/932>);
  useful as a second reading of NFC framing.
- **1Password** and **Dashlane** Android apps implement the authenticator
  side of caBLEv2 (closed source); their public write-ups confirm the
  tunnel-server domain list and FCM contact-ID use on Android
  (CTAP 2.2 §11.5.2: "For Android this is an FCM registration token").

## 2. Roles

```
   Mac (Chrome/Safari)                 Phone (tapbridge)              FIDO card
   ───────────────────                 ──────────────────              ─────────
   WebAuthn RP                         hybrid authenticator           crypto root
   caBLEv2 client platform             - BLE advertiser (§5.2)       CTAP2 applet
   - QR display (QR-initiated)         - tunnel client (§5.3)       SELECT AID
   - BLE scanner / trial-decrypt       - CTAP2↔APDU relay (§5.4)    A0000006472F0002
   - Noise initiator                   - tap gate (registered keys) NFCCTAP_MSG
   - TunnelServer client               (existing mechanism)
```

The phone never holds keys and never interprets CTAP2 semantics beyond the
framing layer; assertions are produced by the card. This mirrors the
tapbridge principle: no software credentials, the card does all crypto.

The **TunnelServer** (WebSocket relay, e.g. `cable.ua5v.com` /
`cable.auth.com`) is a high-availability service with a domain known to
authenticators (CTAP 2.2 §11.5). We do not operate one; we use the assigned
domains, matching what desktop Chrome/Safari already speak. The tunnel sees
only Noise-encrypted bytes.

## 3. The caBLEv2 message flow (QR-initiated)

Step-by-step, with the exact byte formats our crate implements. CTAP 2.2
§11.5.1.

### 3.1 The Mac shows a QR (`FIDO:/…`)

Contents: URI scheme `FIDO:` (uppercase; single foreslash — required for
some devices to treat it as a URI, and not a double slash because there is
no authority). The rest is the **digit encoding** of a canonical-CBOR map:

- Key 0: 33-byte P-256 X9.62 **compressed** public key — the Mac's
  authenticating identity for this exchange.
- Key 1: 16-byte random **QR secret** — parent of every derived key below.
- Key 2: count of assigned tunnel-server domains the Mac knows (u8).
- Key 3 (optional): epoch seconds.
- Key 4 (optional): true if the Mac can perform state-assisted transactions.
- Key 5 (optional): `"ga"`/`"mc"` hint (getAssertion/makeCredential follows).
- GREASE: the spec's example adds a random key 65535 with value 0 about 25%
  of the time to keep parsers strict.

**Digit encoding** (efficiency for QR): the CBOR bytes are split into
7-byte chunks, each read little-endian and rendered as a zero-padded
**17-digit** base-10 number; a trailing partial chunk of 1..6 bytes uses
3/5/8/10/13/15 digits respectively (the minimum any value of that many
bytes could need).

Implemented: `hybrid::qr::{QrContents::parse, digit_decode, digit_encode}`.

### 3.2 The phone advertises over BLE

The phone (authenticator) shows proximity with a **BLE advertisement** on
service data for UUID `0000fff9-0000-1000-8000-00805f9b34fb` with a
20-byte service-data payload:

```
plaintext (16 bytes):
  [0]      flags = 0 (versioning bits, must be zero today)
  [1..11]  10-byte connection nonce (proof of possession)
  [11..14] 3-byte routing ID (opaque, for the tunnel service)
  [14..16] u16 LE encoded tunnel-server domain (0..255 assigned, >=256 hashed)

service data (20 bytes) = AES-256-ECB(plaintext) || HMAC-SHA256(eid_key)[0..4]
```

The **EID key** is HKDF-SHA256(QR secret, salt=∅, purpose=1) → 64 bytes:
first 32 = AES-256 key, last 32 = HMAC-SHA256 key. The 4-byte truncated
HMAC over the ciphertext (not the plaintext!) is what lets the Mac
trial-decrypt every caBLE-shaped advert it sees without false positives.

Why ECB-of-one-block and not a stream mode: the spec's poor-man's
wide-block — there is no room for a nonce, and XORing a keystream would let
two authenticators that scanned the same QR interfere (mitm-style
correlation). (CTAP 2.2 §11.5.1 "trialDecrypt" commentary.)

Implemented: `hybrid::advert::{encode_service_data, decode_service_data}`
(both directions; the authenticator encodes, tests trial-decrypt like the
client).

### 3.3 The Mac trial-decrypts, derives keys, connects to the TunnelServer

The Mac scans BLE, trial-decrypts every `0000fff9-…` service-data payload
under the EID key, and on success obtains the advert plaintext. It then:

1. Derives the **tunnel ID** = HKDF(QR secret, purpose=2) → 16 bytes. Not
   derived from the BLE nonce — deliberately, so the tunnel service cannot
   brute-force the nonce from the tunnel ID (spec: "no need to trust it
   more than necessary").
2. Resolves the tunnel-server domain from the advert's u16:
   0..255 = index into the assigned table (`cable.ua5v.com`,
   `cable.auth.com`, …); >=256 or an index beyond the QR's domain count =
   **hashed fallback**: `cable.` + base32(SHA-256("caBLEv2 tunnel server
   domain" ‖ u16_le ‖ 0x00)[0..8] read LE, 5 bits at a time) + one of
   `.com/.org/.net/.info` (indexed by bits 0..1 of that u64).
3. Opens a WebSocket: `wss://<domain>/cable/connect/<routing-id-hex>/
   <tunnel-id-hex>` with **subprotocol `fido.cable`**; HTTP redirects must
   be followed.

Implemented: `hybrid::tunnel::{decode_tunnel_server_domain, TunnelConnect::qr_initiated}`
(URL construction; the WebSocket dial itself is Android-phase).

### 3.4 Noise KNpsk0 handshake on the tunnel

Both sides derive the **PSK** = HKDF(QR secret, salt=advert plaintext (16
bytes), purpose=3) → 32 bytes. The salt is the *decrypted advert plaintext*
— including it authenticates any future advert-format additions
automatically.

The phone also has to be on the tunnel: it scans the QR, derives the same
tunnel ID, and registers itself with its chosen tunnel service (routing ID
+ tunnel ID) over that service's private authenticator-side protocol (CTAP
2.2 §11.5: "the protocol between the authenticator and the tunnel service …
[is] a private detail of the authenticator's implementation"). The tunnel
service then matches the Mac's `/cable/connect/` dial to the phone.

Handshake over the tunnel: Noise **KNpsk0** (P-256, SHA-256, AES-256-GCM)
with a domain-separation byte mixed into the transcript hash before
anything else (`0x01` for KN, `0x00` for NK):

- **The Mac (initiator) speaks first** — this is the whole point: the
  first message proves possession of the BLE advert (i.e. proximity),
  because the PSK derivation needs the advert. Without it a malicious site
  could display a QR and try to drive the ceremony remotely; the BLE advert
  demand forces a radio physically near the victim (§11.5.1).
- The "K" in KNpsk0: the Mac's *static* public key is already known to the
  phone — it came in the QR (key 0). The Mac's first message is its fresh
  *ephemeral* ‖ `EncryptAndHash(∅)`; the phone replies its ephemeral ‖
  `EncryptAndHash(∅)`; the phone side additionally mixes
  `DH(ephemeral_mac_static…)` per the pattern (the responder DHs with the
  initiator's pre-known static); `Split()` yields the two directional
  AES-256-GCM traffic keys.
- On state-assisted transactions (§3.6) the same code runs **NKpsk0**
  instead: now it is the *phone's* long-term public key that was pre-shared
  (linking-data key 4), so the responder's key is the known one.

Not implemented in phase 1 (needs P-256 ECDH + AES-GCM): our crate pins
the PSK derivation and the message framing the handshake consumes. The
spec's `initialHandshakeMessage` / `processHandshakeResponse` pseudo-code
(§11.5.1) is a complete blueprint for the phase-3 implementation.

### 3.5 Post-handshake: getInfo up front, then CTAP2 over the tunnel

Immediately after the handshake the **phone sends the "post handshake"
message**: a CTAP2-canonical-CBOR map `{1: <authenticatorGetInfo response
bytes>}` (optional key 0 = zero-byte padding), saving the client a
round-trip. The getInfo response must report the authenticator's options
consistent with a card-backed relay (see §5.1 for what we advertise).

After that, every WebSocket **binary** frame (text frames forbidden) is
AES-256-GCM ciphertext (12-byte nonce = 4 zero bytes ‖ u32 BE per-direction
counter; empty AD; ≤2^24 messages per direction) over a padded plaintext:
the last plaintext byte is the padding count, granularity 32 recommended,
max 1 MiB. The plaintext is:

- byte 0 = message type: `0` shutdown (client→authenticator only), `1` CTAP,
  `2` update (CBOR map; authenticator→client carries linking data).
- byte 1.. = payload. For type 1 client→authenticator: the CTAP2 request
  (command byte ‖ CBOR); authenticator→client: CTAP2 response (status
  byte ‖ CBOR).

The client then sends `authenticatorGetAssertion` (0x02) — or
`makeCredential` (0x01) at registration — with the RP's parameters; the
phone relays each CTAP message to the card (§5.4), and relays the response
back. Multiple assertions (resident keys / username enumeration) use
`authenticatorGetNextAssertion` (0x08) with the same relay path.

Shutdown (type 0) is client→authenticator only; the authenticator may close
after it. The Mac's Chrome/Safari always sends it at ceremony end.

Implemented: `hybrid::framing::{TunnelMessage, unpad}` (encode/decode +
padding), constants for GCM layer consumption in phase 3.

### 3.6 State-assisted transactions (the second-and-later ceremonies)

Once a transaction has happened, the phone can send **linking data** in an
update message (type 2), so future ceremonies skip the QR:

- contact ID: on Android an **FCM registration token** — the tunnel service
  can wake the phone (that maps cleanly onto tapbridge's existing FCM path).
- link ID: 8 bytes identifying this link.
- link secret: 32 bytes — the parent key material for next time.
- authenticator public key: 65 bytes X9.62 uncompressed — long-term phone
  identity; lets desktops dedupe/sync links (Chrome Sync) and prevents one
  client impersonating the authenticator to another.
- name, and an HMAC-of-handshake-hash "signature" under
  ECDH(QR identity, phone key) — proving possession when replacing link
  data.

Then, later: the Mac opens `wss://<tunnel domain>/cable/contact/
<base64url(contact-id)>` with header `X-caBLE-Client-Payload` = hex of a
CBOR map `{1: link-id, 2: fresh 16-byte client nonce, 3: "ga"/"mc"}`. The
tunnel service pushes a wake to the phone (FCM). The phone derives the EID
key as HKDF(link secret, salt=client nonce, purpose=1), advertises, and
the handshake runs as **NKpsk0** (phone key pre-shared, client initiates
with the advert-derived PSK). A 410 from the contact URL means the user
unlinked — the Mac forgets the link data.

Implemented (pure parts): `hybrid::keys::EidKey::from_link_secret`,
`hybrid::tunnel::TunnelConnect::state_assisted_contact` + client-payload
shape (constants in tests); the phone-side contact push is Android-phase.

## 4. What phase 1 ships: `crates/hybrid`

A workspace member crate, pure protocol logic, no Android, no async, no
sockets. Modules and their spec anchors:

| Module | Contents | Anchor |
|---|---|---|
| `hkdf` | RFC 5869 HKDF-SHA256 with the caBLE purpose-info convention (u32 LE purpose) | CTAP 2.2 §11.5.1 key derivation |
| `advert` | 20-byte service data encode/trial-decrypt; 16-byte plaintext layout; flags/nonce/routing-id/domain split | §11.5.1 `awaitAdvert`/`trialDecrypt`/`unpackDecryptedAdvert` |
| `keys` | EID key (QR + state-assisted), tunnel ID, PSK derivations | §11.5.1 `keyPurpose*` |
| `qr` | `FIDO:/` parse: digit decode + strict canonical-CBOR map reader | §11.5.1 `encodeQRContents`/`digitEncode` |
| `tunnel` | domain decoding (assigned + hashed fallback), connect/contact URL construction, base64url | §11.5.1 `decodeTunnelServerDomain`, §11.5.2 |
| `framing` | tunnel message type byte, padding (32-byte granularity), 1 MiB / 2^24 bounds | §11.5.1 `cableConn`, message types |
| `ctap` | CTAP2↔ISO-7816 relay codec: SELECT AID APDU, NFCCTAP_MSG (INS 0x10) with P1 getresponse bit, NFCCTAP_GETRESPONSE (0x11), NFCCTAP_CONTROL end (0x12), extended-length + short-chaining, CardResponse parse (9000/9100/ISO errors) | §11.3.3–§11.3.7 |

Deliberately NOT in phase 1: the Noise handshake itself, the WebSocket
client, the BLE advertiser, Android service code, CTAP2 CBOR command
parsing (the relay stays byte-for-byte; we frame, we don't interpret).

### 4.1 AID note

The phone's existing registered-keys identification SELECTs
`A0000006472F0001` (tapbridge-specific applet; DESIGN.md). The hybrid relay
uses the **FIDO2 NFC AID `A0000006472F0002`** (RID `A000000647`, PIX
`2F0002`) — CTAP 2.2 §11.3.3. These are distinct applets on the card: 0001
answers identification queries, 0002 speaks CTAP2. Both constants live in
`hybrid::ctap` (`FIDO2_NFC_AID`) and lib.rs (`TAPBRIDGE_ID_AID`).

### 4.2 House style conformance

Following `crates/core/src/wire.rs`:

- **Strict decode**: unknown versions/types, wrong lengths, trailing
  bytes, RFU bits set, non-canonical CBOR — all rejected before use, with
  typed errors (`QrError`, `AdvertDecryptError`, `FrameError`,
  `CtapError`, `TunnelServerDomainError`).
- **Bounded lengths**: advert exactly 20 bytes; QR digit chunking validated
  against the 3/5/8/10/13/15 partial table; padding count ≤ message length;
  1 MiB / 2^24 message/nonce caps; NFCCTAP_MSG ≤ 65535 data bytes;
  short-chaining fragments ≤ 255.
- **Debug redaction**: `EidKey`, `QrContents` (secret), `AdvertPlaintext`
  (nonce + routing ID), `TunnelMessage` (CTAP/update payloads — can carry
  link secrets and PIN material), `CardResponse` (user-identifying CTAP
  data) all render `REDACTED`.
- **Property/unit tests**: round-trips for every codec; strict-rejection
  tests for every invalid shape; independent test vectors (next section).

### 4.3 Test-vector methodology (no self-confirmation bias)

The crypto paths are pinned against an **independent implementation**:
Python's `hashlib`/`hmac` for HKDF/HMAC (written from RFC 5869 directly)
plus the `openssl` CLI (`enc -aes-256-ecb -nopad`) for the advert AES
block, and a round-trip proof of the digit encoding. The generator lives in
the repo at `crates/hybrid/vectors/generate.py` (needs `openssl` on PATH);
its output is embedded as hex constants in the crate's unit tests. Two bugs
were caught this way during development: the state-assisted vector
originally derived from a 16-byte parent (spec link secret is 32 bytes),
and an early `digit_encode` used `from_le_bytes` on a 7-byte slice. This
is why the vectors exist.

## 5. Android integration plan (phases 3–4)

Phase 1 leaves hooks at exactly the boundaries Android needs. Nothing here
is implemented yet; this is the plan-of-record.

### 5.1 What the phone advertises in getInfo (post-handshake message)

`authenticatorGetInfo` response fields the card relay must surface (CTAP
2.2 §6.1.2):

- versions: from the card's own getInfo — relayed, not synthesized. (The
  phone caches it after the first SELECT+NFCCTAP_MSG(getInfo) so the
  post-handshake message is instant.)
- aaguid: the card's AAGUID (the registered-keys flow already extracts it
  via SELECT `…0001`; SELECT `…0002` + NFCCTAP_MSG get_info gives the
  authoritative one).
- options: relay the card's; the phone itself only adds `"hybrid
  transport"` availability by virtue of advertising at all. `rk`
  (resident keys), `uv`, `clientPin` etc. are the card's business.
- transports/attachments: the CTAP2.2 hybrid authenticator declares
  `transports: ["hybrid"]`-equivalent by construction.
- maxMsgSize: min(card extended APDU capacity, tunnel 1 MiB).

The phone MUST NOT invent capabilities the card lacks (e.g. it must not
claim `uv` if the card does no user verification beyond the tap).

### 5.2 BLE advertiser

- `BluetoothLeAdvertiser.startAdvertising` with a
  `AdvertiseData` containing service data for UUID
  `0000fff9-0000-1000-8000-00805f9b34fb`: our 20-byte payload from
  `advert::encode_service_data`.
- Permissions: `BLUETOOTH_ADVERTISE` (S+ runtime permission) +
  `BLUETOOTH_CONNECT`; manifest normal-perm fallback for older targets.
- Advertise while: a tunnel invite is being awaited (QR scan done or FCM
  contact push received) → until the Mac's first handshake message arrives
  or the ceremony times out (cap: 120 s, matching the existing tap-gate
  window). Advertise at `ADVERTISE_MODE_LOW_LATENCY` for discovery speed;
  the payload is fixed per ceremony (nonce is minted once — the EID key
  is static for the whole ceremony, so the advert bytes are constant).
- Battery: advertising for ≤120 s at low latency during an explicit user
  ceremony is acceptable; it stops the moment the handshake completes.
- Android 14+ requires the `foregroundServiceType="connectedDevice"` on the
  service that advertises — we already have a `connectedDevice` FGS type
  for the NFC session service; extend it or add a sibling.

### 5.3 Tunnel client (WebSocket)

- Library choice: OkHttp WebSocket client (already a natural fit with the
  existing Kotlin stack; Java-WebSocket is the alternative). Dial the URL
  from `tunnel::TunnelConnect`, subprotocol `fido.cable`, follow redirects.
- The Noise KNpsk0/NKpsk0 handshake + AES-256-GCM record layer: implement
  in Rust inside `crates/phone` JNI (reuse RustCrypto `p256`, `aes-gcm`,
  `noise`-style state machine hand-rolled to the spec's mixHash/mixKey
  calls — the spec's pseudo-code is a complete blueprint). Kotlin owns
  sockets/lifecycle; Rust owns bytes. Keep it byte-driven: Kotlin hands
  binary frames to JNI, gets binary frames back.
- Contact push (state-assisted): the TunnelServer contact flow reaches the
  phone via the **same FCM channel** tapbridge already uses (our FCM token
  is the caBLE contact ID, §3.6). The wake lands in the existing
  `WakeService`-style path; new message kind `hybrid-invite` routes to the
  hybrid ceremony service instead of the NFC tap gate.
- Domain pinning: the tunnel domain comes from the invite/QR; treat
  `cable.*` domains as the only allowed set (don't give an attacker's
  domain a WebSocket + BLE pairing surface).

### 5.4 Tap gate integration + ceremony lifecycle

```
FCM/QR invite → HybridService (FGS, connectedDevice)
  → SELECT FIDO2 AID 0002 on the next tap (extend TapGate's TECH_DISCOVERED
    window to accept the invite as a trigger; card match logic unchanged:
    registered UID/AAGUID)
  → BLE advertise (5.2) + WebSocket connect (5.3) in parallel
  → Noise handshake; post-handshake getInfo from card cache
  → for each tunnel CTAP message: ctap::msg_apdu → transceive(IsoDep) →
    CardResponse → tunnel CTAP message (loop on 9100 via getresponse_apdu
    until 9000 or error; honour P1 getresponse bit)
  → shutdown(0) from Mac: NFCCTAP_CONTROL end + close IsoDep + stop
    advertising; retire generation (existing wire-v2 semantics for the
    iroh session do not apply here — this is a parallel channel, the iroh
    reader path stays untouched)
```

Rules carried over from the existing design: one ceremony at a time; the
tap gate must not open for a hybrid invite unless the card is a registered
key; nothing user-identifying in logs; NFCCTAP_CONTROL ends the applet
transaction on cancel/shutdown/timeout; on tag loss mid-ceremony, fail the
pending CTAP request with a transport error (CTAP2 `CTAP2_ERR_...` mapping
is the card's business — we surface an ISO transport failure and let the
client see a failed ceremony, never a fabricated success).

### 5.5 Pairing with the Mac (security, per PLAN.md item 4)

The caBLE QR flow itself is an authenticated pairing (the QR secret is a
one-shot shared secret and the Mac's identity key is in the QR). But the
plan's requirement is stricter: **hybrid pairing must not bypass the
registered-keys tap gate, and invites only for known/paired Macs**.

Phase-1 decision (to carry into phase 4):

1. **QR-initiated (first ceremony with this Mac)**: allowed only if the
   user explicitly enters "Add hybrid Mac" mode in the app — i.e. scanning
   the Mac's `FIDO:/` QR is an explicit user action, like the existing
   tapbridge pairing QR scan. This gives user-consent parity with the
   tapbridge QR flow.
2. **State-assisted (later ceremonies)**: the link data is stored only
   after a completed first ceremony; the contact wake is accepted only
   from the tunnel domain set and only when a stored link matches (link ID
   in the client payload). The existing FCM wake routing ("unknown host →
   drop") extends naturally: unknown/absent link ID → drop.
3. The tap gate (registered keys) still gates every NFC session, hybrid or
   iroh-relay: an unregistered card never gets a tunnel.
4. Revocation parity: "Forget this Mac" in the UI clears the link secret,
   link ID and cached contact ID; the next `/cable/contact/` then yields
   the 410 path for the Mac (tunnel believes the authenticator
   uncontactable), matching how "forget host" works for the iroh pairing.

The tapbridge-iroh pairing QR (`tapbridge:1:…`) stays separate: it pairs
the *reader* relationship. The caBLE QR pairs the *hybrid ceremony*
relationship. Users can have either without the other, though in practice
both start from "pair with this Mac" flows.

### 5.6 Desktop side

Nothing (PLAN.md item 5): Safari/Chrome implement the desktop client role
natively, including the QR, BLE scan, trial-decrypt, tunnel dial, Noise
initiator and CTAP2 request generation. The tapbridge Mac app (Track A)
may later surface "hybrid ready" state in its tray; not in scope.

## 6. Verification gates for this phase (honest report)

Run on this machine (macOS, worktree `tapbridge-b`):

| Gate | Result |
|---|---|
| `cargo test -p tapbridge-hybrid` | **44 passed, 0 failed** (unit + strict-decode + independent vectors) |
| `cargo test --workspace --exclude tapbridge-desktop` | **66 passed, 0 failed** (core/host/phone/hybrid) — desktop excluded, see below |
| `cargo test --workspace` (with desktop) | FAILS at `gpui v0.2.1` build script: `cannot execute tool 'metal' due to missing Metal Toolchain; use: xcodebuild -downloadComponent Metal Toolchain` — a pre-existing macOS environment gap (no Xcode Metal toolchain on this build machine), not caused by this branch; the desktop crate compiles on the Windows CI path per VERIFICATION.md |
| `cargo clippy -p tapbridge-hybrid --all-targets` | clean (0 warnings) after fixes |
| `cargo fmt -p tapbridge-hybrid --check` | clean |
| Windows host / existing crates | compile unchanged — hybrid adds only pure-Rust deps (aes, hmac, sha2), no build scripts, no platform-specific code; Windows CI (`scripts/check.ps1`) NOT RUN on this machine (no Windows host here), reported honestly per VERIFICATION.md convention |

Independent-vector coverage: EID key (QR + state-assisted paths), tunnel
ID, PSK, advert service data (AES + HMAC tag), tunnel-domain hashing
(assigned + fallback), digit encoding/decoding round-trip, QR CBOR map
parse — all against a Python+OpenSSL implementation, not self-generated.

## 7. Out-of-phase reminders (phase 2+ backlog)

- Noise handshake state machine (KNpsk0 + NKpsk0, domain-sep byte 0/1) —
  needs p256 + aes-gcm deps; blueprint is the spec pseudo-code.
- WebSocket client + `X-caBLE-Client-Payload` header handling.
- BLE advertiser + Android permissions ceremony.
- getInfo caching strategy and maxMsgSize reconciliation.
- 9100-status-update → user-visible progress (NFCCTAP_GETRESPONSE loop).
- Cancel paths: Mac shutdown, user cancel, tag loss, 120 s timeout.
- Linking-data persistence (Android Keystore-wrapped, like the phone
  identity) and "Forget this Mac".
- Property tests with `proptest` for digit codec fuzzing (the manual
  round-trip tests cover lengths 1..40 today).
- chrome://flags-driven end-to-end test against the hybrid client
  simulator (PLAN.md verification item).
