//! HKDF-SHA256 (RFC 5869), written to the CTAP 2.2 caBLEv2 derivation:
//! output = HKDF(ikm=parent, salt, info=32-bit little-endian purpose).
//!
//! Spec: CTAP 2.2 §11.5.1 "key derivation" — "the info value is a 32-bit,
//! little-endian, purpose identifier"; salt is optional.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// One-shot HKDF-Expand-free HKDF (extract + expand), for outputs <= 255*digest.
pub fn derive_purpose(parent: &[u8], salt: Option<&[u8]>, purpose: u32, out: &mut [u8]) {
    assert!(purpose < 0x100, "caBLEv2 purpose must fit one byte");
    let info = [purpose as u8, 0, 0, 0];
    hkdf_sha256(parent, salt, &info, out);
}

/// HKDF-SHA256 per RFC 5869. `out.len()` must be <= 255 * 32.
pub fn hkdf_sha256(ikm: &[u8], salt: Option<&[u8]>, info: &[u8], out: &mut [u8]) {
    let salt = salt.filter(|s| !s.is_empty()).unwrap_or(&[0u8; 32][..]);
    // Extract
    let mut mac = <HmacSha256 as Mac>::new_from_slice(salt).expect("HMAC accepts any key length");
    mac.update(ikm);
    let prk = mac.finalize().into_bytes();

    // Expand
    let mut t: Vec<u8> = Vec::with_capacity(32);
    let mut counter: u8 = 1;
    let mut produced = 0;
    while produced < out.len() {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&prk).expect("hmac key");
        mac.update(&t);
        mac.update(info);
        mac.update(&[counter]);
        let block = mac.finalize().into_bytes();
        let take = (out.len() - produced).min(32);
        out[produced..produced + take].copy_from_slice(&block[..take]);
        t.extend_from_slice(&block);
        produced += take;
        counter = counter
            .checked_add(1)
            .expect("out.len <= 255*32 keeps counter in range");
    }
}
