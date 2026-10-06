//! caBLEv2 BLE advertisement service data: 20 bytes = AES-256(plaintext16)
//! || HMAC-SHA256(eid_key.hmac)[0..4], where eid_key is 64 bytes derived from
//! the QR secret (QR-initiated) or the link secret + client nonce
//! (state-assisted).
//!
//! The 16-byte plaintext is: flags byte (MUST be 0) || 10-byte connection
//! nonce || 3-byte routing ID || 2-byte little-endian tunnel-server domain
//! identifier (CTAP 2.2 §11.5.1 `unpackDecryptedAdvert`).
//!
//! The authenticator ENCODES the advert (encrypt + tag); the client TRIAL
//! DECRYPTS it. This module provides both directions, because the phone is
//! the authenticator and tests need the client direction to verify.

use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::keys::EidKey;

/// BLE service UUID whose service data carries the caBLEv2 advert.
pub const SERVICE_UUID: &str = "0000fff9-0000-1000-8000-00805f9b34fb";
/// Length of the advert service data payload.
pub const SERVICE_DATA_LEN: usize = 20;
/// Length of the advert plaintext before encryption.
pub const PLAINTEXT_LEN: usize = 16;
/// HMAC tag length appended to the ciphertext.
pub const TAG_LEN: usize = 4;

/// The decrypted, authenticated advert contents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AdvertPlaintext {
    /// Flags byte. Currently always zero; used for versioning later.
    pub flags: u8,
    /// 10-byte connection nonce: proof of possession of the BLE advert.
    pub nonce: [u8; 10],
    /// 3-byte opaque routing ID for the tunnel service.
    pub routing_id: [u8; 3],
    /// 16-bit little-endian encoded tunnel-server domain identifier.
    pub tunnel_server_domain: u16,
}

impl AdvertPlaintext {
    /// Create a valid (flags = 0) advert plaintext.
    pub fn new(nonce: [u8; 10], routing_id: [u8; 3], tunnel_server_domain: u16) -> Self {
        Self {
            flags: 0,
            nonce,
            routing_id,
            tunnel_server_domain,
        }
    }

    fn to_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0] = self.flags;
        out[1..11].copy_from_slice(&self.nonce);
        out[11..14].copy_from_slice(&self.routing_id);
        out[14] = self.tunnel_server_domain as u8;
        out[15] = (self.tunnel_server_domain >> 8) as u8;
        out
    }

    fn from_bytes(bytes: [u8; 16]) -> Result<Self, AdvertDecryptError> {
        if bytes[0] != 0 {
            return Err(AdvertDecryptError::ReservedBits);
        }
        let mut nonce = [0u8; 10];
        nonce.copy_from_slice(&bytes[1..11]);
        let mut routing_id = [0u8; 3];
        routing_id.copy_from_slice(&bytes[11..14]);
        Ok(Self {
            flags: 0,
            nonce,
            routing_id,
            tunnel_server_domain: u16::from_le_bytes([bytes[14], bytes[15]]),
        })
    }
}

impl std::fmt::Debug for AdvertPlaintext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The nonce is a proximity secret; routing ID is tunnel-identifying.
        // Redact both to keep debug output safe in logs.
        f.debug_struct("AdvertPlaintext")
            .field("flags", &self.flags)
            .field("nonce", &"REDACTED")
            .field("routing_id", &"REDACTED")
            .field("tunnel_server_domain", &self.tunnel_server_domain)
            .finish()
    }
}

/// Why a candidate BLE advert failed to authenticate/decrypt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvertDecryptError {
    /// The service data payload was not exactly 20 bytes.
    BadLength,
    /// The 4-byte HMAC tag did not verify (wrong key or not a caBLE advert).
    BadTag,
    /// The plaintext flags byte was non-zero.
    ReservedBits,
}

impl std::fmt::Display for AdvertDecryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::BadLength => "advert service data must be 20 bytes",
            Self::BadTag => "advert HMAC tag mismatch",
            Self::ReservedBits => "advert reserved bits not zero",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for AdvertDecryptError {}

/// Encode an advert for BLE service data: encrypt the 16-byte plaintext with
/// the AES half of the EID key, then append a 4-byte HMAC-SHA256 tag over
/// the ciphertext using the HMAC half.
pub fn encode_service_data(eid_key: &EidKey, plaintext: AdvertPlaintext) -> [u8; SERVICE_DATA_LEN] {
    let block: aes::Block = plaintext.to_bytes().into();
    let mut out = [0u8; SERVICE_DATA_LEN];
    let cipher = Aes256::new_from_slice(eid_key.aes()).expect("32-byte AES key");
    let mut b = block;
    cipher.encrypt_block(&mut b);
    out[..16].copy_from_slice(&b);
    let tag = hmac_tag(eid_key, &out[..16]);
    out[16..].copy_from_slice(&tag);
    out
}

/// Trial-decrypt a candidate 20-byte service data payload under `eid_key`.
/// This is the client-side direction (`trialDecrypt` in the spec); the phone
/// uses it in tests and in the hybrid-client simulator.
pub fn decode_service_data(
    eid_key: &EidKey,
    service_data: &[u8],
) -> Result<AdvertPlaintext, AdvertDecryptError> {
    if service_data.len() != SERVICE_DATA_LEN {
        return Err(AdvertDecryptError::BadLength);
    }
    let (ciphertext, tag) = service_data.split_at(PLAINTEXT_LEN);
    if hmac_tag(eid_key, ciphertext) != *tag {
        return Err(AdvertDecryptError::BadTag);
    }
    let mut raw = [0u8; 16];
    raw.copy_from_slice(ciphertext);
    let mut block: aes::Block = raw.into();
    let cipher = Aes256::new_from_slice(eid_key.aes()).expect("32-byte AES key");
    cipher.decrypt_block(&mut block);
    AdvertPlaintext::from_bytes(block.into())
}

fn hmac_tag(eid_key: &EidKey, ciphertext: &[u8]) -> [u8; TAG_LEN] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(eid_key.hmac()).expect("32-byte HMAC key");
    mac.update(ciphertext);
    let out = mac.finalize().into_bytes();
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(&out[..TAG_LEN]);
    tag
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independent vector generated with python + openssl (see docs/HYBRID.md).
    // QR secret 000102...0f, purpose EID_KEY(1), no salt.
    const EID_KEY_HEX: &str = "d06e92859fa9afce38139c04dae7f64196acfdb9a28dffb326f3a8f2cc0788e228bd73c3484f2f7de169d9b087c30e6f409e70bc86f5d65e03d762b4b0df2af7";
    const PLAIN_HEX: &str = "000102030405060708090aaabbcc0001";
    const SERVICE_DATA_HEX: &str = "31abb0016b24746895a7309e0cc26a7aa1c27392";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn key() -> EidKey {
        EidKey::from_slice(&unhex(EID_KEY_HEX)).unwrap()
    }

    #[test]
    fn advert_roundtrip_independent_vector() {
        let key = key();
        let plain_bytes: [u8; 16] = unhex(PLAIN_HEX).try_into().unwrap();
        let plain = AdvertPlaintext::from_bytes(plain_bytes).unwrap();
        assert_eq!(plain.tunnel_server_domain, 0x0100);
        let sd = encode_service_data(&key, plain);
        assert_eq!(hex(&sd), SERVICE_DATA_HEX);
        let back = decode_service_data(&key, &sd).unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn advert_rejects_wrong_length() {
        let key = key();
        assert_eq!(
            decode_service_data(&key, &[0u8; 19]).unwrap_err(),
            AdvertDecryptError::BadLength
        );
        assert_eq!(
            decode_service_data(&key, &[0u8; 21]).unwrap_err(),
            AdvertDecryptError::BadLength
        );
    }

    #[test]
    fn advert_rejects_bad_tag() {
        let key = key();
        let mut sd = encode_service_data(&key, AdvertPlaintext::new([1; 10], [2; 3], 0));
        sd[19] ^= 1;
        assert_eq!(
            decode_service_data(&key, &sd).unwrap_err(),
            AdvertDecryptError::BadTag
        );
        // wrong key entirely
        let other = EidKey::from_slice(&[7u8; 64]).unwrap();
        assert_eq!(
            decode_service_data(&other, &sd).unwrap_err(),
            AdvertDecryptError::BadTag
        );
    }

    #[test]
    fn advert_rejects_nonzero_flags() {
        // Encrypt a plaintext with flags != 0 under the key; tag will verify
        // but the reserved-bits check must reject it.
        let key = key();
        let plain = AdvertPlaintext::new([0u8; 10], [0u8; 3], 0);
        let mut raw = plain.to_bytes();
        raw[0] = 1;
        let mut sd = [0u8; SERVICE_DATA_LEN];
        let mut block: aes::Block = raw.into();
        let cipher = Aes256::new_from_slice(key.aes()).unwrap();
        cipher.encrypt_block(&mut block);
        sd[..16].copy_from_slice(&block);
        let tag = hmac_tag(&key, &sd[..16]);
        sd[16..].copy_from_slice(&tag);
        assert_eq!(
            decode_service_data(&key, &sd).unwrap_err(),
            AdvertDecryptError::ReservedBits
        );
    }

    #[test]
    fn advert_debug_redacts_nonce_and_routing_id() {
        let plain = AdvertPlaintext::new([9u8; 10], [8u8; 3], 0x0100);
        let dbg = format!("{plain:?}");
        assert!(dbg.contains("REDACTED"));
        assert!(!dbg.contains("0909"));
        assert!(!dbg.contains("080808"));
    }

    #[test]
    fn advert_plaintext_layout_matches_spec() {
        let p = AdvertPlaintext::new(
            [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a],
            [0xaa, 0xbb, 0xcc],
            0x0100,
        );
        let raw = p.to_bytes();
        assert_eq!(raw[0], 0);
        assert_eq!(
            &raw[1..11],
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a]
        );
        assert_eq!(&raw[11..14], &[0xaa, 0xbb, 0xcc]);
        // little-endian domain
        assert_eq!(raw[14], 0x00);
        assert_eq!(raw[15], 0x01);
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}
