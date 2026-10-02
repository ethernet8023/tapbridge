//! One atomic host identity/pairing record, protected by the operating system.
//!
//! Record format `TBID2` (plaintext before DPAPI):
//! `"TBID2" | key[32] | peer_flag | peer[32]? | name_len(u16) name | push_len(u16) push`
//! The name and push fields are present only when a peer is recorded.
//! No migration: the product is unreleased; a `TBID1` record is corrupt state.

use iroh::{EndpointId, SecretKey};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub struct Store {
    pub key: SecretKey,
    pub peer: Option<EndpointId>,
    /// Phone display name from the newest accepted Hello, if stored.
    pub peer_name: Option<String>,
    /// Newest push token from any accepted Hello, if stored.
    pub push_token: Option<String>,
    path: PathBuf,
}

impl Store {
    pub fn open(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        let path = directory.join("identity.bin");
        match fs::read(&path) {
            Ok(data) => {
                let mut plain = unprotect(&data)?;
                let result = (|| {
                    if plain.len() < 38 || &plain[..5] != b"TBID2" {
                        return Err(invalid());
                    }
                    let key = SecretKey::from_bytes(plain[5..37].try_into().unwrap());
                    let mut store = Self {
                        key,
                        peer: None,
                        peer_name: None,
                        push_token: None,
                        path,
                    };
                    match (plain[37], plain.len()) {
                        (0, 38) => {}
                        (1, _) if plain.len() >= 70 => {
                            store.peer = Some(
                                EndpointId::from_bytes(plain[38..70].try_into().unwrap())
                                    .map_err(|_| invalid())?,
                            );
                            let mut at = 70usize;
                            while at < plain.len() {
                                let Some(len) = read_u16(&plain[at..]) else {
                                    return Err(invalid());
                                };
                                at += 2;
                                if at + len > plain.len() {
                                    return Err(invalid());
                                }
                                if len > 0 {
                                    let bytes = &plain[at..at + len];
                                    if store.peer_name.is_none() {
                                        store.peer_name = Some(
                                            std::str::from_utf8(bytes)
                                                .map_err(|_| invalid())?
                                                .to_owned(),
                                        );
                                    } else {
                                        store.push_token = Some(
                                            std::str::from_utf8(bytes)
                                                .map_err(|_| invalid())?
                                                .to_owned(),
                                        );
                                    }
                                }
                                at += len;
                            }
                            if at != plain.len() {
                                return Err(invalid());
                            }
                        }
                        _ => return Err(invalid()),
                    }
                    Ok(store)
                })();
                plain.fill(0);
                result
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let store = Self {
                    key: SecretKey::generate(),
                    peer: None,
                    peer_name: None,
                    push_token: None,
                    path,
                };
                store.persist(&Record::default(), true)?;
                Ok(store)
            }
            Err(e) => Err(e),
        }
    }

    /// Pair with `peer`, dropping any remembered phone name or push token.
    pub fn set_peer(&mut self, peer: Option<EndpointId>) -> io::Result<()> {
        let record = Record {
            peer,
            name: None,
            push: None,
        };
        self.persist(&record, false)?;
        self.peer = peer;
        self.peer_name = None;
        self.push_token = None;
        Ok(())
    }

    /// Persist the newest phone name and/or push token from an accepted Hello.
    pub fn set_phone_info(
        &mut self,
        peer: Option<EndpointId>,
        name: Option<&str>,
        push: Option<&str>,
    ) -> io::Result<()> {
        if peer.is_none() && name.is_none() && push.is_none() {
            return Ok(());
        }
        let peer = peer.or(self.peer);
        let name = name.or(self.peer_name.as_deref());
        let push = push.or(self.push_token.as_deref());
        let record = Record {
            peer,
            name: name.map(str::to_owned),
            push: push.map(str::to_owned),
        };
        self.persist(&record, false)?;
        self.peer = peer;
        self.peer_name = name.map(str::to_owned);
        self.push_token = push.map(str::to_owned);
        Ok(())
    }

    fn persist(&self, record: &Record, create: bool) -> io::Result<()> {
        let mut plain = b"TBID2".to_vec();
        plain.extend_from_slice(&self.key.to_bytes());
        plain.push(u8::from(record.peer.is_some()));
        if let Some(peer) = record.peer {
            plain.extend_from_slice(peer.as_bytes());
        }
        if let Some(_peer) = record.peer {
            for field in [record.name.as_deref(), record.push.as_deref()] {
                write_u16(&mut plain, field.map_or(0, str::len).try_into().unwrap());
                if let Some(field) = field {
                    plain.extend_from_slice(field.as_bytes());
                }
            }
        }
        let protected = protect(&plain);
        plain.fill(0);
        let protected = protected?;
        let parent = self.path.parent().ok_or_else(invalid)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&protected)?;
        temp.as_file().sync_all()?;
        if create {
            temp.persist_noclobber(&self.path)
        } else {
            temp.persist(&self.path)
        }
        .map_err(|e| e.error)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

#[derive(Default)]
struct Record {
    peer: Option<EndpointId>,
    name: Option<String>,
    push: Option<String>,
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u16(input: &[u8]) -> Option<usize> {
    if input.len() < 2 {
        return None;
    }
    Some(u16::from_le_bytes([input[0], input[1]]) as usize)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid saved identity; restore it or explicitly re-pair",
    )
}

#[cfg(windows)]
fn crypt(data: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len().try_into().map_err(|_| invalid())?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // DPAPI borrows the input for this call; it allocates output with LocalAlloc.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // Successful DPAPI output is valid for cbData bytes; zero plaintext before freeing.
    let result = unsafe {
        let bytes = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let copy = bytes.to_vec();
        if !encrypt {
            bytes.fill(0);
        }
        LocalFree(output.pbData.cast());
        copy
    };
    Ok(result)
}

#[cfg(windows)]
fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    crypt(data, true)
}
#[cfg(windows)]
fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    crypt(data, false)
}
#[cfg(not(windows))]
fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}
#[cfg(not(windows))]
fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_state_fails_without_replacing_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.bin");
        fs::write(&path, b"broken").unwrap();
        assert!(Store::open(dir.path()).is_err());
        assert_eq!(fs::read(path).unwrap(), b"broken");
    }

    #[test]
    fn failed_persistence_does_not_change_in_memory_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        fs::remove_file(dir.path().join("identity.bin")).unwrap();
        fs::create_dir(dir.path().join("identity.bin")).unwrap();
        assert!(
            store
                .set_peer(Some(SecretKey::generate().public()))
                .is_err()
        );
        assert!(store.peer.is_none());
    }

    #[test]
    fn revocation_is_persisted_without_changing_host_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let id = store.key.public();
        store
            .set_peer(Some(SecretKey::generate().public()))
            .unwrap();
        store.set_peer(None).unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.key.public(), id);
        assert!(store.peer.is_none());
        assert!(store.peer_name.is_none());
        assert!(store.push_token.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_record_is_dpapi_protected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let disk = fs::read(dir.path().join("identity.bin")).unwrap();
        assert!(!disk.windows(32).any(|bytes| bytes == store.key.to_bytes()));
        assert!(!disk.starts_with(b"TBID1"));
        assert!(!disk.starts_with(b"TBID2"));
        assert_eq!(&unprotect(&disk).unwrap()[..5], b"TBID2");
    }

    #[test]
    fn identity_and_pairing_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let peer = SecretKey::generate().public();
        let id = {
            let mut store = Store::open(dir.path()).unwrap();
            store.set_peer(Some(peer)).unwrap();
            store.key.public()
        };
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.key.public(), id);
        assert_eq!(store.peer, Some(peer));
    }

    #[test]
    fn phone_name_and_push_token_persist_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let peer = SecretKey::generate().public();
        {
            let mut store = Store::open(dir.path()).unwrap();
            store.set_peer(Some(peer)).unwrap();
            store
                .set_phone_info(None, Some("Pixel 9"), Some("fcm-token-1"))
                .unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.peer, Some(peer));
        assert_eq!(store.peer_name.as_deref(), Some("Pixel 9"));
        assert_eq!(store.push_token.as_deref(), Some("fcm-token-1"));
    }

    #[test]
    fn newest_accepted_hello_name_and_token_win() {
        let dir = tempfile::tempdir().unwrap();
        let peer = SecretKey::generate().public();
        let mut store = Store::open(dir.path()).unwrap();
        store
            .set_phone_info(Some(peer), Some("Pixel 9"), Some("tok-1"))
            .unwrap();
        store
            .set_phone_info(None, Some("Renamed"), Some("tok-2"))
            .unwrap();
        assert_eq!(store.peer_name.as_deref(), Some("Renamed"));
        assert_eq!(store.push_token.as_deref(), Some("tok-2"));
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.peer_name.as_deref(), Some("Renamed"));
        assert_eq!(store.push_token.as_deref(), Some("tok-2"));
    }

    #[test]
    fn set_peer_clears_remembered_phone_info() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let peer = SecretKey::generate().public();
        store
            .set_phone_info(Some(peer), Some("Pixel 9"), Some("tok"))
            .unwrap();
        let new_peer = SecretKey::generate().public();
        store.set_peer(Some(new_peer)).unwrap();
        assert!(store.peer_name.is_none());
        assert!(store.push_token.is_none());
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.peer, Some(new_peer));
        assert!(store.peer_name.is_none());
        assert!(store.push_token.is_none());
    }

    #[test]
    fn token_can_persist_without_pairing_yet() {
        // A pinned phone cannot exist before pairing, but the API must not
        // silently drop a push-only update when called defensively.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let peer = SecretKey::generate().public();
        store
            .set_phone_info(Some(peer), Some("Pixel"), Some("tok"))
            .unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.push_token.as_deref(), Some("tok"));
    }
}
