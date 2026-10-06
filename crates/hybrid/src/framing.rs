//! Tunnel message framing: the post-handshake record format on the caBLEv2
//! WebSocket.
//!
//! After the Noise KNpsk0/NKpsk0 handshake, every WebSocket binary frame
//! carries AES-256-GCM ciphertext (per-direction big-endian counter nonce in
//! 12 bytes, empty additional data) over a padded plaintext. The first
//! plaintext byte is the message type (CTAP 2.2 §11.5.1):
//!
//! - 0: shutdown (client -> authenticator only)
//! - 1: CTAP message (CTAP2 request/response payload follows)
//! - 2: update (CBOR map; authenticator -> client carries linking info)
//!
//! Padding: the final byte of the plaintext is the number of preceding bytes
//! that are padding; granularity up to 256, 32 recommended; messages cap at
//! 1 MiB.
//!
//! This module implements the plaintext layer (type byte + payload, with
//! padding) as pure encode/decode over bytes, so the GCM layer in phase 3
//! wraps these. The Noise handshake itself is out of scope here (needs P-256
//! ECDH + AES-GCM + async transport; see docs/HYBRID.md Android plan).

/// Message type byte: shutdown.
pub const TYPE_SHUTDOWN: u8 = 0;
/// Message type byte: CTAP message.
pub const TYPE_CTAP: u8 = 1;
/// Message type byte: update (linking information).
pub const TYPE_UPDATE: u8 = 2;

/// Recommended padding granularity in bytes.
pub const PADDING_GRANULARITY: usize = 32;
/// Maximum plaintext size (spec: 1 MiB).
pub const MAX_PLAINTEXT: usize = 1 << 20;
/// Messages per direction before nonce exhaustion (2^24).
pub const MAX_MESSAGES: u32 = 1 << 24;

/// A decoded tunnel message (the plaintext after AES-GCM decryption).
#[derive(Clone, PartialEq, Eq)]
pub enum TunnelMessage {
    /// Type 0. Client -> authenticator only. No payload.
    Shutdown,
    /// Type 1. CTAP2 request (client -> authenticator) or response
    /// (authenticator -> client). Payload is the CTAP2 command bytes
    /// (command byte || CBOR).
    Ctap(Vec<u8>),
    /// Type 2. CBOR-encoded update map. Authenticator -> client may carry
    /// linking information (keys 0/1); client -> authenticator is unused
    /// today.
    Update(Vec<u8>),
}

impl TunnelMessage {
    /// Decode a decrypted, unpadded tunnel message. Strict: empty input,
    /// unknown type byte, or oversized payload is an error.
    pub fn decode(plaintext: &[u8]) -> Result<Self, FrameError> {
        let Some((&ty, payload)) = plaintext.split_first() else {
            return Err(FrameError::Empty);
        };
        if payload.len() > MAX_PLAINTEXT {
            return Err(FrameError::TooLarge);
        }
        match ty {
            TYPE_SHUTDOWN if payload.is_empty() => Ok(Self::Shutdown),
            TYPE_CTAP if !payload.is_empty() => Ok(Self::Ctap(payload.to_vec())),
            TYPE_UPDATE if !payload.is_empty() => Ok(Self::Update(payload.to_vec())),
            TYPE_SHUTDOWN | TYPE_CTAP | TYPE_UPDATE => Err(FrameError::BadLength),
            _ => Err(FrameError::UnknownType(ty)),
        }
    }

    /// Encode without padding (message type byte + payload).
    pub fn encode_unpadded(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.payload().len());
        out.push(self.type_byte());
        out.extend_from_slice(self.payload());
        out
    }

    /// Encode with padding to the recommended granularity: append
    /// `extra` bytes where the final byte holds `extra - 1` (i.e. the count
    /// of padding bytes preceding the count byte is `value`, total appended
    /// = value + 1... matching the spec's `extraBytes`/`extraBytes-1` shape).
    pub fn encode_padded(&self) -> Vec<u8> {
        let msg = self.encode_unpadded();
        let extra = PADDING_GRANULARITY - (msg.len() % PADDING_GRANULARITY);
        let mut out = msg;
        out.extend(std::iter::repeat_n(0, extra - 1));
        out.push((extra - 1) as u8);
        out
    }

    /// Type byte for this message.
    pub fn type_byte(&self) -> u8 {
        match self {
            Self::Shutdown => TYPE_SHUTDOWN,
            Self::Ctap(_) => TYPE_CTAP,
            Self::Update(_) => TYPE_UPDATE,
        }
    }

    /// Payload after the type byte.
    pub fn payload(&self) -> &[u8] {
        match self {
            Self::Shutdown => &[],
            Self::Ctap(d) | Self::Update(d) => d,
        }
    }
}

impl std::fmt::Debug for TunnelMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // CTAP payloads and update maps can contain secrets (link secret in
        // linking data, PIN hashes in CTAP); redact payloads entirely.
        match self {
            Self::Shutdown => f.write_str("TunnelMessage::Shutdown"),
            Self::Ctap(_) => f.write_str("TunnelMessage::Ctap(REDACTED)"),
            Self::Update(_) => f.write_str("TunnelMessage::Update(REDACTED)"),
        }
    }
}

/// Errors from tunnel message decoding / padding handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Zero-length plaintext.
    Empty,
    /// Unknown message type byte.
    UnknownType(u8),
    /// Payload length inconsistent with the type (e.g. shutdown with a
    /// payload, CTAP with an empty payload).
    BadLength,
    /// Padding byte claims more padding than the message holds.
    BadPadding,
    /// Plaintext exceeds 1 MiB.
    TooLarge,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("empty tunnel message")?,
            Self::UnknownType(t) => write!(f, "unknown tunnel message type {t:#x}")?,
            Self::BadLength => f.write_str("tunnel message length invalid for its type")?,
            Self::BadPadding => f.write_str("invalid tunnel message padding")?,
            Self::TooLarge => f.write_str("tunnel message exceeds 1 MiB")?,
        }
        Ok(())
    }
}

/// Strip the padding from a decrypted padded plaintext (inverse of
/// `encode_padded`): the last byte is the number of padding bytes before it.
pub fn unpad(padded: &[u8]) -> Result<&[u8], FrameError> {
    let Some((&last, _)) = padded.split_last() else {
        return Err(FrameError::Empty);
    };
    let padding = last as usize;
    if padding + 1 > padded.len() {
        return Err(FrameError::BadPadding);
    }
    let end = padded.len() - 1 - padding;
    if end > MAX_PLAINTEXT {
        return Err(FrameError::TooLarge);
    }
    Ok(&padded[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_message_roundtrip() {
        for msg in [
            TunnelMessage::Shutdown,
            TunnelMessage::Ctap(vec![0x04]), // authenticatorGetInfo
            TunnelMessage::Ctap(vec![0x02, 0xA1, 0x01, 0x02]), // getAssertion + CBOR
            TunnelMessage::Update(vec![0xA1, 0x01, 0xA3]),
        ] {
            let encoded = msg.encode_unpadded();
            assert_eq!(TunnelMessage::decode(&encoded).unwrap(), msg);
        }
    }

    #[test]
    fn tunnel_message_type_bytes() {
        assert_eq!(TunnelMessage::Shutdown.type_byte(), 0);
        assert_eq!(TunnelMessage::Ctap(vec![1]).type_byte(), 1);
        assert_eq!(TunnelMessage::Update(vec![1]).type_byte(), 2);
    }

    #[test]
    fn tunnel_message_strict_decode() {
        // empty
        assert_eq!(TunnelMessage::decode(&[]).unwrap_err(), FrameError::Empty);
        // unknown type
        assert_eq!(
            TunnelMessage::decode(&[3, 0]).unwrap_err(),
            FrameError::UnknownType(3)
        );
        assert_eq!(
            TunnelMessage::decode(&[0xFF, 0]).unwrap_err(),
            FrameError::UnknownType(0xFF)
        );
        // shutdown with payload
        assert_eq!(
            TunnelMessage::decode(&[0, 1]).unwrap_err(),
            FrameError::BadLength
        );
        // CTAP with empty payload
        assert_eq!(
            TunnelMessage::decode(&[1]).unwrap_err(),
            FrameError::BadLength
        );
        // update with empty payload
        assert_eq!(
            TunnelMessage::decode(&[2]).unwrap_err(),
            FrameError::BadLength
        );
    }

    #[test]
    fn tunnel_message_padding_roundtrip() {
        let msg = TunnelMessage::Ctap(vec![0x04]);
        let padded = msg.encode_padded();
        // padded to a multiple of 32
        assert_eq!(padded.len() % PADDING_GRANULARITY, 0);
        assert!(!padded.is_empty());
        let unpadded = unpad(&padded).unwrap();
        assert_eq!(TunnelMessage::decode(unpadded).unwrap(), msg);

        // message that already is a multiple of 32 still gets 32 more bytes
        // (extra = 32, count byte = 31): max padding never zero-length.
        let exact = TunnelMessage::Ctap(vec![0xAB; 31]);
        let padded = exact.encode_padded();
        assert_eq!(padded.len(), 64);
        assert_eq!(unpad(&padded).unwrap(), &exact.encode_unpadded()[..]);
    }

    #[test]
    fn tunnel_unpad_strict() {
        assert_eq!(unpad(&[]).unwrap_err(), FrameError::Empty);
        // padding byte claims more than available
        assert_eq!(unpad(&[0x00, 0x05]).unwrap_err(), FrameError::BadPadding);
        // single byte that is a valid "no padding" marker
        assert_eq!(unpad(&[0x42, 0x00]).unwrap(), &[0x42]);
        // single byte = padding 0 for an empty message is still invalid at
        // the TunnelMessage layer (empty), but unpad itself allows it
        assert_eq!(unpad(&[0x00]).unwrap(), &[] as &[u8]);
    }

    #[test]
    fn tunnel_message_debug_redacts() {
        let msg = TunnelMessage::Ctap(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let s = format!("{msg:?}");
        assert_eq!(s, "TunnelMessage::Ctap(REDACTED)");
        assert!(!s.contains("deadbeef"));
        let upd = TunnelMessage::Update(vec![0x99]);
        assert_eq!(format!("{upd:?}"), "TunnelMessage::Update(REDACTED)");
    }

    #[test]
    fn tunnel_shutdown_decodes_bare_type_byte() {
        // Shutdown is exactly one byte, no payload. (Direction — client to
        // authenticator only — is a session-layer rule, not a framing one.)
        assert_eq!(
            TunnelMessage::decode(&[0]).unwrap(),
            TunnelMessage::Shutdown
        );
    }
}
