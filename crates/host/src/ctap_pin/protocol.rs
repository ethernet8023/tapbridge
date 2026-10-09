//! CTAP 2.1 PIN/UV auth protocols 1 and 2 (§6.5.6, §6.5.7): P-256 ECDH
//! key agreement, the shared-secret KDF, AES-256-CBC and HMAC-SHA-256.
//! Used on both sides of the proxy (the platform-facing virtual
//! authenticator and the card-facing client) and by the test card.
//!
//! Secrets live in [`Zeroizing`] buffers and the types have redacted
//! `Debug`s: nothing here can end up in a log line by accident.

use super::cbor::{self, Value};
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::NoPadding};
use hmac::{Hmac, Mac};
use p256::{
    EncodedPoint, PublicKey, SecretKey,
    elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint},
};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    One,
    Two,
}

impl Protocol {
    pub fn from_number(number: u64) -> Option<Self> {
        match number {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            _ => None,
        }
    }

    pub fn number(self) -> u64 {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }

    /// pinUvAuthParam over `message` with `key` (a token or a shared
    /// secret's HMAC half): 16 bytes for protocol 1, 32 for protocol 2.
    pub fn authenticate(self, key: &[u8], message: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC takes any key length");
        mac.update(message);
        let tag = mac.finalize().into_bytes();
        match self {
            Self::One => tag[..16].to_vec(),
            Self::Two => tag.to_vec(),
        }
    }

    /// Constant-time check of a pinUvAuthParam. Protocol 1 also accepts
    /// only the 16-byte truncation (CTAP 2.1 §6.5.6 verify).
    pub fn verify(self, key: &[u8], message: &[u8], signature: &[u8]) -> bool {
        let expected = self.authenticate(key, message);
        expected.len() == signature.len()
            && expected
                .iter()
                .zip(signature)
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

/// Fills `buf` from the OS RNG.
pub fn random(buf: &mut [u8]) {
    getrandom::getrandom(buf).expect("OS random number generator");
}

/// An ephemeral P-256 key-agreement key (the authenticator's, or the
/// platform's for one exchange).
pub struct KeyAgreement {
    secret: SecretKey,
}

impl std::fmt::Debug for KeyAgreement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyAgreement(REDACTED)")
    }
}

impl KeyAgreement {
    pub fn generate() -> Self {
        loop {
            let mut scalar = Zeroizing::new([0u8; 32]);
            random(scalar.as_mut());
            if let Some(key) = Self::from_scalar(&scalar) {
                return key;
            }
        }
    }

    /// From a fixed scalar (tests); None when out of range.
    pub fn from_scalar(scalar: &[u8; 32]) -> Option<Self> {
        SecretKey::from_slice(scalar)
            .ok()
            .map(|secret| Self { secret })
    }

    /// The public key as the COSE_Key CTAP sends (§6.5.6 getPublicKey):
    /// kty EC2, alg -25 (ECDH-ES+HKDF-256), crv P-256, x, y.
    pub fn cose_public(&self) -> Value {
        let point = self.secret.public_key().to_encoded_point(false);
        cbor::int_map(vec![
            (1, Value::Uint(2)),
            (3, Value::int(-25)),
            (-1, Value::Uint(1)),
            (-2, Value::Bytes(point.x().expect("uncompressed").to_vec())),
            (-3, Value::Bytes(point.y().expect("uncompressed").to_vec())),
        ])
    }

    /// ECDH with the peer's COSE key, then the protocol's KDF. None when
    /// the peer key is malformed or not on the curve.
    pub fn shared_secret(&self, protocol: Protocol, peer: &Value) -> Option<SharedSecret> {
        let x: [u8; 32] = peer.get(-2)?.as_bytes()?.try_into().ok()?;
        let y: [u8; 32] = peer.get(-3)?.as_bytes()?.try_into().ok()?;
        if peer.get(1)?.as_uint()? != 2 || peer.get(-1)?.as_uint()? != 1 {
            return None;
        }
        let point = EncodedPoint::from_affine_coordinates(&x.into(), &y.into(), false);
        let public = Option::<PublicKey>::from(PublicKey::from_encoded_point(&point))?;
        let shared =
            p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), public.as_affine());
        let z = Zeroizing::new(shared.raw_secret_bytes().to_vec());
        Some(SharedSecret::from_z(protocol, &z))
    }
}

/// The shared secret of one key agreement: 32 bytes (protocol 1) or the
/// HMAC key || AES key (protocol 2).
pub struct SharedSecret {
    protocol: Protocol,
    bytes: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SharedSecret({:?}, REDACTED)", self.protocol)
    }
}

impl SharedSecret {
    fn from_z(protocol: Protocol, z: &[u8]) -> Self {
        let bytes = match protocol {
            Protocol::One => Sha256::digest(z).to_vec(),
            Protocol::Two => {
                let hkdf = hkdf::Hkdf::<Sha256>::new(Some(&[0u8; 32]), z);
                let mut out = vec![0u8; 64];
                hkdf.expand(b"CTAP2 HMAC key", &mut out[..32])
                    .expect("32 bytes is a valid HKDF length");
                hkdf.expand(b"CTAP2 AES key", &mut out[32..])
                    .expect("32 bytes is a valid HKDF length");
                out
            }
        };
        Self {
            protocol,
            bytes: Zeroizing::new(bytes),
        }
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    fn aes_key(&self) -> &[u8] {
        match self.protocol {
            Protocol::One => &self.bytes,
            Protocol::Two => &self.bytes[32..],
        }
    }

    fn hmac_key(&self) -> &[u8] {
        match self.protocol {
            Protocol::One => &self.bytes,
            Protocol::Two => &self.bytes[..32],
        }
    }

    /// encrypt(): zero IV (protocol 1) or a random IV prepended
    /// (protocol 2). `plaintext` must be whole AES blocks.
    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        let mut iv = [0u8; 16];
        if self.protocol == Protocol::Two {
            random(&mut iv);
        }
        self.encrypt_with_iv(plaintext, iv)
    }

    fn encrypt_with_iv(&self, plaintext: &[u8], iv: [u8; 16]) -> Vec<u8> {
        assert!(plaintext.len().is_multiple_of(16), "whole AES blocks only");
        let mut buf = plaintext.to_vec();
        let len = buf.len();
        Aes256CbcEnc::new_from_slices(self.aes_key(), &iv)
            .expect("32-byte key, 16-byte IV")
            .encrypt_padded_mut::<NoPadding>(&mut buf, len)
            .expect("block aligned");
        match self.protocol {
            Protocol::One => buf,
            Protocol::Two => {
                let mut out = iv.to_vec();
                out.extend_from_slice(&buf);
                out
            }
        }
    }

    /// decrypt(): None when the length is not whole blocks (plus the IV
    /// for protocol 2).
    pub fn decrypt(&self, ciphertext: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        let (iv, body) = match self.protocol {
            Protocol::One => ([0u8; 16], ciphertext),
            Protocol::Two => {
                if ciphertext.len() < 16 {
                    return None;
                }
                let (iv, body) = ciphertext.split_at(16);
                (iv.try_into().ok()?, body)
            }
        };
        if body.is_empty() || !body.len().is_multiple_of(16) {
            return None;
        }
        let mut buf = Zeroizing::new(body.to_vec());
        Aes256CbcDec::new_from_slices(self.aes_key(), &iv)
            .ok()?
            .decrypt_padded_mut::<NoPadding>(&mut buf)
            .ok()?;
        Some(buf)
    }

    /// authenticate() with the shared secret (setPIN / changePIN /
    /// getPinToken-free paths).
    pub fn authenticate(&self, message: &[u8]) -> Vec<u8> {
        self.protocol.authenticate(self.hmac_key(), message)
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        self.protocol.verify(self.hmac_key(), message, signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Known answers computed independently with Python `cryptography`
    /// (scalars 0x11.. and 0x22..).
    const B_X: &str = "d65a93977caa3d1b081852ff57a79e465f1660577304baead505dd3a48589cf3";
    const B_Y: &str = "50185e895372df6221ea3a137557e473fddb6755f05bd507c3c533fce9c91285";
    const P1_SHARED: &str = "98f5bf15dbc72627acd9a8ab61ce21349ea3496d79136adf08ad381cc9c0fa25";
    const P2_SHARED: &str = "abc3809533cb07b6ae481ea05b28ac708ffed9777147a09aabc8ab58e167ba0eaecad75245932defecf4cd0d93420fcd05d99403d09a49655fed76fa64c2cf82";
    const P1_CT: &str = "3a6fce1f2c0517ade8dc3b3f9f6dbb4b735732c052d19aafbcf85f336946cd07";
    const P2_CT: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a62126976d5814e4384a886b7c1c8d4b2bc880e06000dbbf2eef838f7bf233c37";
    const P1_AUTH: &str = "4deb3fca792685e7b27182320dd993a5";
    const P2_AUTH: &str = "0aa380d6a537ef2be791aa0ece20c26f85b6b363a963b06bb48602b6db19d698";

    fn pair() -> (KeyAgreement, KeyAgreement) {
        (
            KeyAgreement::from_scalar(&[0x11; 32]).unwrap(),
            KeyAgreement::from_scalar(&[0x22; 32]).unwrap(),
        )
    }

    #[test]
    fn cose_public_key_carries_the_point() {
        let (_, b) = pair();
        let cose = b.cose_public();
        assert_eq!(cose.get(1), Some(&Value::Uint(2)));
        assert_eq!(cose.get(3), Some(&Value::int(-25)));
        assert_eq!(cose.get(-1), Some(&Value::Uint(1)));
        assert_eq!(cose.get(-2).unwrap().as_bytes().unwrap(), hex(B_X));
        assert_eq!(cose.get(-3).unwrap().as_bytes().unwrap(), hex(B_Y));
    }

    #[test]
    fn protocol_one_matches_known_answers() {
        let (a, b) = pair();
        let shared = a.shared_secret(Protocol::One, &b.cose_public()).unwrap();
        assert_eq!(*shared.bytes, hex(P1_SHARED));
        let plaintext: Vec<u8> = (0..32).collect();
        assert_eq!(shared.encrypt(&plaintext), hex(P1_CT));
        assert_eq!(*shared.decrypt(&hex(P1_CT)).unwrap(), plaintext);
        assert_eq!(shared.authenticate(b"hello"), hex(P1_AUTH));
        assert!(shared.verify(b"hello", &hex(P1_AUTH)));
    }

    #[test]
    fn protocol_two_matches_known_answers() {
        let (a, b) = pair();
        let shared = a.shared_secret(Protocol::Two, &b.cose_public()).unwrap();
        assert_eq!(*shared.bytes, hex(P2_SHARED));
        let plaintext: Vec<u8> = (0..32).collect();
        assert_eq!(shared.encrypt_with_iv(&plaintext, [0x5A; 16]), hex(P2_CT));
        assert_eq!(*shared.decrypt(&hex(P2_CT)).unwrap(), plaintext);
        assert_eq!(shared.authenticate(b"hello"), hex(P2_AUTH));
    }

    #[test]
    fn both_sides_derive_the_same_secret() {
        let (a, b) = pair();
        for protocol in [Protocol::One, Protocol::Two] {
            let ab = a.shared_secret(protocol, &b.cose_public()).unwrap();
            let ba = b.shared_secret(protocol, &a.cose_public()).unwrap();
            assert_eq!(*ab.bytes, *ba.bytes);
        }
    }

    #[test]
    fn protocol_two_uses_a_fresh_random_iv() {
        let (a, b) = pair();
        let shared = a.shared_secret(Protocol::Two, &b.cose_public()).unwrap();
        let block = [7u8; 16];
        let (one, two) = (shared.encrypt(&block), shared.encrypt(&block));
        assert_eq!(one.len(), 32);
        assert_ne!(one, two);
        assert_eq!(*shared.decrypt(&one).unwrap(), block);
    }

    #[test]
    fn bad_peer_keys_and_ciphertext_lengths_are_refused() {
        let (a, _) = pair();
        let off_curve = cbor::int_map(vec![
            (1, Value::Uint(2)),
            (3, Value::int(-25)),
            (-1, Value::Uint(1)),
            (-2, Value::Bytes(vec![1; 32])),
            (-3, Value::Bytes(vec![2; 32])),
        ]);
        assert!(a.shared_secret(Protocol::One, &off_curve).is_none());
        assert!(a.shared_secret(Protocol::One, &Value::Uint(1)).is_none());
        let shared = a
            .shared_secret(Protocol::Two, &pair().1.cose_public())
            .unwrap();
        assert!(shared.decrypt(&[0; 16]).is_none(), "IV only");
        assert!(shared.decrypt(&[0; 40]).is_none(), "partial block");
    }

    #[test]
    fn verify_rejects_wrong_or_truncated_params() {
        let key = [9u8; 32];
        let good = Protocol::Two.authenticate(&key, b"m");
        assert!(Protocol::Two.verify(&key, b"m", &good));
        assert!(!Protocol::Two.verify(&key, b"m", &good[..16]));
        assert!(!Protocol::Two.verify(&key, b"n", &good));
        assert!(Protocol::One.verify(&key, b"m", &good[..16]));
        assert!(!Protocol::One.verify(&key, b"m", &[]));
    }

    #[test]
    fn debug_output_is_redacted() {
        let (a, b) = pair();
        let shared = a.shared_secret(Protocol::One, &b.cose_public()).unwrap();
        assert!(!format!("{shared:?}").contains("98"));
        assert_eq!(format!("{a:?}"), "KeyAgreement(REDACTED)");
    }
}
