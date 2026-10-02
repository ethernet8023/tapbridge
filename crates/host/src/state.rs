//! One atomic host identity/pairing record, protected by the operating system.

use iroh::{EndpointId, SecretKey};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub struct Store {
    pub key: SecretKey,
    pub peer: Option<EndpointId>,
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
                    if plain.len() != 38 && plain.len() != 70 {
                        return Err(invalid());
                    }
                    if &plain[..5] != b"TBID1" {
                        return Err(invalid());
                    }
                    let key = SecretKey::from_bytes(plain[5..37].try_into().unwrap());
                    let peer = match (plain[37], plain.len()) {
                        (0, 38) => None,
                        (1, 70) => Some(
                            EndpointId::from_bytes(plain[38..].try_into().unwrap())
                                .map_err(|_| invalid())?,
                        ),
                        _ => return Err(invalid()),
                    };
                    Ok(Self { key, peer, path })
                })();
                plain.fill(0);
                result
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let store = Self {
                    key: SecretKey::generate(),
                    peer: None,
                    path,
                };
                store.persist(None, true)?;
                Ok(store)
            }
            Err(e) => Err(e),
        }
    }

    pub fn set_peer(&mut self, peer: Option<EndpointId>) -> io::Result<()> {
        self.persist(peer, false)?;
        self.peer = peer;
        Ok(())
    }

    fn persist(&self, peer: Option<EndpointId>, create: bool) -> io::Result<()> {
        let mut plain = b"TBID1".to_vec();
        plain.extend_from_slice(&self.key.to_bytes());
        plain.push(u8::from(peer.is_some()));
        if let Some(peer) = peer {
            plain.extend_from_slice(peer.as_bytes());
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
    }

    #[cfg(windows)]
    #[test]
    fn windows_record_is_dpapi_protected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let disk = fs::read(dir.path().join("identity.bin")).unwrap();
        assert!(!disk.windows(32).any(|bytes| bytes == store.key.to_bytes()));
        assert!(!disk.starts_with(b"TBID1"));
        assert_eq!(&unprotect(&disk).unwrap()[..5], b"TBID1");
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
}
