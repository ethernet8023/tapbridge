//! Cards seen through the phone, keyed by NFC UID, plus the passive
//! capability probe that fills in what each card can do.
//!
//! Plain text file `cards.txt` in the state directory (nothing here is
//! secret). Format, one card per line after a header line:
//!
//! ```text
//! tapbridge-cards 1
//! UID_HEX \t FIRST_SEEN \t LAST_SEEN \t ATR_HEX \t FLAGS \t LABEL
//! ```
//!
//! Times are Unix seconds. FLAGS is `-` or any of `f` (FIDO applet), `p`
//! (PIV applet), `c` (PIV authentication certificate in slot 9A). The label
//! is last so it may hold anything but tabs and newlines.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const HEADER: &str = "tapbridge-cards 1";
const FILE: &str = "cards.txt";
/// Longest label kept, in characters.
pub const MAX_LABEL: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnownCard {
    pub uid: Vec<u8>,
    pub label: String,
    pub first_seen: u64,
    pub last_seen: u64,
    pub atr: Vec<u8>,
    pub has_fido: bool,
    pub has_piv: bool,
    /// A certificate is present in PIV slot 9A (PIV authentication).
    pub piv_certs: bool,
}

/// What the probe saw a card answer successfully. Only `true` flags carry
/// information: they are merged in, never cleared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub has_fido: bool,
    pub has_piv: bool,
    pub piv_certs: bool,
}

impl Capabilities {
    pub fn any(&self) -> bool {
        self.has_fido || self.has_piv || self.piv_certs
    }
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
/// characters. `None` when nothing is left.
pub fn clean_label(label: &str) -> Option<String> {
    let line: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed: String = line.trim().chars().take(MAX_LABEL).collect();
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

/// Parses the file text; `None` when it isn't a cards file at all.
/// Malformed card lines are skipped, and a UID seen twice keeps its first
/// line.
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
        if !cards.iter().any(|known| known.uid == card.uid) {
            cards.push(card);
        }
    }
    Some(cards)
}

fn parse_line(line: &str) -> Option<KnownCard> {
    let mut fields = line.splitn(6, '\t');
    let uid = unhex(fields.next()?)?;
    let first_seen = fields.next()?.parse().ok()?;
    let last_seen = fields.next()?.parse().ok()?;
    let atr = unhex(fields.next()?)?;
    let flags = fields.next()?;
    let label = clean_label(fields.next()?)?;
    if !uid_stable(&uid) || uid.len() > tapbridge_core::wire::MAX_UID {
        return None;
    }
    if flags != "-" && !flags.chars().all(|c| matches!(c, 'f' | 'p' | 'c')) {
        return None;
    }
    Some(KnownCard {
        uid,
        label,
        first_seen,
        last_seen,
        atr,
        has_fido: flags.contains('f'),
        has_piv: flags.contains('p'),
        piv_certs: flags.contains('c'),
    })
}

pub fn serialize(cards: &[KnownCard]) -> String {
    let mut text = format!("{HEADER}\n");
    for card in cards {
        let mut flags = String::new();
        for (set, flag) in [
            (card.has_fido, 'f'),
            (card.has_piv, 'p'),
            (card.piv_certs, 'c'),
        ] {
            if set {
                flags.push(flag);
            }
        }
        if flags.is_empty() {
            flags.push('-');
        }
        let label = clean_label(&card.label).unwrap_or_else(|| "Card".into());
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{flags}\t{label}\n",
            hex(&card.uid),
            card.first_seen,
            card.last_seen,
            hex(&card.atr),
        ));
    }
    text
}

/// The persistent list. Every change is saved right away.
pub struct KnownCards {
    cards: Vec<KnownCard>,
    path: PathBuf,
}

impl KnownCards {
    /// Loads `cards.txt` from `directory`. Never fails: a missing file is an
    /// empty list, and an unreadable one is moved aside to `cards.txt.bad`
    /// (kept for inspection, not silently overwritten).
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

    pub fn list(&self) -> &[KnownCard] {
        &self.cards
    }

    pub fn get(&self, uid: &[u8]) -> Option<&KnownCard> {
        self.cards.iter().find(|card| card.uid == uid)
    }

    /// Records a tap at `now`. Returns whether the list changed (always,
    /// for a stable UID: the last-seen time moves); cards without a stable
    /// UID are ignored.
    pub fn see(&mut self, uid: &[u8], atr: &[u8], now: u64) -> bool {
        if !uid_stable(uid) || uid.len() > tapbridge_core::wire::MAX_UID {
            return false;
        }
        if let Some(card) = self.cards.iter_mut().find(|card| card.uid == uid) {
            card.last_seen = now;
            if !atr.is_empty() {
                card.atr = atr.to_vec();
            }
        } else {
            let label = self.default_label(atr);
            self.cards.push(KnownCard {
                uid: uid.to_vec(),
                label,
                first_seen: now,
                last_seen: now,
                atr: atr.to_vec(),
                ..KnownCard::default()
            });
        }
        true
    }

    /// Merges newly observed capabilities. Returns whether anything changed.
    pub fn note(&mut self, uid: &[u8], seen: Capabilities) -> bool {
        let Some(card) = self.cards.iter_mut().find(|card| card.uid == uid) else {
            return false;
        };
        let before = (card.has_fido, card.has_piv, card.piv_certs);
        card.has_fido |= seen.has_fido;
        // A certificate read means the PIV applet answered too.
        card.has_piv |= seen.has_piv || seen.piv_certs;
        card.piv_certs |= seen.piv_certs;
        before != (card.has_fido, card.has_piv, card.piv_certs)
    }

    pub fn rename(&mut self, uid: &[u8], label: &str) -> bool {
        let Some(label) = clean_label(label) else {
            return false;
        };
        match self.cards.iter_mut().find(|card| card.uid == uid) {
            Some(card) if card.label != label => {
                card.label = label;
                true
            }
            _ => false,
        }
    }

    pub fn forget(&mut self, uid: &[u8]) -> bool {
        let before = self.cards.len();
        self.cards.retain(|card| card.uid != uid);
        self.cards.len() != before
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

    /// The card's announced name, else the first free "Card N".
    fn default_label(&self, atr: &[u8]) -> String {
        if let Some(name) = atr_name(atr).and_then(|name| clean_label(&name)) {
            return name;
        }
        (1..)
            .map(|n| format!("Card {n}"))
            .find(|label| !self.cards.iter().any(|card| &card.label == label))
            .expect("unbounded range")
    }
}

/// Passive capability detection: watches the APDUs the PC itself sends
/// (macOS's PIV token, the security-key relay, the Windows reader) and
/// their answers. It never sends anything to the card.
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

    /// The card answered APDU `id`; returns what that answer proves.
    pub fn response(&mut self, id: u64, rapdu: &[u8]) -> Capabilities {
        let mut seen = Capabilities::default();
        let Some((pending, probe)) = self.pending.take() else {
            return seen;
        };
        if pending != id {
            return seen;
        }
        let [.., sw1, sw2] = rapdu else {
            return seen;
        };
        let ok = (*sw1, *sw2) == (0x90, 0x00);
        let more = *sw1 == 0x61;
        match probe {
            Probe::SelectFido => seen.has_fido = ok,
            Probe::SelectPiv => seen.has_piv = ok || more,
            // An empty 9000 is no certificate; 61xx means data follows.
            Probe::CertAuth => seen.piv_certs = (ok && rapdu.len() > 2) || more,
        }
        seen
    }

    /// The APDU failed (tag lost, I/O error): nothing to learn.
    pub fn failed(&mut self, id: u64) {
        if self.pending.is_some_and(|(pending, _)| pending == id) {
            self.pending = None;
        }
    }
}

/// Short-form APDUs only: every command the probe cares about is short.
fn classify(apdu: &[u8]) -> Option<Probe> {
    let [cla, ins, p1, p2, lc, rest @ ..] = apdu else {
        return None;
    };
    // Interindustry class only, no chaining; secure messaging bits ignored.
    if cla & 0xF0 != 0x00 {
        return None;
    }
    let data = rest.get(..*lc as usize)?;
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
    const OTHER: [u8; 4] = [0x5A, 0x01, 0x02, 0x03];
    /// 3B 8C 80 01 + "PivApplet\0\0\0"-style historical bytes + TCK.
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
    fn uid_less_cards_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        assert!(!cards.see(&[], &atr_with(&[]), 10));
        assert!(!cards.see(&[0x08, 9, 9, 9], &atr_with(&[]), 10));
        assert!(cards.list().is_empty());
        assert!(!cards.note(
            &[],
            Capabilities {
                has_fido: true,
                ..Capabilities::default()
            }
        ));
    }

    #[test]
    fn new_cards_get_numbered_or_announced_labels() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.see(&UID, &atr_with(&[0x80, 0x73]), 1);
        cards.see(&OTHER, &atr_with(&[]), 2);
        cards.see(&[0x04, 9, 9, 9, 9, 9, 9], &atr_with(b"\x80YubiKey"), 3);
        let labels: Vec<_> = cards.list().iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Card 1", "Card 2", "YubiKey"]);
        // A freed number is reused, never a duplicate.
        cards.forget(&UID);
        cards.see(&[0x04, 7, 7, 7, 7, 7, 7], &[], 4);
        assert_eq!(cards.list().last().unwrap().label, "Card 1");
    }

    #[test]
    fn re_seeing_a_card_merges_instead_of_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.see(&UID, &atr_with(&[1]), 100);
        cards.rename(&UID, "Implant");
        cards.note(
            &UID,
            Capabilities {
                has_fido: true,
                ..Capabilities::default()
            },
        );
        assert!(cards.see(&UID, &atr_with(&[2]), 250));
        assert!(cards.note(
            &UID,
            Capabilities {
                piv_certs: true,
                ..Capabilities::default()
            }
        ));
        // Nothing new: no change reported.
        assert!(!cards.note(
            &UID,
            Capabilities {
                has_fido: true,
                ..Capabilities::default()
            }
        ));
        assert_eq!(cards.list().len(), 1);
        let card = &cards.list()[0];
        assert_eq!(card.label, "Implant");
        assert_eq!((card.first_seen, card.last_seen), (100, 250));
        assert_eq!(card.atr, atr_with(&[2]));
        assert!(card.has_fido && card.has_piv && card.piv_certs);
    }

    #[test]
    fn list_survives_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.see(&UID, &atr_with(b"PivApplet"), 1_700_000_000);
        cards.see(&OTHER, &[], 1_700_000_100);
        cards.note(
            &UID,
            Capabilities {
                has_fido: true,
                has_piv: true,
                piv_certs: false,
            },
        );
        cards.rename(&OTHER, "  Office\tbadge\n ");
        cards.save().unwrap();
        let reopened = KnownCards::open(dir.path());
        assert_eq!(reopened.list(), cards.list());
        assert_eq!(reopened.get(&OTHER).unwrap().label, "Office badge");
        let text = fs::read_to_string(dir.path().join("cards.txt")).unwrap();
        assert!(text.starts_with("tapbridge-cards 1\n04112233445566\t1700000000\t"));
        assert!(text.contains("\tfp\tPivApplet\n"));
        assert!(text.contains("\t-\tOffice badge\n"));
    }

    #[test]
    fn forget_and_rename_report_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut cards = KnownCards::open(dir.path());
        cards.see(&UID, &[], 1);
        assert!(!cards.rename(&UID, "   "));
        assert!(!cards.rename(&OTHER, "Nope"));
        assert!(cards.rename(&UID, &"x".repeat(100)));
        assert_eq!(cards.get(&UID).unwrap().label.len(), MAX_LABEL);
        assert!(!cards.forget(&OTHER));
        assert!(cards.forget(&UID));
        assert!(cards.list().is_empty());
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cards.txt");
        fs::write(&path, b"\xff\xfe not a cards file").unwrap();
        let mut cards = KnownCards::open(dir.path());
        assert!(cards.list().is_empty());
        assert_eq!(
            fs::read(dir.path().join("cards.txt.bad")).unwrap(),
            b"\xff\xfe not a cards file"
        );
        cards.see(&UID, &[], 5);
        cards.save().unwrap();
        assert_eq!(KnownCards::open(dir.path()).list().len(), 1);
    }

    #[test]
    fn malformed_lines_are_skipped_and_duplicates_keep_the_first() {
        let text = "tapbridge-cards 1\n\
            04112233445566\t1\t2\t3B00\tfc\tFirst\n\
            zz\t1\t2\t\t-\tBad hex\n\
            5A010203\tx\t2\t\t-\tBad time\n\
            5A010203\t1\t2\t\tq\tBad flag\n\
            08010203\t1\t2\t\t-\tRandom UID\n\
            5A010203\t1\t2\t\t-\t\n\
            5A010203\t1\t2\n\
            04112233445566\t9\t9\t\t-\tSecond\n\
            5A010203\t3\t4\t\t-\tGood\n";
        let cards = parse(text).unwrap();
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].label, "First");
        assert!(cards[0].has_fido && !cards[0].has_piv && cards[0].piv_certs);
        assert_eq!(cards[1].label, "Good");
        assert!(parse("").is_none());
        assert!(parse("tapbridge-cards 2\n").is_none());
        assert_eq!(parse("tapbridge-cards 1\n").unwrap(), Vec::new());
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
        // Historical length past the end of the ATR.
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
    fn probe_sees_fido_select_success() {
        let mut probe = CapabilityProbe::default();
        probe.request(1, &select(&FIDO_AID));
        let mut answer = b"U2F_V2".to_vec();
        answer.extend([0x90, 0x00]);
        assert_eq!(
            probe.response(1, &answer),
            Capabilities {
                has_fido: true,
                ..Capabilities::default()
            }
        );
        // The same select failing proves nothing.
        probe.request(2, &select(&FIDO_AID));
        assert!(!probe.response(2, &[0x6A, 0x82]).any());
    }

    #[test]
    fn probe_sees_piv_select_and_certificate_from_macos_traffic() {
        let mut probe = CapabilityProbe::default();
        // macOS's pivtoken selects the short PIV AID and gets 61xx.
        probe.request(10, &select(&PIV_AID));
        assert!(probe.response(10, &[0x61, 0x11]).has_piv);
        probe.request(11, &GET_CERT_9A);
        let seen = probe.response(11, &[0x53, 0x82, 0x01, 0x00, 0x90, 0x00]);
        assert!(seen.piv_certs && !seen.has_fido);
        probe.request(12, &GET_CERT_9A);
        assert!(probe.response(12, &[0x61, 0x00]).piv_certs);
        // Empty slot: not found, or an empty success.
        probe.request(13, &GET_CERT_9A);
        assert!(!probe.response(13, &[0x6A, 0x82]).any());
        probe.request(14, &GET_CERT_9A);
        assert!(!probe.response(14, &[0x90, 0x00]).any());
    }

    #[test]
    fn probe_ignores_other_commands_mismatched_ids_and_failures() {
        let mut probe = CapabilityProbe::default();
        // Other slots and other applets.
        probe.request(
            1,
            &[0x00, 0xCB, 0x3F, 0xFF, 5, 0x5C, 3, 0x5F, 0xC1, 0x0A, 0],
        );
        assert!(!probe.response(1, &[0x90, 0x00, 0x90, 0x00]).any());
        probe.request(2, &select(&[0xA0, 0, 0, 0, 0x03]));
        assert!(!probe.response(2, &[0x90, 0x00]).any());
        // A response to another id doesn't count.
        probe.request(3, &select(&PIV_AID));
        assert!(!probe.response(4, &[0x90, 0x00]).any());
        // A failed APDU clears it.
        probe.request(5, &select(&PIV_AID));
        probe.failed(5);
        assert!(!probe.response(5, &[0x90, 0x00]).any());
        // A new card clears it.
        probe.request(6, &select(&PIV_AID));
        probe.reset();
        assert!(!probe.response(6, &[0x90, 0x00]).any());
        // CTAP traffic, truncated APDUs and short replies never panic.
        probe.request(7, &[0x80, 0x10, 0, 0, 1, 4]);
        assert!(!probe.response(7, &[0x00, 0x90, 0x00]).any());
        probe.request(8, &[0x00, 0xA4, 0x04, 0x00, 9, 0xA0]);
        assert!(!probe.response(8, &[0x90]).any());
    }
}
