//! Pure state for the Cards section: the one card action in progress
//! ("Set up as smart card", "Use for Mac login") and the text Preferences
//! shows for it. The shell feeds it time, connection state and service
//! events, and carries out the [`Effect`]s it returns.

use std::time::{Duration, Instant};

/// How long to wait for the phone to connect after a wake.
pub const CONNECT_WAIT: Duration = Duration::from_secs(90);
/// How long to wait for the card to be put on the phone.
pub const CARD_WAIT: Duration = Duration::from_secs(90);

/// What the shell must do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Send `SetupPiv` to the connected phone.
    SendSetup,
    /// Ask for the phone (wake it, or ReadCard on a live session).
    RequestTap,
    /// Run the Mac login pairing now: the card is on the phone.
    RunPairing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// The phone was woken; `SetupPiv` goes out once it connects.
    Connecting {
        since: Instant,
    },
    /// The phone shows its smart-card setup.
    OnPhone,
    /// The phone finished its setup.
    SetupDone {
        ok: bool,
    },
    /// Waiting for the card on the phone before pairing (macOS login).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    WaitingCard {
        since: Instant,
    },
    /// `sc_auth` is running (macOS may be showing its password dialog).
    Pairing,
    Finished {
        ok: bool,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardTask {
    pub uid: Vec<u8>,
    pub step: Step,
}

impl CardTask {
    /// "Set up as smart card": try the live session first.
    pub fn setup(uid: Vec<u8>) -> (Self, Effect) {
        (
            Self {
                uid,
                step: Step::OnPhone,
            },
            Effect::SendSetup,
        )
    }

    /// The result of [`Effect::SendSetup`]. Not sent: no live session, so
    /// wake the phone and send once it connects.
    pub fn setup_sent(&mut self, sent: bool, now: Instant) -> Option<Effect> {
        if sent {
            self.step = Step::OnPhone;
            None
        } else if matches!(self.step, Step::Connecting { .. }) {
            // Connected but the frame didn't make it: give up cleanly.
            self.finish(false, "The phone disconnected. Try again.");
            None
        } else {
            self.step = Step::Connecting { since: now };
            Some(Effect::RequestTap)
        }
    }

    /// A wake or tap request couldn't go out at all.
    pub fn unreachable(&mut self) {
        if matches!(
            self.step,
            Step::Connecting { .. } | Step::WaitingCard { .. }
        ) {
            self.finish(false, "Open tapbridge on your phone, then try again.");
        }
    }

    /// The phone reported the setup outcome. Returns whether this task
    /// was waiting for it.
    pub fn setup_done(&mut self, ok: bool) -> bool {
        let waiting = matches!(self.step, Step::OnPhone | Step::Connecting { .. });
        if waiting {
            self.step = Step::SetupDone { ok };
        }
        waiting
    }

    /// "Use for Mac login": runs at once when the card is on the phone.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn mac_login(uid: Vec<u8>, present: bool, now: Instant) -> (Self, Effect) {
        let (step, effect) = if present {
            (Step::Pairing, Effect::RunPairing)
        } else {
            (Step::WaitingCard { since: now }, Effect::RequestTap)
        };
        (Self { uid, step }, effect)
    }

    /// The pairing finished.
    pub fn paired(&mut self, ok: bool, text: String) {
        if self.step == Step::Pairing {
            self.step = Step::Finished { ok, text };
        }
    }

    /// Called every tick. `present_uid` is the stable UID of the card on
    /// the phone (empty when none).
    pub fn tick(&mut self, now: Instant, connected: bool, present_uid: &[u8]) -> Option<Effect> {
        match self.step {
            Step::Connecting { .. } if connected => Some(Effect::SendSetup),
            Step::Connecting { since } if now.duration_since(since) >= CONNECT_WAIT => {
                self.finish(false, "Open tapbridge on your phone, then try again.");
                None
            }
            Step::WaitingCard { .. } if present_uid == self.uid.as_slice() => {
                self.step = Step::Pairing;
                Some(Effect::RunPairing)
            }
            Step::WaitingCard { since } if now.duration_since(since) >= CARD_WAIT => {
                self.finish(false, "No card was tapped. Try again.");
                None
            }
            _ => None,
        }
    }

    /// Something is under way: no other card action may start.
    pub fn busy(&self) -> bool {
        !matches!(self.step, Step::SetupDone { .. } | Step::Finished { .. })
    }

    /// Whether this step can be dismissed (nothing is running behind it).
    pub fn dismissable(&self) -> bool {
        !matches!(self.step, Step::Pairing)
    }

    /// The status line under the card.
    pub fn text(&self) -> String {
        match &self.step {
            Step::Connecting { .. } => "Waking your phone\u{2026}".into(),
            Step::OnPhone => "Finish the setup on your phone.".into(),
            Step::SetupDone { ok: true } => "Smart card set up.".into(),
            Step::SetupDone { ok: false } => "Setup didn't finish on the phone.".into(),
            Step::WaitingCard { .. } => "Hold the card to your phone.".into(),
            Step::Pairing => "Pairing with your Mac login\u{2026}".into(),
            Step::Finished { text, .. } => text.clone(),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(
            self.step,
            Step::SetupDone { ok: false } | Step::Finished { ok: false, .. }
        )
    }

    fn finish(&mut self, ok: bool, text: &str) {
        self.step = Step::Finished {
            ok,
            text: text.into(),
        };
    }
}

/// One step of the "macOS login" checklist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    /// Can be done now.
    Todo,
    /// Something is wrong and needs the user (e.g. an applet that blocks NFC).
    Problem,
    /// Waits for an earlier step.
    Locked,
}

/// The three steps toward signing in to the Mac with a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checklist {
    pub applet: StepState,
    pub certificates: StepState,
    pub mac_login: StepState,
}

impl Checklist {
    /// From what is known about the card. Each step needs the one before.
    pub fn new(card: &tapbridge_host::cards::KnownCard) -> Self {
        let applet = if card.piv_blocked {
            StepState::Problem
        } else if card.has_piv || card.piv_certs {
            StepState::Done
        } else {
            StepState::Todo
        };
        let certificates = match applet {
            StepState::Done if card.piv_certs => StepState::Done,
            StepState::Done => StepState::Todo,
            _ => StepState::Locked,
        };
        let mac_login = match certificates {
            StepState::Done if card.mac_login => StepState::Done,
            StepState::Done => StepState::Todo,
            _ => StepState::Locked,
        };
        Self {
            applet,
            certificates,
            mac_login,
        }
    }

    pub fn ready(&self) -> bool {
        self.mac_login == StepState::Done
    }
}

/// Dangerous Things flexSecure: its ATR historical bytes (the phone builds
/// `3B 8n 80 01 <historical> TCK`). Matches the ATR the forum and our own
/// implant report: 3B8D80010031C173C8400052A51000900070.
const FLEXSECURE_HISTORICAL: [u8; 13] = [
    0x00, 0x31, 0xC1, 0x73, 0xC8, 0x40, 0x00, 0x52, 0xA5, 0x10, 0x00, 0x90, 0x00,
];

pub fn is_flexsecure(atr: &[u8]) -> bool {
    match atr {
        [0x3B, t0, 0x80, 0x01, rest @ ..] if t0 & 0xF0 == 0x80 => rest
            .get(..usize::from(t0 & 0x0F))
            .is_some_and(|historical| historical == FLEXSECURE_HISTORICAL),
        _ => false,
    }
}

pub const GP_MOBILE_URL: &str =
    "https://play.google.com/store/apps/details?id=com.dangerousthings.gpmobile";
pub const PIVAPPLET_RELEASE_URL: &str =
    "https://github.com/arekinath/PivApplet/releases/tag/v0.9.0";
/// The build that works over NFC on a flexSecure: P-256/P-384 ECDSA, RSA,
/// AES/3DES admin keys, extended APDUs, and no strict-contactless lock.
pub const FLEXSECURE_CAP: &str = "PivApplet-0.9.0-jc304-REePAxaD.cap";

/// "How to install" for step 1, one paragraph per entry. `blocked`: an
/// applet is there but locks its keys over NFC.
pub fn install_steps(atr: &[u8], blocked: bool) -> Vec<String> {
    let mut steps = Vec::new();
    if blocked {
        steps.push(
            "The smart-card applet on this card refuses its keys over NFC: it's a \
             strict-contactless build (an S in its name, like REePSAaD). Replace it \
             with a build without the S."
                .to_owned(),
        );
    }
    if is_flexsecure(atr) {
        steps.push(format!(
            "This looks like a flexSecure. Install {FLEXSECURE_CAP} from the PivApplet \
             v0.9.0 release with the GlobalPlatform Mobile app on your phone. Don't use \
             builds with S in the name: they block NFC."
        ));
    } else {
        steps.push(
            "Install PivApplet from its releases page with the GlobalPlatform Mobile \
             app on your phone, if your card can take applets. Pick a jc304 build \
             without S in its name: those block NFC."
                .to_owned(),
        );
    }
    steps.push(
        "With gp.jar instead: java -jar gp.jar --list, then --delete <old PivApplet \
         package AID from the list> if one is there, then --install <the .cap file>. \
         Add --key if your card's GlobalPlatform key isn't the default."
            .to_owned(),
    );
    steps.push(
        "This doesn't touch the passkey (FIDO) applet. A new PivApplet starts with \
         PIN 123456 and PUK 12345678."
            .to_owned(),
    );
    steps
}

/// "Last seen" for a card row.
pub fn last_seen_text(now: u64, last_seen: u64, present: bool) -> String {
    if present {
        return "now".into();
    }
    let ago = now.saturating_sub(last_seen);
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match ago {
        0..60 => "just now".into(),
        60..3600 => plural(ago / 60, "minute"),
        3600..86_400 => plural(ago / 3600, "hour"),
        86_400..172_800 => "yesterday".into(),
        172_800..5_184_000 => plural(ago / 86_400, "day"),
        _ => plural(ago / 2_592_000, "month"),
    }
}

/// The pairing window's "tap a card to test" step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestTap {
    /// Waiting for a card; `asked` once ReadCard (or a wake) went out.
    Waiting {
        since: Instant,
        asked: bool,
    },
    Works {
        label: String,
    },
    TimedOut,
}

/// How long the test step waits for a card.
pub const TEST_WAIT: Duration = Duration::from_secs(60);

impl TestTap {
    pub fn start(now: Instant) -> Self {
        Self::Waiting {
            since: now,
            asked: false,
        }
    }

    /// Called every tick. Returns true when the tap request should go out
    /// now (once, as soon as the new phone's session is live).
    pub fn tick(&mut self, now: Instant, connected: bool) -> bool {
        match self {
            Self::Waiting { since, .. } if now.duration_since(*since) >= TEST_WAIT => {
                *self = Self::TimedOut;
                false
            }
            Self::Waiting { asked, .. } if connected && !*asked => {
                *asked = true;
                true
            }
            _ => false,
        }
    }

    /// A card arrived on the phone.
    pub fn card(&mut self, label: String) {
        if matches!(self, Self::Waiting { .. }) {
            *self = Self::Works { label };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: [u8; 4] = [4, 1, 2, 3];

    #[test]
    fn setup_on_a_live_session_goes_straight_to_the_phone() {
        let now = Instant::now();
        let (mut task, effect) = CardTask::setup(UID.to_vec());
        assert_eq!(effect, Effect::SendSetup);
        assert_eq!(task.setup_sent(true, now), None);
        assert_eq!(task.step, Step::OnPhone);
        assert_eq!(task.tick(now, true, &[]), None);
        task.setup_done(true);
        assert_eq!(task.step, Step::SetupDone { ok: true });
        assert!(!task.failed());
    }

    #[test]
    fn setup_without_a_session_wakes_then_sends_once_connected() {
        let now = Instant::now();
        let (mut task, _) = CardTask::setup(UID.to_vec());
        assert_eq!(task.setup_sent(false, now), Some(Effect::RequestTap));
        assert!(matches!(task.step, Step::Connecting { .. }));
        assert_eq!(task.tick(now, false, &[]), None);
        assert_eq!(task.tick(now, true, &[]), Some(Effect::SendSetup));
        assert_eq!(task.setup_sent(true, now), None);
        assert_eq!(task.step, Step::OnPhone);
        assert!(task.setup_done(false));
        assert!(task.failed());
        // A late or repeated answer changes nothing.
        assert!(!task.setup_done(true));
        assert!(task.failed());
    }

    #[test]
    fn setup_gives_up_when_the_phone_never_connects() {
        let now = Instant::now();
        let (mut task, _) = CardTask::setup(UID.to_vec());
        task.setup_sent(false, now);
        assert_eq!(task.tick(now + CONNECT_WAIT, false, &[]), None);
        assert!(task.failed());
        assert!(task.text().contains("Open tapbridge on your phone"));
        // No push token at all: same message at once.
        let (mut task, _) = CardTask::setup(UID.to_vec());
        task.setup_sent(false, now);
        task.unreachable();
        assert!(task.text().contains("Open tapbridge on your phone"));
    }

    #[test]
    fn mac_login_waits_for_the_right_card() {
        let now = Instant::now();
        let (mut task, effect) = CardTask::mac_login(UID.to_vec(), false, now);
        assert_eq!(effect, Effect::RequestTap);
        assert_eq!(task.tick(now, true, &[]), None);
        assert_eq!(task.tick(now, true, &[4, 9, 9, 9]), None);
        assert_eq!(task.tick(now, true, &UID), Some(Effect::RunPairing));
        assert!(!task.dismissable() && task.busy());
        // Only once.
        assert_eq!(task.tick(now, true, &UID), None);
        task.paired(true, "Done.".into());
        assert!(!task.busy());
        assert_eq!(
            task.step,
            Step::Finished {
                ok: true,
                text: "Done.".into()
            }
        );
        let (task, effect) = CardTask::mac_login(UID.to_vec(), true, now);
        assert_eq!((task.step, effect), (Step::Pairing, Effect::RunPairing));
    }

    #[test]
    fn mac_login_times_out_without_a_tap() {
        let now = Instant::now();
        let (mut task, _) = CardTask::mac_login(UID.to_vec(), false, now);
        task.tick(now + CARD_WAIT, true, &[]);
        assert!(task.failed() && task.dismissable());
    }

    #[test]
    fn last_seen_reads_naturally() {
        let now = 10_000_000;
        assert_eq!(last_seen_text(now, 0, true), "now");
        assert_eq!(last_seen_text(now, now - 5, false), "just now");
        assert_eq!(last_seen_text(now, now - 60, false), "1 minute ago");
        assert_eq!(last_seen_text(now, now - 7200, false), "2 hours ago");
        assert_eq!(last_seen_text(now, now - 90_000, false), "yesterday");
        assert_eq!(last_seen_text(now, now - 3 * 86_400, false), "3 days ago");
        assert_eq!(
            last_seen_text(now, now - 90 * 86_400, false),
            "3 months ago"
        );
        // A clock that went backwards is not "in the future".
        assert_eq!(last_seen_text(now, now + 50, false), "just now");
    }

    #[test]
    fn test_tap_asks_once_connected_and_times_out() {
        let now = Instant::now();
        let mut test = TestTap::start(now);
        assert!(!test.tick(now, false));
        assert!(test.tick(now, true));
        assert!(!test.tick(now, true));
        test.card("Card 1".into());
        assert_eq!(
            test,
            TestTap::Works {
                label: "Card 1".into()
            }
        );
        // Works stays put.
        assert!(!test.tick(now + TEST_WAIT, true));
        let mut test = TestTap::start(now);
        assert!(!test.tick(now + TEST_WAIT, true));
        assert_eq!(test, TestTap::TimedOut);
        test.card("late".into());
        assert_eq!(test, TestTap::TimedOut);
    }

    fn card(
        edit: impl FnOnce(&mut tapbridge_host::cards::KnownCard),
    ) -> tapbridge_host::cards::KnownCard {
        let mut card = tapbridge_host::cards::KnownCard::default();
        edit(&mut card);
        card
    }

    #[test]
    fn each_step_waits_for_the_one_before() {
        use StepState::*;
        let none = Checklist::new(&card(|_| {}));
        assert_eq!(
            (none.applet, none.certificates, none.mac_login),
            (Todo, Locked, Locked)
        );
        let applet = Checklist::new(&card(|c| c.has_piv = true));
        assert_eq!(
            (applet.applet, applet.certificates, applet.mac_login),
            (Done, Todo, Locked)
        );
        let certs = Checklist::new(&card(|c| {
            c.has_piv = true;
            c.piv_certs = true;
        }));
        assert_eq!(
            (certs.applet, certs.certificates, certs.mac_login),
            (Done, Done, Todo)
        );
        let ready = Checklist::new(&card(|c| {
            c.has_piv = true;
            c.piv_certs = true;
            c.mac_login = true;
        }));
        assert!(ready.ready());
        // A stale Mac login flag can't skip missing certificates.
        let stale = Checklist::new(&card(|c| {
            c.has_piv = true;
            c.mac_login = true;
        }));
        assert_eq!(stale.mac_login, Locked);
    }

    #[test]
    fn a_blocking_applet_is_a_problem_and_locks_the_rest() {
        let blocked = Checklist::new(&card(|c| {
            c.has_piv = true;
            c.piv_blocked = true;
        }));
        assert_eq!(blocked.applet, StepState::Problem);
        assert_eq!(blocked.certificates, StepState::Locked);
    }

    #[test]
    fn flexsecure_is_recognised_by_its_atr() {
        let atr = [
            0x3B, 0x8D, 0x80, 0x01, 0x00, 0x31, 0xC1, 0x73, 0xC8, 0x40, 0x00, 0x52, 0xA5, 0x10,
            0x00, 0x90, 0x00, 0x70,
        ];
        assert!(is_flexsecure(&atr));
        let mut other = atr;
        other[8] = 0x00;
        assert!(!is_flexsecure(&other));
        assert!(!is_flexsecure(&[]));
        assert!(!is_flexsecure(&[0x3B, 0x80, 0x80, 0x01, 0x01]));
    }

    #[test]
    fn install_steps_name_the_exact_build_for_a_flexsecure() {
        let flex = [
            0x3B, 0x8D, 0x80, 0x01, 0x00, 0x31, 0xC1, 0x73, 0xC8, 0x40, 0x00, 0x52, 0xA5, 0x10,
            0x00, 0x90, 0x00, 0x70,
        ];
        let steps = install_steps(&flex, false).join(" ");
        assert!(steps.contains(FLEXSECURE_CAP));
        assert!(!steps.contains("strict-contactless"));
        let generic = install_steps(&[], true).join(" ");
        assert!(!generic.contains(FLEXSECURE_CAP));
        assert!(generic.contains("strict-contactless"));
        assert!(generic.contains("gp.jar"));
    }
}
