//! "Use for Mac login": pairs the macOS user with the PIV identity on the
//! card that is on the phone right now, through `sc_auth`.
//!
//! `sc_auth pair` needs admin rights. It runs under `osascript ... with
//! administrator privileges`, so macOS shows its own password dialog and
//! tapbridge never sees the password. Every command goes through
//! [`Runner`], which tests replace with a fake: nothing here may run
//! `sc_auth` or `osascript` for real under `cargo test`.

use std::{io, time::Duration};

const SC_AUTH: &str = "/usr/sbin/sc_auth";
const OSASCRIPT: &str = "/usr/bin/osascript";
const LAUNCHCTL: &str = "/bin/launchctl";

/// One identity line from `sc_auth identities`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Public key hash, as `sc_auth pair -h` takes it.
    pub hash: String,
    pub label: String,
    pub paired: bool,
}

/// One `SmartCard:` block: a token that is inserted right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub id: String,
    pub identities: Vec<Identity>,
}

/// Parses `sc_auth identities` output. Lines it doesn't understand are
/// skipped; an identity line before any `SmartCard:` header is ignored.
pub fn parse_identities(text: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut paired = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(id) = trimmed.strip_prefix("SmartCard:") {
            tokens.push(Token {
                id: id.trim().to_owned(),
                identities: Vec::new(),
            });
            paired = None;
        } else if trimmed.starts_with("Unpaired identities") {
            paired = Some(false);
        } else if trimmed.starts_with("Paired identities") {
            // macOS 26 says "Paired identities which are used for
            // authentication:"; older versions "Paired identities:".
            paired = Some(true);
        } else if let (Some(paired), Some(token)) = (paired, tokens.last_mut()) {
            let (hash, label) = trimmed
                .split_once(char::is_whitespace)
                .unwrap_or((trimmed, ""));
            if valid_hash(hash) {
                token.identities.push(Identity {
                    hash: hash.to_owned(),
                    label: label.trim().to_owned(),
                    paired,
                });
            }
        }
    }
    tokens
}

/// SHA-1 public key hash: 40 hex digits. Anything else never reaches a
/// command line.
fn valid_hash(hash: &str) -> bool {
    hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Short macOS account names: letters, digits, `.`, `_`, `-`.
fn valid_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 64
        && !user.starts_with('-')
        && user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Which identity to pair, from the inserted tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Pair(String),
    AlreadyPaired,
    /// No inserted card has a PIV identity (yet).
    NoIdentity,
    /// More than one inserted card could be meant.
    Ambiguous,
}

/// Only the cards that are inserted are listed, and the tapbridge reader
/// holds the phone's card. With one candidate card, pair its PIV
/// Authentication identity (slot 9A), else its only identity.
pub fn pick(tokens: &[Token]) -> Pick {
    let with_identities: Vec<&Token> = tokens
        .iter()
        .filter(|token| !token.identities.is_empty())
        .collect();
    let token = match with_identities.as_slice() {
        [] => return Pick::NoIdentity,
        [token] => *token,
        _ => {
            // Several cards, but only one still needs pairing.
            let unpaired: Vec<&&Token> = with_identities
                .iter()
                .filter(|token| token.identities.iter().any(|id| !id.paired))
                .collect();
            match unpaired.as_slice() {
                [] => return Pick::AlreadyPaired,
                [token] => **token,
                _ => return Pick::Ambiguous,
            }
        }
    };
    let preferred = |id: &Identity| id.label.contains("PIV Authentication");
    if token.identities.iter().any(|id| preferred(id) && id.paired) {
        return Pick::AlreadyPaired;
    }
    let unpaired: Vec<&Identity> = token.identities.iter().filter(|id| !id.paired).collect();
    if let Some(identity) = unpaired.iter().find(|id| preferred(id)) {
        return Pick::Pair(identity.hash.clone());
    }
    match unpaired.as_slice() {
        [] => Pick::AlreadyPaired,
        [identity] => Pick::Pair(identity.hash.clone()),
        _ => Pick::Ambiguous,
    }
}

/// How a "Use for Mac login" attempt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Paired. `keychain` is false when macOS couldn't store the login
    /// keychain unlock key, so the first smart-card login asks for the
    /// password once.
    Paired {
        keychain: bool,
    },
    AlreadyPaired,
    /// The card left the phone mid-way (CryptoTokenKit error -8).
    CardLeft,
    /// The user dismissed the macOS password dialog.
    Cancelled,
    NoIdentity,
    Ambiguous,
    Failed(String),
}

impl Outcome {
    pub fn ok(&self) -> bool {
        matches!(self, Self::Paired { .. } | Self::AlreadyPaired)
    }

    /// One plain sentence for the Cards section.
    pub fn message(&self) -> String {
        match self {
            Self::Paired { keychain: true } => "Your Mac login now works with this card.".into(),
            Self::Paired { keychain: false } => "Your Mac login now works with this card. \
                 The first time you log in with it, macOS asks for your password once \
                 to unlock your keychain."
                .into(),
            Self::AlreadyPaired => "This card is already set up for your Mac login.".into(),
            Self::CardLeft => "The card moved away. Keep it on your phone and try again.".into(),
            Self::Cancelled => "Pairing was cancelled.".into(),
            Self::NoIdentity => "Your Mac can't see a login certificate on this card yet. \
                 Keep it on your phone and try again."
                .into(),
            Self::Ambiguous => {
                "More than one smart card is connected. Remove the others and try again.".into()
            }
            Self::Failed(reason) => format!("Pairing didn't work: {reason}"),
        }
    }
}

/// Reads what `sc_auth pair` (through osascript) printed.
pub fn classify_pair(succeeded: bool, output: &str, now_paired: bool) -> Outcome {
    let keychain = !output.contains("Failed to store Login keychain unlock key");
    if output.contains("successfully paired") || now_paired {
        return Outcome::Paired { keychain };
    }
    // -8 is the card going away mid-operation. Other CryptoTokenKit
    // errors (e.g. -7, token not visible) are real failures.
    if output.contains("CryptoTokenKit error -8") {
        return Outcome::CardLeft;
    }
    // osascript: "User canceled. (-128)"
    if output.contains("(-128)") || output.contains("User canceled") {
        return Outcome::Cancelled;
    }
    let reason = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(if succeeded {
            "sc_auth gave no answer"
        } else {
            "sc_auth failed"
        });
    Outcome::Failed(reason.chars().take(200).collect())
}

/// The AppleScript that runs `sc_auth pair` as admin. Both values are
/// validated, so they need no quoting inside the script.
///
/// The admin shell runs in the system context, where CryptoTokenKit's
/// system `ctkd` can't see the card's token (it lives in the user's
/// session): `sc_auth` then fails with error -7, "missing tokenID".
/// `launchctl asuser` runs it, still as root, in the user's session, which
/// is what `sudo` from Terminal gets for free.
pub fn pair_script(user: &str, uid: u32, hash: &str) -> Option<String> {
    (valid_user(user) && valid_hash(hash)).then(|| {
        format!(
            "do shell script \"{LAUNCHCTL} asuser {uid} {SC_AUTH} pair -u {user} -h {hash} 2>&1\" \
             with administrator privileges"
        )
    })
}

/// Runs one program, returning (exited successfully, stdout + stderr).
pub trait Runner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)>;
}

pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
        let output = std::process::Command::new(program).args(args).output()?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok((output.status.success(), text))
    }
}

fn identities(runner: &dyn Runner) -> io::Result<Vec<Token>> {
    let (_, text) = runner.run(SC_AUTH, &["identities"])?;
    Ok(parse_identities(&text))
}

/// Pairs `user` with the PIV identity of the card on the phone. macOS's
/// token driver needs a moment after the card arrives, so the identity
/// lookup is tried `attempts` times, `pause` apart.
pub fn pair_present_card(
    runner: &dyn Runner,
    user: &str,
    attempts: u32,
    pause: Duration,
) -> Outcome {
    let mut picked = Pick::NoIdentity;
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            std::thread::sleep(pause);
        }
        picked = match identities(runner) {
            Ok(tokens) => pick(&tokens),
            Err(error) => return Outcome::Failed(error.to_string()),
        };
        if picked != Pick::NoIdentity {
            break;
        }
    }
    let hash = match picked {
        Pick::Pair(hash) => hash,
        Pick::AlreadyPaired => return Outcome::AlreadyPaired,
        Pick::NoIdentity => return Outcome::NoIdentity,
        Pick::Ambiguous => return Outcome::Ambiguous,
    };
    let Some(script) = pair_script(user, current_uid(), &hash) else {
        return Outcome::Failed("unexpected account name".into());
    };
    let (succeeded, output) = match runner.run(OSASCRIPT, &["-e", &script]) {
        Ok(result) => result,
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    let now_paired = identities(runner).is_ok_and(|tokens| {
        tokens
            .iter()
            .flat_map(|token| &token.identities)
            .any(|identity| identity.hash == hash && identity.paired)
    });
    classify_pair(succeeded, &output, now_paired)
}

/// Whether the inserted card's login identity (PIV Authentication,
/// else its only identity) is paired: `None` while no inserted card has an
/// identity yet, or several cards make it unclear.
pub fn login_paired(tokens: &[Token]) -> Option<bool> {
    match pick(tokens) {
        Pick::AlreadyPaired => Some(true),
        Pick::Pair(_) => Some(false),
        Pick::NoIdentity | Pick::Ambiguous => None,
    }
}

/// Reads [`login_paired`] for the card on the phone, retrying while
/// macOS's token driver catches up with a fresh tap.
pub fn present_card_paired(runner: &dyn Runner, attempts: u32, pause: Duration) -> Option<bool> {
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            std::thread::sleep(pause);
        }
        if let Some(paired) = identities(runner)
            .ok()
            .and_then(|tokens| login_paired(&tokens))
        {
            return Some(paired);
        }
    }
    None
}

/// The app's user id (the logged-in user: the app runs in their session).
fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    // The home directory is owned by the user; no libc needed.
    std::env::var_os("HOME")
        .and_then(|home| std::fs::metadata(home).ok())
        .map_or(501, |meta| meta.uid())
}

/// The logged-in user's short name.
pub fn current_user() -> Option<String> {
    std::env::var("USER").ok().filter(|user| valid_user(user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque};

    const HASH: &str = "806D2E7FC2DD43F043BAB139643DC2DA703A110C";
    /// Real output from the user's Mac.
    const UNPAIRED: &str = "SmartCard: com.apple.pivtoken:147A799AD94549E587912D573A74BDAC\n\
        Unpaired identities:\n\
        806D2E7FC2DD43F043BAB139643DC2DA703A110C\tCertificate For PIV Authentication (Pixel 9 Pro XL PIV)\n";
    const PAIRED: &str = "SmartCard: com.apple.pivtoken:147A799AD94549E587912D573A74BDAC\n\
        Paired identities:\n\
        806D2E7FC2DD43F043BAB139643DC2DA703A110C\tCertificate For PIV Authentication (Pixel 9 Pro XL PIV)\n";
    const SUDO_PAIR: &str = "Failed to store Login keychain unlock key. Login keychain unlock key \
        can be stored only when pairing the current user.\nUser was successfully paired (public \
        key hash: 806D2E7FC2DD43F043BAB139643DC2DA703A110C) but user password will be required \
        after next SmartCard login to unlock Login keychain.\n";

    #[test]
    fn parses_real_unpaired_output() {
        let tokens = parse_identities(UNPAIRED);
        assert_eq!(
            tokens,
            [Token {
                id: "com.apple.pivtoken:147A799AD94549E587912D573A74BDAC".into(),
                identities: vec![Identity {
                    hash: HASH.into(),
                    label: "Certificate For PIV Authentication (Pixel 9 Pro XL PIV)".into(),
                    paired: false,
                }],
            }]
        );
        assert_eq!(pick(&tokens), Pick::Pair(HASH.into()));
    }

    /// macOS 26 words the paired section differently.
    #[test]
    fn parses_macos_26_paired_heading() {
        let text = "SmartCard: com.apple.pivtoken:147A799AD94549E587912D573A74BDAC\n\
            Paired identities which are used for authentication:\n\
            806D2E7FC2DD43F043BAB139643DC2DA703A110C\tCertificate For PIV Authentication (Pixel 9 Pro XL PIV)\n";
        let tokens = parse_identities(text);
        assert!(tokens[0].identities[0].paired);
        assert_eq!(pick(&tokens), Pick::AlreadyPaired);
        assert_eq!(login_paired(&tokens), Some(true));
    }

    #[test]
    fn parses_paired_sections_and_several_cards() {
        let tokens = parse_identities(PAIRED);
        assert!(tokens[0].identities[0].paired);
        assert_eq!(pick(&tokens), Pick::AlreadyPaired);
        let text = format!(
            "{PAIRED}SmartCard: com.apple.pivtoken:AAAA\nUnpaired identities:\n\
             {}\tCertificate For Digital Signature (Other)\n\
             {}\tCertificate For PIV Authentication (Other)\n",
            "1".repeat(40),
            "2".repeat(40)
        );
        let tokens = parse_identities(&text);
        assert_eq!(tokens.len(), 2);
        // Only the second card still needs pairing; its 9A identity wins.
        assert_eq!(pick(&tokens), Pick::Pair("2".repeat(40)));
    }

    #[test]
    fn pick_handles_empty_and_ambiguous_listings() {
        assert_eq!(pick(&parse_identities("")), Pick::NoIdentity);
        assert_eq!(
            pick(&parse_identities("SmartCard: com.apple.pivtoken:X\n")),
            Pick::NoIdentity
        );
        let two = format!(
            "SmartCard: a\nUnpaired identities:\n{}\tCertificate For PIV Authentication (A)\n\
             SmartCard: b\nUnpaired identities:\n{}\tCertificate For PIV Authentication (B)\n",
            "A".repeat(40),
            "B".repeat(40)
        );
        assert_eq!(pick(&parse_identities(&two)), Pick::Ambiguous);
        // Junk and identity lines before any header are ignored.
        let junk = format!(
            "{}\tstray\nSmartCard: a\nUnpaired identities:\nnot-a-hash\tx\n\"; rm -rf /\tx\n",
            "C".repeat(40)
        );
        assert_eq!(pick(&parse_identities(&junk)), Pick::NoIdentity);
    }

    #[test]
    fn classifies_pair_output() {
        assert_eq!(
            classify_pair(true, SUDO_PAIR, false),
            Outcome::Paired { keychain: false }
        );
        assert_eq!(
            classify_pair(true, "", true),
            Outcome::Paired { keychain: true }
        );
        assert_eq!(
            classify_pair(false, "Error pairing (CryptoTokenKit error -8.)", false),
            Outcome::CardLeft
        );
        assert_eq!(
            classify_pair(false, "Error pairing (CryptoTokenKit error -7.)", false),
            Outcome::Failed("Error pairing (CryptoTokenKit error -7.)".into())
        );
        assert_eq!(
            classify_pair(false, "0:89: execution error: User canceled. (-128)", false),
            Outcome::Cancelled
        );
        assert_eq!(
            classify_pair(false, "\n  Something odd\nmore", false),
            Outcome::Failed("Something odd".into())
        );
        assert!(
            Outcome::Paired { keychain: false }
                .message()
                .contains("password once")
        );
    }

    #[test]
    fn pair_script_refuses_unsafe_values() {
        assert_eq!(
            pair_script("ethernet", 501, HASH).unwrap(),
            format!(
                "do shell script \"/bin/launchctl asuser 501 /usr/sbin/sc_auth pair -u ethernet -h {HASH} 2>&1\" \
                 with administrator privileges"
            )
        );
        assert!(pair_script("eth\"; say hi", 501, HASH).is_none());
        assert!(pair_script("-u", 501, HASH).is_none());
        assert!(pair_script("ethernet", 501, "806D").is_none());
        assert!(pair_script("ethernet", 501, &format!("{}\"", &HASH[..39])).is_none());
    }

    /// Replays canned answers and records every command.
    struct Fake {
        answers: RefCell<VecDeque<(bool, String)>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn new(answers: &[(bool, &str)]) -> Self {
            Self {
                answers: RefCell::new(
                    answers
                        .iter()
                        .map(|(ok, text)| (*ok, (*text).to_owned()))
                        .collect(),
                ),
                calls: RefCell::default(),
            }
        }
    }

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().map(|arg| (*arg).to_owned()));
            self.calls.borrow_mut().push(call);
            self.answers
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| io::Error::other("unexpected command"))
        }
    }

    #[test]
    fn pairs_the_present_card_through_osascript() {
        let fake = Fake::new(&[(true, UNPAIRED), (true, SUDO_PAIR), (true, PAIRED)]);
        let outcome = pair_present_card(&fake, "ethernet", 3, Duration::ZERO);
        assert_eq!(outcome, Outcome::Paired { keychain: false });
        let calls = fake.calls.borrow();
        assert_eq!(calls[0], ["/usr/sbin/sc_auth", "identities"]);
        assert_eq!(calls[1][..2], ["/usr/bin/osascript", "-e"]);
        assert!(calls[1][2].contains(&format!("pair -u ethernet -h {HASH}")));
        assert!(calls[1][2].ends_with("with administrator privileges"));
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn waits_for_the_token_then_gives_up_without_asking_for_admin() {
        let empty = "";
        let fake = Fake::new(&[
            (true, empty),
            (true, UNPAIRED),
            (false, "(-128)"),
            (true, UNPAIRED),
        ]);
        assert_eq!(
            pair_present_card(&fake, "ethernet", 3, Duration::ZERO),
            Outcome::Cancelled
        );
        let fake = Fake::new(&[(true, empty), (true, empty)]);
        assert_eq!(
            pair_present_card(&fake, "ethernet", 2, Duration::ZERO),
            Outcome::NoIdentity
        );
        assert!(
            fake.calls
                .borrow()
                .iter()
                .all(|call| call[0] != "/usr/bin/osascript")
        );
        let fake = Fake::new(&[(true, PAIRED)]);
        assert_eq!(
            pair_present_card(&fake, "ethernet", 2, Duration::ZERO),
            Outcome::AlreadyPaired
        );
    }

    #[test]
    fn login_paired_reads_the_present_card() {
        assert_eq!(login_paired(&parse_identities(UNPAIRED)), Some(false));
        assert_eq!(login_paired(&parse_identities(PAIRED)), Some(true));
        assert_eq!(login_paired(&parse_identities("")), None);
    }

    #[test]
    fn present_card_paired_waits_for_the_token_and_never_pairs() {
        let fake = Fake::new(&[(true, ""), (true, PAIRED)]);
        assert_eq!(present_card_paired(&fake, 3, Duration::ZERO), Some(true));
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| call.iter().any(|arg| arg == "identities"))
        );
    }
}
