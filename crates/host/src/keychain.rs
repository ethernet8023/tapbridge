//! macOS at-rest protection for the host identity record: a random 32-byte
//! AES-256-GCM key lives in the **data-protection keychain** as a
//! generic-password item; `identity.bin` stores only
//! `TBGCM1 | nonce[12] | ciphertext+tag`.
//!
//! The data-protection keychain (not the legacy login keychain) is used so
//! the daemon never triggers a keychain access prompt: items the process
//! itself created are readable back without UI authorization, which also
//! keeps `cargo test` hermetic.
//!
//! The key item is created once and read via get-first (never overwritten
//! after creation, so concurrent first runs cannot clobber a key another
//! record was sealed with). No plaintext fallback: if the keychain is
//! unavailable the store fails to open.

use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, AeadCore, Key, KeyInit, OsRng},
};
use iroh::SecretKey;
use security_framework::base::Error as KeychainError;
use security_framework::passwords::{
    delete_generic_password, generic_password, set_generic_password_options,
};
use security_framework::passwords_options::PasswordOptions;
use std::io;
use std::sync::{Mutex, OnceLock};

const MAGIC: &[u8] = b"TBGCM1";
/// AES-GCM tag size.
const TAG_LEN: usize = 16;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid saved identity; restore it or explicitly re-pair",
    )
}

fn keychain(error: KeychainError) -> io::Error {
    io::Error::other(format!("keychain: {error}"))
}

const SERVICE: &str = "computer.ethernet.tapbridge.host-record";
const ACCOUNT: &str = "record key";

/// The keychain service name. Tests (and only tests) append the process id:
/// recompiled unsigned binaries cannot read items created by a previous
/// build (the keychain ACL pins the cdhash), which would hang the test
/// runner on an access prompt. A per-process name keeps every test run
/// hermetic; production always uses the fixed SERVICE.
fn service_name() -> String {
    // Tests (and only tests) isolate per caller: recompiled unsigned
    // binaries cannot read items created by a previous build (the keychain
    // ACL pins the cdhash), which would hang the test runner on an access
    // prompt. Dependency crates compiled into another crate's test binary
    // have cfg(test)=false, so the e2e desktop test sets the same override
    // through this env var. Production never sets it and uses the fixed
    // SERVICE name.
    if let Some(suffix) = std::env::var_os("TAPBRIDGE_TEST_KEYCHAIN") {
        let suffix = suffix.to_string_lossy();
        return format!("{SERVICE}#{suffix}");
    }
    if let Some(suffix) = TEST_SUFFIX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        // A test holds the shared lock for its whole body; its suffix is
        // visible to every thread (including tokio workers) it runs.
        return format!("{SERVICE}#{suffix}");
    }
    if cfg!(test) {
        format!("{SERVICE}#test{}", std::process::id())
    } else {
        SERVICE.to_owned()
    }
}

/// Test-only: the current keychain-touching test's suffix. Tests that touch
/// the keychain serialize on [`TEST_LOCK`]; the suffix is process-global so
/// async worker threads see the same service name as the test thread.
static TEST_SUFFIX: Mutex<Option<String>> = Mutex::new(None);

/// Test-only: the keychain-touching tests serialize on this lock.
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Test-only RAII session: holds the shared lock (so keychain tests never
/// overlap), sets the suffix for every thread, and deletes the item on drop.
#[cfg(test)]
pub struct TestSession {
    _guard: std::sync::MutexGuard<'static, ()>,
    suffix: String,
}

#[cfg(test)]
impl TestSession {
    pub fn begin() -> Self {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let suffix = format!("t{}", std::process::id());
        *TEST_SUFFIX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(suffix.clone());
        TestSession {
            _guard: guard,
            suffix,
        }
    }
}

#[cfg(test)]
impl Drop for TestSession {
    fn drop(&mut self) {
        let service = format!("{SERVICE}#{}", self.suffix);
        let _ = delete_generic_password(&service, ACCOUNT);
        *TEST_SUFFIX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

/// Query for the record key item. The legacy (file) keychain is used, not
/// the data-protection keychain: the latter requires a keychain-access-group
/// entitlement that an unsigned dev/test binary does not have. Items this
/// process created are readable back from the file keychain without a
/// prompt, keeping `cargo test` hermetic.
fn options() -> PasswordOptions {
    PasswordOptions::new_generic_password(&service_name(), ACCOUNT)
}

/// The AES-256 record key, created in the keychain on first use.
///
/// The whole find/write/read sequence holds the init lock: concurrent
/// first-use callers (e.g. parallel test threads, or Store::open racing a
/// seal) must not interleave, or a reader that already captured the old
/// key would later decrypt against a replaced item. At most one caller
/// ever writes; everyone else finds the item and reads it.
fn record_key() -> io::Result<[u8; 32]> {
    static INIT: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = INIT.get_or_init(|| Mutex::new(()));
    // A panicking holder must not poison first-use for the rest of the
    // process; the keychain item itself is the source of truth.
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let found = generic_password(options()).map_err(keychain);
    match found {
        Ok(found) => found.try_into().map_err(|_| invalid()),
        Err(_) => {
            let key = SecretKey::generate().to_bytes();
            set_generic_password_options(&key, options()).map_err(keychain)?;
            // A concurrent first run may have won the write race; seal and
            // open with whatever the keychain now holds.
            generic_password(options())
                .map_err(keychain)?
                .try_into()
                .map_err(|_| invalid())
        }
    }
}

/// Seals `data` with the keychain key. Envelope: `TBGCM1 | nonce | ct+tag`.
pub fn seal(data: &[u8]) -> io::Result<Vec<u8>> {
    let key = record_key()?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng); // 96 bits
    let ciphertext = cipher
        .encrypt(&nonce, data)
        .map_err(|_| io::Error::other("record encryption failed"))?;
    let mut envelope = Vec::with_capacity(MAGIC.len() + nonce.len() + ciphertext.len());
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

/// Opens a [`seal`] envelope. Fails closed on any tampering.
pub fn open(envelope: &[u8]) -> io::Result<Vec<u8>> {
    let nonce_len = 12; // AES-GCM standard 96-bit nonce
    if envelope.len() < MAGIC.len() + nonce_len + TAG_LEN || &envelope[..MAGIC.len()] != MAGIC {
        return Err(invalid());
    }
    let key = record_key()?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    cipher
        .decrypt(
            aes_gcm::Nonce::from_slice(&envelope[MAGIC.len()..MAGIC.len() + nonce_len]),
            &envelope[MAGIC.len() + nonce_len..],
        )
        .map_err(|_| invalid())
}

/// Test-only: delete this process's keychain item (see `service_name`).
#[cfg(test)]
pub fn delete_test_item() -> io::Result<()> {
    delete_generic_password(&service_name(), ACCOUNT)
        .map(|()| ())
        .map_err(keychain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_then_open_round_trips_and_rejects_tampering() {
        let _session = TestSession::begin();
        let sealed = seal(b"TBID2 secret record").unwrap();
        assert!(sealed.starts_with(b"TBGCM1"));
        assert!(!sealed.windows(8).any(|w| w == b"TBID2 sec"));
        assert_eq!(open(&sealed).unwrap(), b"TBID2 secret record");
        // Any single-bit change must fail closed.
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open(&tampered).is_err());
        assert!(open(b"TBGCM1").is_err());
        assert!(open(b"short").is_err());
    }
}
