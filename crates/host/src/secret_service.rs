//! Linux at-rest protection for the host identity record, the same envelope
//! as the macOS keychain one: a random 32-byte AES-256-GCM key lives in the
//! freedesktop Secret Service (gnome-keyring, KWallet, KeePassXC), and
//! `identity.bin` stores only `TBGCM1 | nonce[12] | ciphertext+tag`.
//!
//! The caller decides what happens when no Secret Service is running (see
//! `state::protect`): a first write falls back to a 0600 file, but a record
//! that was ever sealed here is never opened without its key.
//!
//! The key is created get-first and never overwritten; host.lock already
//! serializes first runs, so there is no add-only race to handle.

use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, AeadCore, Key, KeyInit, OsRng},
};
use std::io;

pub const MAGIC: &[u8] = b"TBGCM1";
const TAG_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const SERVICE: &str = "computer.ethernet.tapbridge.host-record";
const ACCOUNT: &str = "record key";

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid saved identity; restore it or explicitly re-pair",
    )
}

fn keyring_error(error: keyring::Error) -> io::Error {
    io::Error::other(format!("secret service: {error}"))
}

/// The record key from the Secret Service, created on first use.
fn record_key() -> io::Result<[u8; 32]> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(keyring_error)?;
    match entry.get_secret() {
        Ok(found) => found.try_into().map_err(|_| invalid()),
        // Only a genuinely missing entry may be created: a locked or absent
        // service must surface, or the key the record is sealed with could
        // be replaced and the identity destroyed.
        Err(keyring::Error::NoEntry) => {
            let mut key = [0u8; 32];
            getrandom::getrandom(&mut key).map_err(io::Error::other)?;
            entry.set_secret(&key).map_err(keyring_error)?;
            Ok(key)
        }
        Err(error) => Err(keyring_error(error)),
    }
}

/// Seals `data` with the Secret Service key.
pub fn seal(data: &[u8]) -> io::Result<Vec<u8>> {
    seal_with(&record_key()?, data)
}

/// Opens a [`seal`] envelope. Fails closed on any tampering.
pub fn open(envelope: &[u8]) -> io::Result<Vec<u8>> {
    if envelope.len() < MAGIC.len() + NONCE_LEN + TAG_LEN || &envelope[..MAGIC.len()] != MAGIC {
        return Err(invalid());
    }
    open_with(&record_key()?, envelope)
}

fn seal_with(key: &[u8; 32], data: &[u8]) -> io::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, data)
        .map_err(|_| io::Error::other("record encryption failed"))?;
    let mut envelope = Vec::with_capacity(MAGIC.len() + nonce.len() + ciphertext.len());
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

fn open_with(key: &[u8; 32], envelope: &[u8]) -> io::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(
            aes_gcm::Nonce::from_slice(&envelope[MAGIC.len()..MAGIC.len() + NONCE_LEN]),
            &envelope[MAGIC.len() + NONCE_LEN..],
        )
        .map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_and_rejects_tampering() {
        let key = [7u8; 32];
        let sealed = seal_with(&key, b"TBID2 record").unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert_eq!(open_with(&key, &sealed).unwrap(), b"TBID2 record");
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open_with(&key, &tampered).is_err());
        assert!(open_with(&[8u8; 32], &sealed).is_err());
    }
}
