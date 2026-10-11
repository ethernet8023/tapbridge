//! The card record phone and PC keep in sync, and the rule that merges two
//! copies of it.
//!
//! Cards are keyed by their stable NFC UID. Every synced field carries the
//! Unix time (milliseconds) of its last change, and merging takes the newer
//! value field by field, so edits made on either side converge no matter the
//! order they arrive in. Equal timestamps fall back to comparing the values,
//! which keeps the merge commutative.
//!
//! Forgetting a card stamps `deleted_at`. A card is gone while that stamp is
//! newer than every field's stamp; naming it again (a newer `label_at`)
//! brings it back.

/// Bits of [`CardInfo::caps`]: what the card was last seen to support.
pub mod caps {
    /// A FIDO (passkey) applet answered SELECT.
    pub const FIDO: u8 = 1;
    /// A PIV (smart-card) applet answered SELECT.
    pub const PIV: u8 = 2;
    /// PIV slot 9A holds a certificate.
    pub const CERTS: u8 = 4;
    /// The PIV applet refuses its keys over NFC (strict-contactless build).
    pub const BLOCKED: u8 = 8;
    pub const ALL: u8 = FIDO | PIV | CERTS | BLOCKED;
}

/// Longest label, in bytes of UTF-8.
pub const MAX_LABEL: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardInfo {
    /// Stable NFC UID, 1..=[`crate::wire::MAX_UID`] bytes.
    pub uid: Vec<u8>,
    pub label: String,
    pub label_at: u64,
    /// The phone opens tapbridge when this card is tapped, even with the
    /// app closed.
    pub wake: bool,
    pub wake_at: u64,
    /// [`caps`] bits.
    pub caps: u8,
    pub caps_at: u64,
    /// The card's PIV identity is paired with the PC's macOS login.
    pub mac_login: bool,
    pub mac_login_at: u64,
    /// When the card was forgotten (0: never).
    pub deleted_at: u64,
}

impl CardInfo {
    /// A card just added (named) at `now`.
    pub fn new(uid: Vec<u8>, label: String, wake: bool, caps: u8, now: u64) -> Self {
        Self {
            uid,
            label,
            label_at: now,
            wake,
            wake_at: now,
            caps,
            caps_at: now,
            mac_login: false,
            mac_login_at: 0,
            deleted_at: 0,
        }
    }

    pub fn has(&self, bits: u8) -> bool {
        self.caps & bits == bits
    }

    /// Forgotten, and not named again since.
    pub fn deleted(&self) -> bool {
        self.deleted_at > self.newest_edit()
    }

    /// The latest of the field stamps (not `deleted_at`).
    pub fn newest_edit(&self) -> u64 {
        self.label_at
            .max(self.wake_at)
            .max(self.caps_at)
            .max(self.mac_login_at)
    }

    /// Both copies must describe the same card (same UID).
    pub fn merge(&self, other: &Self) -> Self {
        debug_assert_eq!(self.uid, other.uid);
        let (label, label_at) = newer((&self.label, self.label_at), (&other.label, other.label_at));
        let (wake, wake_at) = newer((&self.wake, self.wake_at), (&other.wake, other.wake_at));
        let (caps, caps_at) = newer((&self.caps, self.caps_at), (&other.caps, other.caps_at));
        let (mac_login, mac_login_at) = newer(
            (&self.mac_login, self.mac_login_at),
            (&other.mac_login, other.mac_login_at),
        );
        Self {
            uid: self.uid.clone(),
            label: label.clone(),
            label_at,
            wake: *wake,
            wake_at,
            caps: *caps,
            caps_at,
            mac_login: *mac_login,
            mac_login_at,
            deleted_at: self.deleted_at.max(other.deleted_at),
        }
    }

    /// Fits the wire: UID length, label length, known caps bits.
    pub fn valid(&self) -> bool {
        (1..=crate::wire::MAX_UID).contains(&self.uid.len())
            && !self.label.is_empty()
            && self.label.len() <= MAX_LABEL
            && self.caps & !caps::ALL == 0
    }
}

/// The value with the newer stamp; on a tie, the larger value.
fn newer<'a, T: Ord>(a: (&'a T, u64), b: (&'a T, u64)) -> (&'a T, u64) {
    if (a.1, a.0) >= (b.1, b.0) { a } else { b }
}

/// Unix time in milliseconds.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(label: &str, at: u64) -> CardInfo {
        CardInfo::new(vec![4, 1, 2, 3], label.into(), true, caps::FIDO, at)
    }

    #[test]
    fn each_field_takes_the_newer_edit() {
        let mut phone = card("Ring", 10);
        let mut pc = card("Ring", 10);
        phone.label = "Hand".into();
        phone.label_at = 20;
        pc.mac_login = true;
        pc.mac_login_at = 30;
        let merged = phone.merge(&pc);
        assert_eq!(merged.label, "Hand");
        assert!(merged.mac_login);
        assert_eq!(merged, pc.merge(&phone), "order doesn't matter");
        assert_eq!(merged.merge(&merged), merged, "idempotent");
    }

    #[test]
    fn ties_resolve_the_same_way_on_both_sides() {
        let a = card("Alpha", 10);
        let b = card("Beta", 10);
        assert_eq!(a.merge(&b).label, b.merge(&a).label);
    }

    #[test]
    fn a_skewed_clock_only_shifts_which_edit_wins() {
        // The PC's clock runs an hour ahead: its older edit still wins, but
        // both sides agree on it.
        let phone = CardInfo {
            label: "Phone name".into(),
            label_at: 1_000,
            ..card("x", 1)
        };
        let pc = CardInfo {
            label: "PC name".into(),
            label_at: 3_600_000,
            ..card("x", 1)
        };
        assert_eq!(phone.merge(&pc), pc.merge(&phone));
        assert_eq!(phone.merge(&pc).label, "PC name");
    }

    #[test]
    fn forgetting_sticks_until_the_card_is_named_again() {
        let live = card("Ring", 10);
        let forgotten = CardInfo {
            deleted_at: 20,
            ..live.clone()
        };
        let merged = live.merge(&forgotten);
        assert!(
            merged.deleted(),
            "the other side's older copy can't revive it"
        );
        let renamed = CardInfo {
            label: "Ring again".into(),
            label_at: 30,
            ..merged.clone()
        };
        assert!(!renamed.merge(&merged).deleted());
    }

    #[test]
    fn validity_checks_uid_label_and_caps() {
        assert!(card("Ring", 1).valid());
        assert!(
            !CardInfo {
                uid: vec![],
                ..card("Ring", 1)
            }
            .valid()
        );
        assert!(
            !CardInfo {
                uid: vec![1; 11],
                ..card("Ring", 1)
            }
            .valid()
        );
        assert!(!card("", 1).valid());
        assert!(!card(&"x".repeat(65), 1).valid());
        assert!(
            !CardInfo {
                caps: 16,
                ..card("Ring", 1)
            }
            .valid()
        );
    }
}
