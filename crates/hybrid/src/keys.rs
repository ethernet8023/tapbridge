//! caBLEv2 key derivations: EID key (64 bytes), tunnel ID (16 bytes) and the
//! Noise PSK (32 bytes). All are HKDF-SHA256 with a 32-bit little-endian
//! purpose as `info` (CTAP 2.2 §11.5.1 key derivation).

use crate::constants::{PURPOSE_EID_KEY, PURPOSE_PSK, PURPOSE_TUNNEL_ID};
use crate::hkdf::derive_purpose;

/// The 64-byte EID key: bytes 0..32 are an AES-256 key, bytes 32..64 are an
/// HMAC-SHA256 key. Never logged; [`Debug`] is redacted.
pub struct EidKey([u8; 64]);

impl EidKey {
    /// Build from exactly 64 bytes (HKDF output).
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let arr: [u8; 64] = bytes.try_into().ok()?;
        Some(Self(arr))
    }

    /// Derive the EID key for a QR-initiated transaction from the 16-byte
    /// QR secret (no salt).
    pub fn from_qr_secret(qr_secret: &[u8; 16]) -> Self {
        Self::derive(qr_secret, None)
    }

    /// Derive the EID key for a state-assisted transaction from the 32-byte
    /// link secret, salted with the client payload's 16-byte nonce.
    pub fn from_link_secret(link_secret: &[u8; 32], client_nonce: &[u8; 16]) -> Self {
        Self::derive(link_secret, Some(client_nonce))
    }

    fn derive(parent: &[u8], salt: Option<&[u8]>) -> Self {
        let mut out = [0u8; 64];
        derive_purpose(parent, salt, PURPOSE_EID_KEY, &mut out);
        Self(out)
    }

    /// AES-256 half (first 32 bytes).
    pub fn aes(&self) -> &[u8; 32] {
        self.0[..32].try_into().unwrap()
    }

    /// HMAC-SHA256 half (last 32 bytes).
    pub fn hmac(&self) -> &[u8; 32] {
        self.0[32..].try_into().unwrap()
    }
}

impl std::fmt::Debug for EidKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EidKey(REDACTED)")
    }
}

/// Derive the 16-byte tunnel ID from the 16-byte QR secret (purpose 2, no
/// salt). Identifies the exchange to the tunnel service; NOT bound to the
/// BLE advert nonce by design (spec: prevents tunnel-service brute-forcing
/// the nonce from the tunnel ID).
pub fn tunnel_id_from_qr_secret(qr_secret: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    derive_purpose(qr_secret, None, PURPOSE_TUNNEL_ID, &mut out);
    out
}

/// Derive the 32-byte Noise PSK from the QR secret (QR-initiated) or the link
/// secret (state-assisted), salted with the decrypted 16-byte advert
/// plaintext (purpose 3). The spec includes the advert so that future
/// advert-format additions are automatically authenticated.
pub fn psk_from_secret(secret: &[u8], advert_plaintext: &[u8; 16]) -> [u8; 32] {
    let mut out = [0u8; 32];
    derive_purpose(secret, Some(advert_plaintext), PURPOSE_PSK, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // Independent python/openssl vectors (see docs/HYBRID.md).
    #[test]
    fn eid_key_from_qr_secret_matches_vector() {
        let secret: [u8; 16] = unhex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let key = EidKey::from_qr_secret(&secret);
        let expected = "d06e92859fa9afce38139c04dae7f64196acfdb9a28dffb326f3a8f2cc0788e228bd73c3484f2f7de169d9b087c30e6f409e70bc86f5d65e03d762b4b0df2af7";
        assert_eq!(unhex(expected), key.0.to_vec());
    }

    #[test]
    fn tunnel_id_matches_vector() {
        let secret: [u8; 16] = unhex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        assert_eq!(
            hex(&tunnel_id_from_qr_secret(&secret)),
            "de1d8150b45f03349b2b00a52e2e893e"
        );
        // second secret/vector
        let secret2: [u8; 16] = unhex("77777777777777777777777777777777")
            .try_into()
            .unwrap();
        assert_eq!(
            hex(&tunnel_id_from_qr_secret(&secret2)),
            "d771b6bfb65db0bc1688c74d11dc9cce"
        );
    }

    #[test]
    fn psk_matches_vector() {
        let secret: [u8; 16] = unhex("77777777777777777777777777777777")
            .try_into()
            .unwrap();
        let advert: [u8; 16] = unhex("000102030405060708090aaabbcc0001")
            .try_into()
            .unwrap();
        assert_eq!(
            hex(&psk_from_secret(&secret, &advert)),
            "d1c6bfc43f355d59135fedf764ea2359f7a98132e3ab358ba80105ad79812ae7"
        );
    }

    #[test]
    fn state_assisted_eid_key_matches_vector() {
        // link secret = 32 zero bytes, client nonce = 0x42*16
        let link_secret = [0u8; 32];
        let nonce = [0x42u8; 16];
        let key = EidKey::from_link_secret(&link_secret, &nonce);
        assert_eq!(
            hex(&key.0),
            "d90db691478e349d327110ab5d6e8d9037e9cc5201b0faf52045fb6d980a024f4fd2e24f9bfd44e34161c86ff8d9d6afac4acd0e1223ab72741d344d4aa63be4"
        );
    }

    #[test]
    fn eid_key_debug_redacted() {
        let key = EidKey::from_qr_secret(&[1u8; 16]);
        assert_eq!(format!("{key:?}"), "EidKey(REDACTED)");
    }

    #[test]
    fn eid_key_halves() {
        let key = EidKey::from_qr_secret(&[1u8; 16]);
        assert_eq!(key.aes().len(), 32);
        assert_eq!(key.hmac().len(), 32);
        assert_ne!(key.aes(), key.hmac());
    }

    #[test]
    fn eid_key_from_slice_strict() {
        assert!(EidKey::from_slice(&[0u8; 63]).is_none());
        assert!(EidKey::from_slice(&[0u8; 65]).is_none());
        assert!(EidKey::from_slice(&[0u8; 64]).is_some());
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}
