//! Linux: registering a card's FIDO credential with pam_u2f, so a tap on the
//! phone approves `sudo` and `su` (the NixOS module's `pam.sudo` / `pam.su`).
//!
//! The credential is made by `pamu2fcfg` talking to tapbridge's own virtual
//! security key, which relays to the card on the phone. pam_u2f reads
//! `$XDG_CONFIG_HOME/Yubico/u2f_keys` (else `~/.config/Yubico/u2f_keys`): one
//! line per user, `user:cred:cred`, each `cred` being
//! `keyhandle,pubkey,type,options` (pam-u2f's `parse_native_format`; it keeps
//! only the LAST line for a user, so a second card goes on the same line).
//!
//! pam_u2f cannot say which card a credential belongs to, so tapbridge keeps
//! its own `u2f_cards` file in its state dir: `<uid hex> <keyhandle>` per card.
//! That is what lets "Remove" take out exactly this card's credential, and
//! lets a hand-edited authfile show up as "not registered".

use std::{
    ffi::OsString,
    fs, io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
};

/// Set by the Nix modules to the store path of `pamu2fcfg`.
pub const PAMU2FCFG_ENV: &str = "TAPBRIDGE_PAMU2FCFG";

/// Where pam_u2f looks without an `authfile=` setting.
pub fn authfile() -> Option<PathBuf> {
    authfile_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

fn authfile_from(xdg_config: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let config = match xdg_config.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(home.filter(|home| !home.is_empty())?).join(".config"),
    };
    Some(config.join("Yubico/u2f_keys"))
}

/// The two files this feature touches.
pub struct Paths {
    /// pam_u2f's authfile.
    pub authfile: PathBuf,
    /// tapbridge's card to key handle table.
    pub cards: PathBuf,
}

impl Paths {
    pub fn current() -> Option<Self> {
        Some(Self {
            authfile: authfile()?,
            cards: crate::linux::state_dir().join("u2f_cards"),
        })
    }
}

/// The key handle of a credential (`keyhandle,pubkey,type,options`).
fn key_handle(credential: &str) -> &str {
    credential.split(',').next().unwrap_or("")
}

/// `text` with `credential` on `user`'s line (the last one, as pam_u2f reads
/// it), or on a new line. Adding a credential that is already there changes
/// nothing.
pub fn add_credential(text: &str, user: &str, credential: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let owner = |line: &String| line.split(':').next() == Some(user);
    match lines.iter().rposition(owner) {
        Some(at) => {
            let handle = key_handle(credential);
            let there = lines[at]
                .split(':')
                .skip(1)
                .any(|existing| key_handle(existing) == handle);
            if !there {
                lines[at].push(':');
                lines[at].push_str(credential);
            }
        }
        None => lines.push(format!("{user}:{credential}")),
    }
    lines.join("\n") + "\n"
}

/// `text` without the credential with this key handle. A user left with no
/// credentials loses the line.
pub fn remove_credential(text: &str, user: &str, handle: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split(':');
        if fields.next() != Some(user) {
            out.push(line.to_owned());
            continue;
        }
        let kept: Vec<&str> = fields.filter(|cred| key_handle(cred) != handle).collect();
        if !kept.is_empty() {
            out.push(format!("{user}:{}", kept.join(":")));
        }
    }
    if out.is_empty() {
        String::new()
    } else {
        out.join("\n") + "\n"
    }
}

pub fn has_credential(text: &str, user: &str, handle: &str) -> bool {
    text.lines()
        .filter(|line| line.split(':').next() == Some(user))
        .flat_map(|line| line.split(':').skip(1))
        .any(|cred| key_handle(cred) == handle)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The key handle registered for this card, if any.
fn card_handle<'a>(cards: &'a str, uid: &[u8]) -> Option<&'a str> {
    let uid = hex(uid);
    cards.lines().find_map(|line| {
        let (card, handle) = line.split_once(' ')?;
        (card == uid).then_some(handle)
    })
}

fn set_card_handle(cards: &str, uid: &[u8], handle: &str) -> String {
    let mut text = clear_card(cards, uid);
    text.push_str(&format!("{} {handle}\n", hex(uid)));
    text
}

fn clear_card(cards: &str, uid: &[u8]) -> String {
    let uid = hex(uid);
    cards
        .lines()
        .filter(|line| line.split_once(' ').map(|(card, _)| card) != Some(uid.as_str()))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Replaces `path` with `text` (0644 like a hand-made authfile; pam_u2f warns
/// about group or world writable ones), creating its directory.
fn write(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tapbridge-new");
    let temp = PathBuf::from(temp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(temp, path)
}

fn read(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        other => other,
    }
}

/// tapbridge's virtual key, as `HID_ID` writes it (bus 3 = USB).
const OWN_HID_ID: &str = "0003:00001209:00007A73";

/// hidraw devices that speak FIDO (usage page 0xF1D0) and are not tapbridge's
/// own key. pamu2fcfg 1.4.0 has no way to pick a device: it registers on the
/// first FIDO key it finds, so with a real key plugged in the credential could
/// land on that instead.
pub fn other_fido_keys(sysfs_hidraw: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(sysfs_hidraw) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let device = entry.path().join("device");
            let descriptor = fs::read(device.join("report_descriptor")).ok()?;
            // `06 D0 F1`: Usage Page (FIDO Alliance), the first item of a key's descriptor.
            if !descriptor.starts_with(&[0x06, 0xD0, 0xF1]) {
                return None;
            }
            let uevent = fs::read_to_string(device.join("uevent")).ok()?;
            let id = uevent
                .lines()
                .find_map(|line| line.strip_prefix("HID_ID="))?;
            let name = uevent
                .lines()
                .find_map(|line| line.strip_prefix("HID_NAME="))
                .unwrap_or("a security key");
            (!id.eq_ignore_ascii_case(OWN_HID_ID)).then(|| name.to_owned())
        })
        .collect();
    found.sort();
    found
}

/// What to tell the user when another FIDO key is plugged in, or `None`.
pub fn other_keys_warning(sysfs_hidraw: &Path) -> Option<String> {
    let others = other_fido_keys(sysfs_hidraw);
    (!others.is_empty()).then(|| {
        format!(
            "Unplug {} first: the key would be made on it instead of on your card.",
            others.join(", ")
        )
    })
}

/// Makes a credential on the card that is on the phone now (`pamu2fcfg`
/// prints `user:keyhandle,pubkey,type,options`) and registers it for
/// `uid`. `make` runs the tool; it returns its stdout, or what to tell the
/// user. Returns (ok, one sentence for the user).
pub fn register(
    paths: &Paths,
    uid: &[u8],
    make: impl FnOnce() -> Result<String, String>,
) -> (bool, String) {
    if let Some(why) = other_keys_warning(Path::new("/sys/class/hidraw")) {
        return (false, why);
    }
    let line = match make() {
        Ok(line) => line,
        Err(why) => return (false, why),
    };
    let Some((user, credential)) = line.trim().split_once(':') else {
        return (false, "pamu2fcfg printed nothing usable.".to_owned());
    };
    if user.is_empty() || credential.is_empty() {
        return (false, "pamu2fcfg printed nothing usable.".to_owned());
    }
    let result = (|| {
        let authfile = add_credential(&read(&paths.authfile)?, user, credential);
        write(&paths.authfile, &authfile)?;
        let cards = set_card_handle(&read(&paths.cards)?, uid, key_handle(credential));
        write(&paths.cards, &cards)
    })();
    match result {
        Ok(()) => (
            true,
            "Registered: sudo and su can use a tap of this card.".to_owned(),
        ),
        Err(error) => (false, format!("Couldn't save the key: {error}")),
    }
}

/// Takes this card's credential out of the authfile. Returns (ok, sentence).
pub fn unregister(paths: &Paths, user: &str, uid: &[u8]) -> (bool, String) {
    let result = (|| {
        let cards = read(&paths.cards)?;
        let Some(handle) = card_handle(&cards, uid) else {
            return Ok(Some(
                "tapbridge didn't register this card. Edit ~/.config/Yubico/u2f_keys to remove it."
                    .to_owned(),
            ));
        };
        let authfile = remove_credential(&read(&paths.authfile)?, user, handle);
        write(&paths.authfile, &authfile)?;
        write(&paths.cards, &clear_card(&cards, uid))?;
        Ok::<_, io::Error>(None)
    })();
    match result {
        Ok(None) => (
            true,
            "Removed: sudo and su no longer accept this card.".to_owned(),
        ),
        Ok(Some(why)) => (false, why),
        Err(error) => (false, format!("Couldn't change the key file: {error}")),
    }
}

/// Whether this card's registered credential is still in the authfile.
pub fn registered(paths: &Paths, user: &str, uid: &[u8]) -> bool {
    let (Ok(cards), Ok(authfile)) = (read(&paths.cards), read(&paths.authfile)) else {
        return false;
    };
    card_handle(&cards, uid).is_some_and(|handle| has_credential(&authfile, user, handle))
}

/// Runs `pamu2fcfg` (the Nix modules point `TAPBRIDGE_PAMU2FCFG` at it). It
/// blocks until the card answers, which the virtual key bounds itself.
pub fn run_pamu2fcfg() -> Result<String, String> {
    let program = std::env::var_os(PAMU2FCFG_ENV).unwrap_or_else(|| "pamu2fcfg".into());
    let output = Command::new(&program).output().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            "pamu2fcfg isn't installed. Install pam_u2f (on NixOS, turn on \
             services.tapbridge.pam.sudo)."
                .to_owned()
        } else {
            format!("Couldn't run pamu2fcfg: {error}")
        }
    })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let last = stderr.lines().rev().find(|line| !line.trim().is_empty());
    Err(match last {
        Some(line) => format!("Registration failed: {}", line.trim()),
        None => "Registration failed.".to_owned(),
    })
}

/// The logged-in user, as pam_u2f will look it up.
pub fn current_user() -> Option<String> {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|user| !user.is_empty() && !user.contains([':', '\n']))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "kh1==,pk1==,es256,+presence";
    const B: &str = "kh2==,pk2==,es256,+presence";
    const UID: [u8; 4] = [4, 1, 2, 3];

    #[test]
    fn authfile_follows_xdg_then_home() {
        let some = |text: &str| Some(OsString::from(text));
        assert_eq!(
            authfile_from(some("/x/cfg"), some("/h")),
            Some(PathBuf::from("/x/cfg/Yubico/u2f_keys"))
        );
        assert_eq!(
            authfile_from(some(""), some("/h")),
            Some(PathBuf::from("/h/.config/Yubico/u2f_keys"))
        );
        assert_eq!(authfile_from(None, None), None);
    }

    #[test]
    fn a_second_card_goes_on_the_same_line() {
        let one = add_credential("", "ethie", A);
        assert_eq!(one, format!("ethie:{A}\n"));
        let two = add_credential(&one, "ethie", B);
        assert_eq!(two, format!("ethie:{A}:{B}\n"));
        // pam_u2f reads the last line of a user, and so do we.
        let split = format!("ethie:{A}\nroot:x,y,es256,\nethie:{B}\n");
        let more = add_credential(&split, "ethie", "kh3==,pk3==,es256,+presence");
        assert!(
            more.lines()
                .last()
                .unwrap()
                .starts_with(&format!("ethie:{B}:kh3=="))
        );
    }

    #[test]
    fn registering_twice_does_not_duplicate() {
        let once = add_credential("", "ethie", A);
        assert_eq!(add_credential(&once, "ethie", A), once);
    }

    #[test]
    fn other_users_lines_are_left_alone() {
        let text = "root:r,r,es256,\n";
        let added = add_credential(text, "ethie", A);
        assert_eq!(added, format!("root:r,r,es256,\nethie:{A}\n"));
        assert_eq!(remove_credential(&added, "ethie", "kh1=="), text);
    }

    #[test]
    fn removing_one_card_keeps_the_other_and_the_last_one_drops_the_line() {
        let both = format!("ethie:{A}:{B}\n");
        let left = remove_credential(&both, "ethie", "kh1==");
        assert_eq!(left, format!("ethie:{B}\n"));
        assert_eq!(remove_credential(&left, "ethie", "kh2=="), "");
        // Unknown handle: nothing changes.
        assert_eq!(remove_credential(&both, "ethie", "nope"), both);
    }

    #[test]
    fn has_credential_matches_the_whole_key_handle() {
        let text = format!("ethie:{A}\n");
        assert!(has_credential(&text, "ethie", "kh1=="));
        assert!(!has_credential(&text, "ethie", "kh1"));
        assert!(!has_credential(&text, "root", "kh1=="));
    }

    fn paths(dir: &Path) -> Paths {
        Paths {
            authfile: dir.join("cfg/Yubico/u2f_keys"),
            cards: dir.join("state/u2f_cards"),
        }
    }

    #[test]
    fn register_then_remove_round_trips_through_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let (ok, _) = register(&paths, &UID, || Ok(format!("ethie:{A}\n")));
        assert!(ok);
        assert!(registered(&paths, "ethie", &UID));
        assert!(!registered(&paths, "ethie", &[9, 9]));
        // A second card, then remove only the first.
        let (ok, _) = register(&paths, &[7, 7], || Ok(format!("ethie:{B}\n")));
        assert!(ok);
        let (ok, text) = unregister(&paths, "ethie", &UID);
        assert!(ok, "{text}");
        assert!(!registered(&paths, "ethie", &UID));
        assert!(registered(&paths, "ethie", &[7, 7]));
        assert_eq!(
            fs::read_to_string(&paths.authfile).unwrap(),
            format!("ethie:{B}\n")
        );
    }

    #[test]
    fn an_authfile_edited_by_hand_reads_as_not_registered() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        register(&paths, &UID, || Ok(format!("ethie:{A}\n")));
        fs::write(&paths.authfile, "").unwrap();
        assert!(!registered(&paths, "ethie", &UID));
    }

    #[test]
    fn removing_a_card_tapbridge_never_registered_says_so_and_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        fs::create_dir_all(paths.authfile.parent().unwrap()).unwrap();
        fs::write(&paths.authfile, format!("ethie:{A}\n")).unwrap();
        let (ok, text) = unregister(&paths, "ethie", &UID);
        assert!(!ok);
        assert!(text.contains("didn't register"));
        assert_eq!(
            fs::read_to_string(&paths.authfile).unwrap(),
            format!("ethie:{A}\n")
        );
    }

    #[test]
    fn a_failed_tool_or_garbage_output_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let (ok, text) = register(&paths, &UID, || Err("no tap".into()));
        assert!((!ok, text.as_str()) == (true, "no tap"));
        assert!(!register(&paths, &UID, || Ok("garbage".into())).0);
        assert!(!register(&paths, &UID, || Ok("ethie:".into())).0);
        assert!(!paths.authfile.exists());
    }

    fn fake_hidraw(dir: &Path, node: &str, id: &str, name: &str, descriptor: &[u8]) {
        let device = dir.join(node).join("device");
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join("report_descriptor"), descriptor).unwrap();
        fs::write(
            device.join("uevent"),
            format!("HID_ID={id}\nHID_NAME={name}\n"),
        )
        .unwrap();
    }

    #[test]
    fn another_fido_key_is_found_but_our_own_and_other_hid_devices_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let fido = [0x06, 0xD0, 0xF1, 0x09, 0x01];
        fake_hidraw(
            dir.path(),
            "hidraw0",
            "0003:00001209:00007A73",
            "tapbridge",
            &fido,
        );
        fake_hidraw(
            dir.path(),
            "hidraw1",
            "0003:00001050:00000407",
            "Yubico YubiKey",
            &fido,
        );
        // A keyboard: generic desktop usage page (05 01), not FIDO.
        fake_hidraw(
            dir.path(),
            "hidraw2",
            "0003:000026CE:000001A2",
            "Keyboard",
            &[0x05, 0x01],
        );
        assert_eq!(other_fido_keys(dir.path()), ["Yubico YubiKey"]);
        let warning = other_keys_warning(dir.path()).unwrap();
        assert!(warning.contains("Yubico YubiKey") && warning.starts_with("Unplug"));
    }

    #[test]
    fn with_only_our_key_there_is_nothing_to_warn_about() {
        let dir = tempfile::tempdir().unwrap();
        fake_hidraw(
            dir.path(),
            "hidraw0",
            "0003:00001209:00007a73",
            "tapbridge",
            &[0x06, 0xD0, 0xF1],
        );
        assert_eq!(other_keys_warning(dir.path()), None);
        assert_eq!(other_keys_warning(&dir.path().join("missing")), None);
    }

    #[test]
    fn the_authfile_is_not_group_or_world_writable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        register(&paths, &UID, || Ok(format!("ethie:{A}\n")));
        let mode = fs::metadata(&paths.authfile).unwrap().permissions().mode();
        assert_eq!(mode & 0o022, 0);
    }
}
