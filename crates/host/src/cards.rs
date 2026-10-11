//! Cards known on this PC, synced with the phone.
//!
//! Every card is a [`CardInfo`] (see `tapbridge_core::cards`): name, the
//! phone's "open tapbridge when tapped" flag, what the card supports, and
//! whether it is paired with a PC's OS login, each with its own edit time.
//! Cards are added only when the user names one (on either device); a tap
//! of an unknown card is reported so the UI can offer that.
//!
//! The PC keeps two things the phone doesn't need: when each card was last
//! seen and its ATR (for the install instructions). Both are local only.
//!
//! Plain text file `cards.txt` in the state directory (nothing here is
//! secret), one card per line after the header:
//!
//! ```text
//! tapbridge-cards 2
//! UID LABEL_AT WAKE WAKE_AT CAPS CAPS_AT SETUP_LOGIN MAC_LOGIN_AT DELETED_AT LAST_SEEN ATR LABEL
//! ```
//!
//! Tab-separated; UID and ATR in hex, times in Unix milliseconds, flags 0/1,
//! CAPS a decimal bit set. The label is last so it may hold anything but
//! tabs and newlines. Any other file is set aside as `cards.txt.bad`.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub use tapbridge_core::cards::{CardInfo, MAX_LABEL, caps};

const HEADER: &str = "tapbridge-cards 2";
const FILE: &str = "cards.txt";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnownCard {
    pub info: CardInfo,
    /// Unix milliseconds of the last tap seen through the phone (0: never).
    pub last_seen: u64,
    pub atr: Vec<u8>,
}

/// A UID worth remembering: present, and not a random one (ISO 14443-3
/// random 4-byte UIDs start with 0x08). Matches the phone's
/// `RegisteredKeys.uidStable`.
pub fn uid_stable(uid: &[u8]) -> bool {
    !uid.is_empty() && !(uid.len() == 4 && uid[0] == 0x08)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// Cleans a user-typed label: one line, trimmed, at most [`MAX_LABEL`]
/// bytes (cut on a character boundary). `None` when nothing is left.
pub fn clean_label(label: &str) -> Option<String> {
    let line: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut trimmed = String::new();
    for c in line.trim().chars() {
        if trimmed.len() + c.len_utf8() > MAX_LABEL {
            break;
        }
        trimmed.push(c);
    }
    let trimmed = trimmed.trim_end().to_owned();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// A name the card announces in its ATR historical bytes (the phone builds
/// the ATR as `3B 8n 80 01 <historical> TCK`): the longest printable ASCII
/// run of four or more characters.
pub fn atr_name(atr: &[u8]) -> Option<String> {
    let historical = match atr {
        [0x3B, t0, 0x80, 0x01, rest @ ..] if t0 & 0xF0 == 0x80 => {
            rest.get(..(t0 & 0x0F) as usize)?
        }
        _ => return None,
    };
    historical
        .split(|byte| !(0x20..0x7F).contains(byte))
        .map(|run| String::from_utf8_lossy(run).trim().to_owned())
        .filter(|run| run.len() >= 4)
        .max_by_key(String::len)
}

/// Parses the file text; `None` when it isn't a current cards file.
/// Malformed lines are skipped; a UID seen twice keeps its first line.
pub fn parse(text: &str) -> Option<Vec<KnownCard>> {
    let mut lines = text.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let mut cards: Vec<KnownCard> = Vec::new();
    for line in lines {
        let Some(card) = parse_line(line) else {
            continue;
        };
        if !cards.iter().any(|known| known.info.uid == card.info.uid) {
            cards.push(card);
        }
    }
    Some(cards)
}

fn parse_line(line: &str) -> Option<KnownCard> {
    let mut fields = line.splitn(12, '\t');
    let mut next = || fields.next();
    let uid = unhex(next()?)?;
    let label_at = next()?.parse().ok()?;
    let wake = flag(next()?)?;
    let wake_at = next()?.parse().ok()?;
    let caps = next()?.parse().ok()?;
    let caps_at = next()?.parse().ok()?;
    let login = flag(next()?)?;
    let login_at = next()?.parse().ok()?;
    let deleted_at = next()?.parse().ok()?;
    let last_seen = next()?.parse().ok()?;
    let atr = unhex(next()?)?;
    let label = clean_label(next()?)?;
    let info = CardInfo {
        uid,
        label,
        label_at,
        wake,
        wake_at,
        caps,
        caps_at,
        login,
        login_at,
        deleted_at,
    };
    (info.valid() && uid_stable(&info.uid) && atr.len() <= tapbridge_core::wire::MAX_ATR).then_some(
        KnownCard {
            info,
            last_seen,
            atr,
        },
    )
}

fn flag(text: &str) -> Option<bool> {
    match text {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

pub fn serialize(cards: &[KnownCard]) -> String {
    let mut text = format!("{HEADER}\n");
    for card in cards {
        let info = &card.info;
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            hex(&info.uid),
            info.label_at,
            u8::from(info.wake),
            info.wake_at,
            info.caps,
            info.caps_at,
            u8::from(info.login),
            info.login_at,
            info.deleted_at,
            card.last_seen,
            hex(&card.atr),
            info.label,
        ));
    }
    text
}

/// The persistent list, forgotten cards included (as tombstones, so the
/// phone's older copy can't bring them back). Save with
/// [`KnownCards::save`] after a change.
pub struct KnownCards {
    cards: Vec<KnownCard>,
    path: PathBuf,
}

impl KnownCards {
    /// Loads `cards.txt` from `directory`. Never fails: a missing file is an
    /// empty list, and any other file is moved aside to `cards.txt.bad`.
    pub fn open(directory: &Path) -> Self {
        let path = directory.join(FILE);
        let cards = match fs::read(&path) {
            Ok(bytes) => match std::str::from_utf8(&bytes).ok().and_then(parse) {
                Some(cards) => cards,
                None => {
                    let _ = fs::rename(&path, directory.join(format!("{FILE}.bad")));
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Self { cards, path }
    }

    /// Cards the user has, oldest first (forgotten ones left out).
    pub fn list(&self) -> Vec<KnownCard> {
        self.cards
            .iter()
            .filter(|card| !card.info.deleted())
            .cloned()
            .collect()
    }

    /// Every record, forgotten ones included: what goes to the phone.
    pub fn records(&self) -> impl Iterator<Item = &CardInfo> {
        self.cards.iter().map(|card| &card.info)
    }

    pub fn get(&self, uid: &[u8]) -> Option<&KnownCard> {
        self.cards
            .iter()
            .find(|card| card.info.uid == uid && !card.info.deleted())
    }

    /// Records a tap. Returns whether the card is known (and so was
    /// updated); unknown cards are left for the user to add.
    pub fn see(&mut self, uid: &[u8], atr: &[u8], now: u64) -> bool {
        match self.live_mut(uid) {
            Some(card) => {
                card.last_seen = now;
                if !atr.is_empty() {
                    card.atr = atr.to_vec();
                }
                true
            }
            None => false,
        }
    }

    /// Adds (or revives) a card the user named, with "open tapbridge when
    /// tapped" on. `None` when the UID or label is unusable.
    pub fn add(&mut self, uid: &[u8], label: &str, atr: &[u8], now: u64) -> Option<&CardInfo> {
        let label = clean_label(label)?;
        if !uid_stable(uid) || uid.len() > tapbridge_core::wire::MAX_UID {
            return None;
        }
        let index = match self.cards.iter().position(|card| card.info.uid == uid) {
            Some(index) => {
                // Revive: newer label and wake stamps outrank the tombstone;
                // what was known about the card (caps, login) is kept.
                let info = &mut self.cards[index].info;
                let now = now.max(info.deleted_at + 1);
                info.label = label;
                info.label_at = now;
                info.wake = true;
                info.wake_at = now;
                index
            }
            None => {
                self.cards.push(KnownCard {
                    info: CardInfo::new(uid.to_vec(), label, true, 0, now),
                    last_seen: 0,
                    atr: Vec::new(),
                });
                self.cards.len() - 1
            }
        };
        let card = &mut self.cards[index];
        card.last_seen = now;
        if !atr.is_empty() {
            card.atr = atr.to_vec();
        }
        Some(&card.info)
    }

    /// Merges a record from the phone. Returns the merged record when it
    /// differs from what the phone sent (send it back), and whether
    /// anything changed here.
    pub fn merge(&mut self, theirs: &CardInfo) -> (Option<CardInfo>, bool) {
        if !theirs.valid() || !uid_stable(&theirs.uid) {
            return (None, false);
        }
        match self
            .cards
            .iter_mut()
            .find(|card| card.info.uid == theirs.uid)
        {
            Some(card) => {
                let merged = card.info.merge(theirs);
                let changed = merged != card.info;
                card.info = merged.clone();
                ((merged != *theirs).then_some(merged), changed)
            }
            None => {
                self.cards.push(KnownCard {
                    info: theirs.clone(),
                    last_seen: 0,
                    atr: Vec::new(),
                });
                (None, true)
            }
        }
    }

    /// Applies a local edit to a known card; returns the new record when it
    /// changed.
    fn edit(&mut self, uid: &[u8], change: impl FnOnce(&mut CardInfo)) -> Option<&CardInfo> {
        let card = self.live_mut(uid)?;
        let before = card.info.clone();
        change(&mut card.info);
        (card.info != before).then_some(&card.info)
    }

    pub fn rename(&mut self, uid: &[u8], label: &str, now: u64) -> Option<&CardInfo> {
        let label = clean_label(label)?;
        self.edit(uid, |info| {
            if info.label != label {
                info.label = label;
                info.label_at = now;
            }
        })
    }

    pub fn set_wake(&mut self, uid: &[u8], wake: bool, now: u64) -> Option<&CardInfo> {
        self.edit(uid, |info| {
            if info.wake != wake {
                info.wake = wake;
                info.wake_at = now;
            }
        })
    }

    /// Merges what was seen on the card into its caps (the phone reads
    /// them; the PC adds what its own traffic proves).
    pub fn note_caps(&mut self, uid: &[u8], seen: u8, now: u64) -> Option<&CardInfo> {
        self.edit(uid, |info| {
            let mut caps = info.caps | seen;
            if seen & caps::CERTS != 0 {
                // Readable now: the applet was reinstalled without the block.
                caps &= !caps::BLOCKED;
            }
            if caps & (caps::CERTS | caps::BLOCKED) != 0 {
                caps |= caps::PIV;
            }
            if caps != info.caps {
                info.caps = caps;
                info.caps_at = now;
            }
        })
    }

    pub fn set_login(&mut self, uid: &[u8], paired: bool, now: u64) -> Option<&CardInfo> {
        self.edit(uid, |info| {
            if info.login != paired {
                info.login = paired;
                info.login_at = now;
            }
        })
    }

    /// Forgets a card; it stays forgotten on the phone too.
    pub fn forget(&mut self, uid: &[u8], now: u64) -> Option<&CardInfo> {
        let card = self.live_mut(uid)?;
        card.info.deleted_at = now.max(card.info.newest_edit() + 1);
        Some(&card.info)
    }

    /// Atomic replace of `cards.txt`.
    pub fn save(&self) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("no parent"))?;
        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(serialize(&self.cards).as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(|e| e.error)?;
        Ok(())
    }

    fn live_mut(&mut self, uid: &[u8]) -> Option<&mut KnownCard> {
        self.cards
            .iter_mut()
            .find(|card| card.info.uid == uid && !card.info.deleted())
    }

    /// The card's announced name, else the first free "Card N".
    pub fn default_label(&self, atr: &[u8]) -> String {
        if let Some(name) = atr_name(atr).and_then(|name| clean_label(&name)) {
            return name;
        }
        let taken = self.list();
        (1..)
            .map(|n| format!("Card {n}"))
            .find(|label| !taken.iter().any(|card| &card.info.label == label))
            .expect("unbounded range")
    }
}

/// Passive capability detection: watches the APDUs the PC itself sends
/// (macOS's PIV token, the security-key relay, the Windows reader) and
/// their answers. It never sends anything to the card. Returns [`caps`]
/// bits; only set bits carry information.
#[derive(Default)]
pub struct CapabilityProbe {
    /// (APDU id, what that request would prove) awaiting its response.
    pending: Option<(u64, Probe)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    SelectFido,
    SelectPiv,
    /// GET DATA for the PIV authentication certificate (5FC105, slot 9A).
    CertAuth,
}

const FIDO_AID: [u8; 8] = [0xA0, 0x00, 0x00, 0x06, 0x47, 0x2F, 0x00, 0x01];
/// PIV RID + application prefix (NIST SP 800-73-4 `A0 00 00 03 08`).
const PIV_AID: [u8; 5] = [0xA0, 0x00, 0x00, 0x03, 0x08];
/// GET DATA tag list for X.509 Certificate for PIV Authentication.
const CERT_9A: [u8; 5] = [0x5C, 0x03, 0x5F, 0xC1, 0x05];

impl CapabilityProbe {
    /// A new card (or none): forget the request in flight.
    pub fn reset(&mut self) {
        self.pending = None;
    }

    /// The PC sent APDU `id` to the card.
    pub fn request(&mut self, id: u64, apdu: &[u8]) {
        self.pending = classify(apdu).map(|probe| (id, probe));
    }

    /// The card answered APDU `id`; returns the [`caps`] that answer proves.
    pub fn response(&mut self, id: u64, rapdu: &[u8]) -> u8 {
        let Some((pending, probe)) = self.pending.take() else {
            return 0;
        };
        if pending != id {
            return 0;
        }
        let [.., sw1, sw2] = rapdu else {
            return 0;
        };
        let ok = (*sw1, *sw2) == (0x90, 0x00);
        let more = *sw1 == 0x61;
        let bit = |set: bool, bit: u8| if set { bit } else { 0 };
        match probe {
            Probe::SelectFido => bit(ok, caps::FIDO),
            Probe::SelectPiv => bit(ok || more, caps::PIV),
            // An empty 9000 is no certificate; 61xx means data follows.
            // 6982 on a plain object read: the applet locks its keys over
            // contactless (PivApplet's strict-contactless builds).
            Probe::CertAuth => {
                bit((ok && rapdu.len() > 2) || more, caps::CERTS)
                    | bit((*sw1, *sw2) == (0x69, 0x82), caps::BLOCKED)
            }
        }
    }

    /// The APDU failed (tag lost, I/O error): nothing to learn.
    pub fn failed(&mut self, id: u64) {
        if self.pending.is_some_and(|(pending, _)| pending == id) {
            self.pending = None;
        }
    }
}

/// Short or extended (`00 Lc1 Lc2`) command data.
fn classify(apdu: &[u8]) -> Option<Probe> {
    let [cla, ins, p1, p2, body @ ..] = apdu else {
        return None;
    };
    // Interindustry class only, no chaining; secure messaging bits ignored.
    if cla & 0xF0 != 0x00 {
        return None;
    }
    let data = match body {
        [0, high, low, rest @ ..] => rest.get(..usize::from(u16::from_be_bytes([*high, *low])))?,
        [lc, rest @ ..] if *lc != 0 => rest.get(..usize::from(*lc))?,
        _ => return None,
    };
    match (ins, p1, p2) {
        (0xA4, 0x04, _) if data.starts_with(&FIDO_AID) => Some(Probe::SelectFido),
        (0xA4, 0x04, _) if data.starts_with(&PIV_AID) => Some(Probe::SelectPiv),
        (0xCB, 0x3F, 0xFF) if data == CERT_9A => Some(Probe::CertAuth),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: [u8; 7] = [0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    /// 3B 8n 80 01 + historical bytes + TCK.
    fn atr_with(historical: &[u8]) -> Vec<u8> {
        let mut atr = vec![0x3B, 0x80 | historical.len() as u8, 0x80, 0x01];
        atr.extend_from_slice(historical);
        let check = atr[1..].iter().fold(0, |acc, byte| acc ^ byte);
        atr.push(check);
        atr
    }

    #[test]
    fn random_and_missing_uids_are_not_stable() {
        assert!(!uid_stable(&[]));
        assert!(!uid_stable(&[0x08, 1, 2, 3]));
        assert!(uid_stable(&[0x04, 1, 2, 3]));
        // Only 4-byte UIDs have the random marker.
        assert!(uid_stable(&[0x08, 1, 2, 3, 4, 5, 6]));
    }

    #[test]
    fn taps_only_update_cards_the_user_added() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        assert!(!cards.see(&UID, &atr_with(b"x"), 10));
        assert!(cards.list().is_empty());
        assert!(cards.add(&[0x08, 9, 9, 9], "Random", &[], 10).is_none());
        assert!(cards.add(&UID, " \n ", &[], 10).is_none());
        let added = cards
            .add(&UID, "  Hand\tring ", &atr_with(b"x"), 10)
            .unwrap();
        assert_eq!(added.label, "Hand ring");
        assert!(added.wake);
        assert!(cards.see(&UID, &atr_with(b"PivApplet"), 20));
        let card = cards.get(&UID).unwrap();
        assert_eq!(card.last_seen, 20);
        assert_eq!(card.atr, atr_with(b"PivApplet"));
    }

    #[test]
    fn default_labels_use_the_atr_name_or_the_next_number() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        assert_eq!(cards.default_label(&atr_with(b"\x80YubiKey")), "YubiKey");
        assert_eq!(cards.default_label(&[]), "Card 1");
        cards.add(&UID, "Card 1", &[], 1);
        assert_eq!(cards.default_label(&[]), "Card 2");
    }

    #[test]
    fn labels_are_cut_to_the_wire_limit_in_bytes() {
        let label = clean_label(&"\u{270b}".repeat(30)).unwrap(); // 3 bytes each
        assert!(label.len() <= MAX_LABEL);
        assert_eq!(label.chars().count(), 21);
        assert_eq!(clean_label(" \n\t "), None);
    }

    #[test]
    fn merging_the_phones_copy_reports_what_to_send_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.add(&UID, "PC name", &[], 10);
        cards.set_login(&UID, true, 20);
        // The phone renamed it later but doesn't know about the login.
        let phone = CardInfo {
            label: "Phone name".into(),
            label_at: 30,
            ..CardInfo::new(UID.to_vec(), "x".into(), true, caps::FIDO, 5)
        };
        let (reply, changed) = cards.merge(&phone);
        assert!(changed);
        let reply = reply.expect("the phone is missing the login");
        assert_eq!(reply.label, "Phone name");
        assert!(reply.login);
        // Merging the reply again settles: nothing to send, nothing new.
        assert_eq!(cards.merge(&reply), (None, false));
    }

    #[test]
    fn an_unknown_card_from_the_phone_is_added_and_invalid_ones_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        let phone = CardInfo::new(UID.to_vec(), "From phone".into(), true, caps::FIDO, 5);
        assert_eq!(cards.merge(&phone), (None, true));
        assert_eq!(cards.list()[0].info.label, "From phone");
        let bad = CardInfo {
            label: String::new(),
            ..phone
        };
        assert_eq!(cards.merge(&bad), (None, false));
        let random = CardInfo::new(vec![0x08, 1, 2, 3], "R".into(), true, 0, 5);
        assert_eq!(cards.merge(&random), (None, false));
    }

    #[test]
    fn forgotten_cards_stay_forgotten_until_added_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.add(&UID, "Ring", &[], 10);
        cards.note_caps(&UID, caps::FIDO, 11);
        let stale = cards.get(&UID).unwrap().info.clone();
        cards.forget(&UID, 20).unwrap();
        assert!(cards.list().is_empty());
        assert!(!cards.see(&UID, &[], 25));
        assert_eq!(cards.records().count(), 1, "kept as a tombstone");
        // The phone's older copy doesn't bring it back...
        let (reply, _) = cards.merge(&stale);
        assert!(reply.unwrap().deleted());
        assert!(cards.list().is_empty());
        // ...but adding it again does, keeping what was known.
        cards.add(&UID, "Ring again", &[], 30).unwrap();
        let card = &cards.list()[0].info;
        assert_eq!(card.label, "Ring again");
        assert!(card.has(caps::FIDO));
        // Even when the adding device's clock is behind the forget.
        cards.forget(&UID, 100).unwrap();
        cards.add(&UID, "Back", &[], 50).unwrap();
        assert_eq!(cards.list().len(), 1);
    }

    #[test]
    fn caps_merge_and_blocked_clears_once_certificates_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.add(&UID, "Ring", &[], 1);
        let info = cards.note_caps(&UID, caps::BLOCKED, 2).unwrap();
        assert!(info.has(caps::PIV | caps::BLOCKED));
        assert_eq!(info.caps_at, 2);
        let info = cards.note_caps(&UID, caps::CERTS, 3).unwrap();
        assert!(info.has(caps::PIV | caps::CERTS));
        assert!(!info.has(caps::BLOCKED));
        assert!(cards.note_caps(&UID, caps::CERTS, 4).is_none(), "no change");
    }

    #[test]
    fn local_edits_stamp_only_their_field() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.add(&UID, "Ring", &[], 1);
        let info = cards.rename(&UID, "Hand", 5).unwrap().clone();
        assert_eq!((info.label_at, info.wake_at), (5, 1));
        assert!(cards.rename(&UID, "Hand", 6).is_none(), "same name");
        let info = cards.set_wake(&UID, false, 7).unwrap();
        assert!(!info.wake);
        assert_eq!(info.wake_at, 7);
        assert!(cards.set_login(&UID, true, 8).unwrap().login);
        assert!(
            cards.rename(&[0x04, 9, 9, 9], "Nope", 9).is_none(),
            "unknown"
        );
    }

    #[test]
    fn the_list_survives_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.add(&UID, "hand \u{270b}", &atr_with(b"x"), 10);
        cards.note_caps(&UID, caps::FIDO | caps::CERTS, 11);
        cards.set_login(&UID, true, 12);
        cards.add(&[0x04, 7, 7, 7], "Gone", &[], 13);
        cards.forget(&[0x04, 7, 7, 7], 14);
        cards.save().unwrap();
        let reopened = KnownCards::open(dir.path());
        assert_eq!(reopened.records().count(), 2, "tombstones are kept");
        assert_eq!(reopened.list(), cards.list());
        assert!(reopened.list()[0].info.login);
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(
            text.starts_with("tapbridge-cards 2\n04112233445566\t10\t1\t10\t7\t11\t1\t12\t0\t")
        );
    }

    #[test]
    fn any_other_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        fs::write(&path, "tapbridge-cards 1\n04112233\t1\t2\t3B00\tf\tOld\n").unwrap();
        assert!(KnownCards::open(dir.path()).list().is_empty());
        assert!(dir.path().join("cards.txt.bad").exists());
        assert!(!path.exists());
    }

    #[test]
    fn malformed_lines_are_skipped_and_duplicates_keep_the_first() {
        let line =
            |uid: &str, label: &str| format!("{uid}\t1\t1\t1\t0\t1\t0\t0\t0\t1\t\t{label}\n");
        let text = format!(
            "{HEADER}\n{}{}{}{}{}{}",
            line("04112233445566", "First"),
            line("zz", "Bad hex"),
            line("08010203", "Random UID"),
            "04AABBCC\t1\t2\t1\t0\t1\t0\t0\t0\t1\t\tBad flag\n",
            line("04112233445566", "Second"),
            line("5A010203", "Good"),
        );
        let cards = parse(&text).unwrap();
        let labels: Vec<_> = cards.iter().map(|c| c.info.label.as_str()).collect();
        assert_eq!(labels, ["First", "Good"]);
        assert!(parse("").is_none());
        assert_eq!(parse(&format!("{HEADER}\n")).unwrap(), Vec::new());
    }

    #[test]
    fn atr_name_reads_printable_historical_bytes_only() {
        assert_eq!(
            atr_name(&atr_with(b"\x80\x01YubiKey")).as_deref(),
            Some("YubiKey")
        );
        assert_eq!(atr_name(&atr_with(&[0x80, 0x73, 0xC0, 0x21])), None);
        assert_eq!(atr_name(&atr_with(b"abc")), None);
        assert_eq!(atr_name(&[0x3B, 0x00]), None);
        assert_eq!(atr_name(&[0x3B, 0x8F, 0x80, 0x01, b'A']), None);
    }

    fn select(aid: &[u8]) -> Vec<u8> {
        let mut apdu = vec![0x00, 0xA4, 0x04, 0x00, aid.len() as u8];
        apdu.extend_from_slice(aid);
        apdu.push(0);
        apdu
    }

    const GET_CERT_9A: [u8; 11] = [0x00, 0xCB, 0x3F, 0xFF, 5, 0x5C, 3, 0x5F, 0xC1, 0x05, 0];

    #[test]
    fn probe_reads_fido_piv_certs_and_blocked() {
        let mut probe = CapabilityProbe::default();
        probe.request(1, &select(&FIDO_AID));
        assert_eq!(probe.response(1, b"U2F_V2\x90\x00"), caps::FIDO);
        probe.request(2, &select(&FIDO_AID));
        assert_eq!(probe.response(2, &[0x6A, 0x82]), 0);
        // macOS's pivtoken selects the short PIV AID and gets 61xx.
        probe.request(3, &select(&PIV_AID));
        assert_eq!(probe.response(3, &[0x61, 0x11]), caps::PIV);
        probe.request(4, &GET_CERT_9A);
        assert_eq!(
            probe.response(4, &[0x53, 0x01, 0x00, 0x90, 0x00]),
            caps::CERTS
        );
        probe.request(5, &GET_CERT_9A);
        assert_eq!(probe.response(5, &[0x61, 0x00]), caps::CERTS);
        probe.request(6, &GET_CERT_9A);
        assert_eq!(probe.response(6, &[0x69, 0x82]), caps::BLOCKED);
        // Empty slot: not found, or an empty success.
        probe.request(7, &GET_CERT_9A);
        assert_eq!(probe.response(7, &[0x6A, 0x82]), 0);
        probe.request(8, &GET_CERT_9A);
        assert_eq!(probe.response(8, &[0x90, 0x00]), 0);
    }

    #[test]
    fn probe_reads_extended_length_commands() {
        let mut probe = CapabilityProbe::default();
        let extended = [
            0x00, 0xCB, 0x3F, 0xFF, 0x00, 0x00, 0x05, 0x5C, 0x03, 0x5F, 0xC1, 0x05, 0x00, 0x00,
        ];
        probe.request(1, &extended);
        assert_eq!(probe.response(1, &[0x53, 0x00, 0x90, 0x00]), caps::CERTS);
        let mut select = vec![0x00, 0xA4, 0x04, 0x00, 0x00, 0x00, 0x05];
        select.extend_from_slice(&PIV_AID);
        probe.request(2, &select);
        assert_eq!(probe.response(2, &[0x90, 0x00]), caps::PIV);
        probe.request(3, &[0x00, 0xA4, 0x04, 0x00, 0x00, 0x00]);
        assert_eq!(probe.response(3, &[0x90, 0x00]), 0);
    }

    #[test]
    fn probe_ignores_other_commands_mismatched_ids_and_failures() {
        let mut probe = CapabilityProbe::default();
        probe.request(
            1,
            &[0x00, 0xCB, 0x3F, 0xFF, 5, 0x5C, 3, 0x5F, 0xC1, 0x0A, 0],
        );
        assert_eq!(probe.response(1, &[0x90, 0x00, 0x90, 0x00]), 0);
        probe.request(2, &select(&[0xA0, 0, 0, 0, 0x03]));
        assert_eq!(probe.response(2, &[0x90, 0x00]), 0);
        probe.request(3, &select(&PIV_AID));
        assert_eq!(probe.response(4, &[0x90, 0x00]), 0);
        probe.request(5, &select(&PIV_AID));
        probe.failed(5);
        assert_eq!(probe.response(5, &[0x90, 0x00]), 0);
        probe.request(6, &select(&PIV_AID));
        probe.reset();
        assert_eq!(probe.response(6, &[0x90, 0x00]), 0);
        probe.request(7, &[0x80, 0x10, 0, 0, 1, 4]);
        assert_eq!(probe.response(7, &[0x00, 0x90, 0x00]), 0);
        probe.request(8, &[0x00, 0xA4, 0x04, 0x00, 9, 0xA0]);
        assert_eq!(probe.response(8, &[0x90]), 0);
    }
}
