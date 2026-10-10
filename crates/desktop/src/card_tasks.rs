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
    /// Waiting for the card on the phone before pairing.
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

    /// The phone reported the setup outcome.
    pub fn setup_done(&mut self, ok: bool) {
        if matches!(self.step, Step::OnPhone | Step::Connecting { .. }) {
            self.step = Step::SetupDone { ok };
        }
    }

    /// "Use for Mac login": runs at once when the card is on the phone.
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
        task.setup_done(false);
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
}
