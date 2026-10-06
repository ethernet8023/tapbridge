//! Pure-protocol parts of the CTAP 2.2 hybrid (caBLEv2) authenticator role.
//!
//! This crate implements the phone-side protocol pieces that can be built and
//! unit-tested without Android: the BLE advertisement encoding/decoding
//! ([`advert`]), the key derivations from the QR secret / link secret
//! ([`keys`]), tunnel-server domain decoding and WebSocket URL construction
//! ([`tunnel`]), the tunnel message framing types ([`framing`]), and CTAP2
//! over ISO-7816 NFC framing as a card-APDU relay codec ([`ctap`]).
//!
//! The Noise KNpsk0/NKpsk0 tunnel handshake and the WebSocket transport are
//! deliberately NOT implemented here: they need P-256 ECDH, AES-256-GCM and
//! an async socket stack, all of which live on the Android side (phase 3/4).
//! What IS implemented is the exact byte-level framing those phases consume,
//! so the handshake code can be dropped in against stable types.
//!
//! Spec: FIDO CTAP 2.2 §11.5 Hybrid transports
//! <https://fidoalliance.org/specs/fido-v2.2-rd-20230321/fido-client-to-authenticator-protocol-v2.2-rd-20230321.html#ctap2-usb> and
//! §11.3 NFC <https://fidoalliance.org/specs/fido-v2.2-rd-20230321/fido-client-to-authenticator-protocol-v2.2-rd-20230321.html#nfc>
//! (see docs/HYBRID.md for full citations).

pub mod advert;
pub mod ctap;
pub mod framing;
pub mod keys;
pub mod qr;
pub mod tunnel;

mod hkdf;

pub use advert::{AdvertDecryptError, AdvertPlaintext, SERVICE_DATA_LEN, SERVICE_UUID};
pub use framing::{FrameError, TunnelMessage};
pub use qr::{QrContents, QrError};
pub use tunnel::{TunnelServerDomainError, decode_tunnel_server_domain};

/// HKDF purposes from CTAP 2.2 §11.5.1 key derivation.
pub mod constants {
    /// Derive the 64-byte EID key from the QR/link secret.
    pub const PURPOSE_EID_KEY: u32 = 1;
    /// Derive the 16-byte tunnel ID from the QR secret.
    pub const PURPOSE_TUNNEL_ID: u32 = 2;
    /// Derive the 32-byte Noise PSK from the secret + advert plaintext.
    pub const PURPOSE_PSK: u32 = 3;
}
