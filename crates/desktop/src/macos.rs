//! macOS desktop support (Track A): the PC/SC reader socket, LaunchAgent
//! autostart, and platform paths. Windows modules (usbip, prompt_watch,
//! passkeys, win, single) are cfg'd out; the tray/service/iroh stack is
//! shared as-is.
//!
//! Reader model per PLAN.md: the reader is always present to PC/SC (the
//! IFD bundle claims a USB VID/PID that is always "attached" from pcscd's
//! perspective — com.apple.ifdreader loads the shim at startup); a card is
//! present exactly when a phone session with a tag exists. When PC/SC
//! powers the card with no live session, the daemon fires the same FCM
//! wake the tray "Request tap" uses, rate-limited.

use std::{
    io::{self, Read, Write},
    os::unix::{net::UnixListener, net::UnixStream},
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use tapbridge_host::{
    ifd::{self as ifd_proto, Request},
    service::{ServiceHandle, Status},
};

/// Where the IFD shim connects. Fixed path: the shim is installed system-wide
/// and cannot know the state dir. World-writable dir, 0o600 socket.
pub const IFD_SOCKET_PATH: &str = "/var/tmp/tapbridge-ifd.sock";

/// Minimum spacing between automatic phone wakes triggered by PC/SC card
/// activity (pcscd polls presence ~1 Hz; a burst must wake at most once).
const WAKE_MIN_INTERVAL: Duration = Duration::from_secs(30);

/// Dock-icon policy: the app is tray-only (no Dock icon) while no visible
/// window is open. `LSUIElement` in Info.plist hides it at launch; the UI
/// tick calls [`dock::sync`] with the count of live windows, which flips the
/// activation policy at runtime. Idempotent: state changes only on a
/// transition. All calls must come from the gpui main thread (AppKit).
#[cfg(target_os = "macos")]
pub mod dock {
    use std::sync::atomic::{AtomicU8, Ordering};

    const UNKNOWN: u8 = 0;
    const HIDDEN: u8 = 1;
    const SHOWN: u8 = 2;

    /// Starts UNKNOWN, not HIDDEN: gpui's `applicationDidFinishLaunching`
    /// forces the Regular policy (a Dock icon) over `LSUIElement`, so the
    /// first sync must always apply the policy, even with zero windows.
    static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

    /// Reports how many visible windows are open; shows/hides the Dock icon
    /// on transitions. The anchor window is excluded (it is never visible).
    pub fn sync(visible_windows: usize) {
        let want = if visible_windows > 0 { SHOWN } else { HIDDEN };
        if STATE.swap(want, Ordering::SeqCst) == want {
            return;
        }
        set_policy(want == SHOWN);
    }

    /// Shows the Dock icon and makes tapbridge the active app, so the window
    /// about to be opened or refreshed lands in front. Call before every
    /// open/refresh: a tray click does not activate the app, and a window
    /// already open behind other apps gets no policy transition to ride on.
    pub fn bring_to_front() {
        STATE.store(SHOWN, Ordering::SeqCst);
        set_policy(true);
    }

    fn set_policy(visible: bool) {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let Some(mtm) = MainThreadMarker::new() else {
            return; // Not the main thread: AppKit calls are invalid anyway.
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        });
        if visible {
            // Cooperative `activate()` is ignored when the request comes from
            // a status-item menu (no app yields to us); the forcing variant
            // is deprecated but is what actually raises a menu bar app.
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
    }
}

/// Rate-limit state: last instant a PC/SC-triggered wake was sent.
/// The daemon owns one; the IFD socket server creates its own instance.
#[derive(Default)]
pub(crate) struct WakeLimiter {
    last: Mutex<Option<Instant>>,
}

impl WakeLimiter {
    /// True when a wake may be sent now (records it).
    pub(crate) fn allowed(&self) -> bool {
        let Ok(mut last) = self.last.lock() else {
            return false;
        };
        match *last {
            Some(at) if at.elapsed() < WAKE_MIN_INTERVAL => false,
            _ => {
                *last = Some(Instant::now());
                true
            }
        }
    }
}

/// State dir: `~/Library/Application Support/tapbridge` (created lazily).
pub fn state_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Application Support/tapbridge");
    }
    PathBuf::from("tapbridge-state")
}

// ---------------------------------------------------------------
// Start at login: a per-user LaunchAgent plist the app writes itself
// (mirrors the Windows Run-key autostart; no SMAppService entitlements
// needed). Label computer.ethernet.tapbridge, RunAtLoad + KeepAlive off:
// the tray app quits when the user quits it.
// ---------------------------------------------------------------

const LAUNCH_AGENT_LABEL: &str = "computer.ethernet.tapbridge";

fn launch_agent_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
    })
}

/// The LaunchAgent plist body for the given executable path.
fn launch_agent_plist(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCH_AGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
</dict>
</plist>
"#
    )
}

/// Whether the LaunchAgent exists (start-at-login enabled).
pub fn autostart_is_enabled() -> bool {
    launch_agent_path().is_some_and(|path| path.is_file())
}

/// Enables/disables start at login by writing/removing the LaunchAgent.
pub fn autostart_set_enabled(enabled: bool) -> Result<(), String> {
    let Some(path) = launch_agent_path() else {
        return Err("HOME not set; cannot manage LaunchAgent".into());
    };
    if enabled {
        let exe = std::env::current_exe().map_err(|error| format!("exe path: {error}"))?;
        let dir = path.parent().unwrap_or(&path).to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|error| format!("{error}"))?;
        std::fs::write(&path, launch_agent_plist(&exe.display().to_string()))
            .map_err(|error| format!("{error}"))?;
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{error}")),
        }
    }
    Ok(())
}

/// Creates and listens on the IFD socket at the canonical path; returns the
/// listener. Replaces any stale socket from a previous run (daemon owns the
/// path exclusively via host.lock before this is called).
pub fn listen() -> io::Result<UnixListener> {
    let path = PathBuf::from(IFD_SOCKET_PATH);
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// Test hook: bind the same protocol on an arbitrary path.
#[cfg(test)]
fn listen_at(path: PathBuf) -> UnixListener {
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind IFD socket");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod IFD socket");
    }
    listener
}

/// How long a transmit to the always-present card waits for a tap.
const VIRTUAL_WAIT: Duration = Duration::from_secs(25);

/// Name of the file whose presence turns the always-present card on.
const ALWAYS_PRESENT_FILE: &str = "always-present";
/// The always-present card's read-only answers, kept across restarts.
const ALWAYS_PRESENT_CACHE: &str = "always-present-cache.txt";

/// Read-only answers of the login card, replayed while it isn't tapped.
///
/// When a card appears, macOS reads its certificates straight away. A
/// card shown without a tap would wake the phone every time, so those
/// reads (SELECT, GET DATA, GET VERSION, GET SERIAL and the GET RESPONSE
/// chains that follow them) are answered from the last real tap. Their
/// data is public. Anything else (PIN, signing) needs the real card.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct ApduCache {
    answers: std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
    /// The command a GET RESPONSE chain continues, and how far along it is.
    chain: Option<(Vec<u8>, u8)>,
    /// The last SELECT, replayed on a fresh tap: the cache answered it, so
    /// the real card never saw it.
    pub last_select: Option<Vec<u8>>,
}

/// SELECT, GET DATA, GET VERSION (Yubico), GET SERIAL (Yubico).
fn cacheable(apdu: &[u8]) -> bool {
    apdu.len() >= 4 && apdu[0] & 0xF0 == 0 && matches!(apdu[1], 0xA4 | 0xCB | 0xFD | 0xF8)
}

fn get_response(apdu: &[u8]) -> bool {
    apdu.len() >= 4 && apdu[0] & 0xF0 == 0 && apdu[1] == 0xC0
}

impl ApduCache {
    /// The key for `apdu`, advancing the chain; `None` for commands that
    /// must reach the card.
    fn key(&mut self, apdu: &[u8]) -> Option<Vec<u8>> {
        if get_response(apdu) {
            let (command, step) = self.chain.as_mut()?;
            *step = step.checked_add(1)?;
            let mut key = command.clone();
            key.extend([0xFF, *step]);
            return Some(key);
        }
        if !cacheable(apdu) {
            self.chain = None;
            return None;
        }
        if apdu[1] == 0xA4 {
            self.last_select = Some(apdu.to_vec());
        }
        self.chain = Some((apdu.to_vec(), 0));
        let mut key = apdu.to_vec();
        key.extend([0xFF, 0]);
        Some(key)
    }

    /// Answers `apdu` from the cache. `Err(())` when it has to go to the card.
    pub fn answer(&mut self, apdu: &[u8]) -> Result<Vec<u8>, ()> {
        let key = self.key(apdu).ok_or(())?;
        self.answers.get(&key).cloned().ok_or(())
    }

    /// Notes what the real card answered. True when the cache changed.
    pub fn learn(&mut self, apdu: &[u8], response: &[u8]) -> bool {
        let Some(key) = self.key(apdu) else {
            return false;
        };
        // 9000, 61xx (more data) and 6A82 (no such object) are stable.
        let stable = matches!(
            response,
            [.., 0x90, 0x00] | [.., 0x61, _] | [.., 0x6A, 0x82]
        );
        if !stable {
            return false;
        }
        self.answers.insert(key, response.to_vec()) != Some(response.to_vec())
    }

    pub fn is_empty(&self) -> bool {
        self.answers.is_empty()
    }

    fn parse(text: &str) -> Self {
        let unhex = |text: &str| -> Option<Vec<u8>> {
            text.len()
                .is_multiple_of(2)
                .then(|| {
                    (0..text.len())
                        .step_by(2)
                        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
                        .collect()
                })
                .flatten()
        };
        let answers = text
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once('\t')?;
                Some((unhex(key)?, unhex(value)?))
            })
            .collect();
        Self {
            answers,
            ..Self::default()
        }
    }

    fn render(&self) -> String {
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
        self.answers
            .iter()
            .map(|(key, value)| format!("{}\t{}\n", hex(key), hex(value)))
            .collect()
    }
}

/// "Always present" card: macOS sees the login card in the reader even
/// without a tap, so the lock screen, `sudo` and other prompts offer the
/// card straight away. Read-only commands are answered from [`ApduCache`];
/// the first one that needs the card wakes the phone and waits for the
/// tap, and the rest is proxied as usual.
///
/// When that wait times out the card is reported gone (`dropped`) until a
/// real tap, so prompts fall back to the password instead of hanging.
#[derive(Default)]
pub struct AlwaysPresent {
    enabled: std::sync::atomic::AtomicBool,
    /// The ATR to present: the card paired with this Mac's login.
    atr: Mutex<Vec<u8>>,
    dropped: std::sync::atomic::AtomicBool,
    cache: Mutex<ApduCache>,
    /// Where the cache is saved; unset in tests.
    cache_path: Mutex<Option<PathBuf>>,
    /// The card generation the last SELECT was replayed to.
    selected: Mutex<Option<u64>>,
}

impl AlwaysPresent {
    pub fn enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
        self.dropped
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Loads the saved cache from `state_dir` and saves there from now on.
    pub fn load_cache(&self, state_dir: &std::path::Path) {
        let path = state_dir.join(ALWAYS_PRESENT_CACHE);
        let cache = std::fs::read_to_string(&path)
            .map(|text| ApduCache::parse(&text))
            .unwrap_or_default();
        if let Ok(mut slot) = self.cache.lock() {
            *slot = cache;
        }
        if let Ok(mut slot) = self.cache_path.lock() {
            *slot = Some(path);
        }
    }

    /// The card to show; `None` when off, unknown, never read, or dropped.
    fn atr(&self) -> Option<Vec<u8>> {
        if !self.enabled() || self.dropped.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        if self.cache.lock().ok()?.is_empty() {
            return None;
        }
        let atr = self.atr.lock().ok()?.clone();
        (!atr.is_empty()).then_some(atr)
    }

    /// Sets the login card's ATR; a different card starts a fresh cache.
    pub fn set_atr(&self, atr: Vec<u8>) {
        let Ok(mut slot) = self.atr.lock() else {
            return;
        };
        if *slot != atr && !slot.is_empty() {
            if let Ok(mut cache) = self.cache.lock() {
                *cache = ApduCache::default();
            }
            self.save_cache();
        }
        *slot = atr;
    }

    fn is_login_card(&self, atr: &[u8]) -> bool {
        self.enabled()
            && self
                .atr
                .lock()
                .is_ok_and(|slot| !slot.is_empty() && *slot == atr)
    }

    /// A real card is in: undoes a drop.
    fn saw_card(&self) {
        self.dropped
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn drop_card(&self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn save_cache(&self) {
        let path = self.cache_path.lock().ok().and_then(|path| path.clone());
        let text = self.cache.lock().map(|cache| cache.render());
        if let (Some(path), Ok(text)) = (path, text) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// The app's always-present card (the IFD server and Settings share it).
pub static ALWAYS_PRESENT: std::sync::LazyLock<AlwaysPresent> =
    std::sync::LazyLock::new(AlwaysPresent::default);

pub fn always_present_saved(state_dir: &std::path::Path) -> bool {
    state_dir.join(ALWAYS_PRESENT_FILE).is_file()
}

pub fn always_present_save(state_dir: &std::path::Path, enabled: bool) -> io::Result<()> {
    let path = state_dir.join(ALWAYS_PRESENT_FILE);
    if enabled {
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(path, b"")
    } else {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// Serve loop body: dispatch one decoded request against the running
/// service. Pure (given the handle + wake callback) so tests drive it
/// directly with a fake service... the ServiceHandle is the real one; tests
/// start a real service against a temp dir instead.
pub fn handle_request(
    request: &Request,
    service: &ServiceHandle,
    mut wake: impl FnMut(),
    limiter: &WakeLimiter,
    always: &AlwaysPresent,
) -> Vec<u8> {
    let status = service.status();
    if status.card_present {
        always.saw_card();
    }
    match request {
        Request::Presence => {
            let present = status.card_present || always.atr().is_some();
            respond(
                if present {
                    ifd_proto::OK
                } else {
                    ifd_proto::E_NO_CARD
                },
                &[],
            )
        }
        Request::Atr | Request::PowerOn => {
            if status.card_present {
                respond(ifd_proto::OK, &status.atr)
            } else if let Some(atr) = always.atr() {
                // Shown without a tap; the first APDU asks for it.
                respond(ifd_proto::OK, &atr)
            } else {
                // Trigger a wake: PC/SC (or OpenSC) wants the card and no
                // session/card is live.
                maybe_wake(&status, &mut wake, limiter);
                respond(ifd_proto::E_NO_CARD, &[])
            }
        }
        Request::PowerOff => respond(ifd_proto::OK, &[]),
        Request::Transmit(apdu) => transmit(apdu, status, service, &mut wake, always),
    }
}

fn transmit(
    apdu: &[u8],
    status: Status,
    service: &ServiceHandle,
    wake: &mut impl FnMut(),
    always: &AlwaysPresent,
) -> Vec<u8> {
    let virtual_card = !status.card_present && always.atr().is_some();
    if virtual_card {
        let cached = always
            .cache
            .lock()
            .map_err(|_| ())
            .and_then(|mut c| c.answer(apdu));
        if let Ok(response) = cached {
            return respond(ifd_proto::OK, &response);
        }
        // Not rate-limited: each prompt is a separate ask, and the wait
        // already spaces them out.
        wake();
        if !wait_for_card(service, VIRTUAL_WAIT) {
            crate::logging::log("ifd: no tap for the always-present card; reporting it gone");
            always.drop_card();
            return respond(ifd_proto::E_NO_CARD, &[]);
        }
    }
    let status = service.status();
    let login_card = status.card_present && always.is_login_card(&status.atr);
    if login_card {
        replay_select(apdu, &status, service, always);
    }
    match service.piv_transmit(apdu.to_vec()) {
        Ok(rapdu) => {
            if login_card {
                let learned = always.cache.lock().is_ok_and(|mut c| c.learn(apdu, &rapdu));
                if learned {
                    always.save_cache();
                }
            }
            respond(ifd_proto::OK, &rapdu)
        }
        Err(error) => respond(error.status(), &[]),
    }
}

/// A fresh tap is a freshly powered card with no applet selected. If the
/// cache answered the SELECT, send it to the card before the first real
/// command of this tap.
fn replay_select(apdu: &[u8], status: &Status, service: &ServiceHandle, always: &AlwaysPresent) {
    let Ok(mut selected) = always.selected.lock() else {
        return;
    };
    if *selected == Some(status.card_generation) {
        return;
    }
    *selected = Some(status.card_generation);
    if apdu.get(1) == Some(&0xA4) {
        return; // This command is a SELECT itself.
    }
    let select = always
        .cache
        .lock()
        .ok()
        .and_then(|cache| cache.last_select.clone());
    if let Some(select) = select {
        let _ = service.piv_transmit(select);
    }
}

/// Waits up to `limit` for a card to be tapped.
fn wait_for_card(service: &ServiceHandle, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if service.status().card_present {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Fires the wake callback (rate-limited) when no session is live and the
/// phone is wakeable, mirroring the tray's `request_tap` outcome logic.
fn maybe_wake(status: &Status, wake: &mut impl FnMut(), limiter: &WakeLimiter) {
    if !status.phone_connected && limiter.allowed() {
        wake();
    }
}

fn respond(status: u8, data: &[u8]) -> Vec<u8> {
    ifd_proto::encode_response(status, data)
}

/// Reads one length-prefixed request from the stream (u32 BE + body).
fn read_request(stream: &mut UnixStream) -> io::Result<Request> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > ifd_proto::MAX_BODY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad request length",
        ));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    ifd_proto::decode_request(&body)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad request body"))
}

/// Writes one length-prefixed response.
fn write_response(stream: &mut UnixStream, response: &[u8]) -> io::Result<()> {
    stream.write_all(response)?;
    stream.flush()
}

/// Accepts shim connections forever (each connection is served serially:
/// pcscd serializes reader access per driver).
pub fn serve(
    listener: UnixListener,
    service: ServiceHandle,
    wake: impl Fn() + Send + Sync + 'static,
    always: &'static AlwaysPresent,
) {
    let wake = std::sync::Arc::new(wake);
    let limiter = std::sync::Arc::new(WakeLimiter::default());
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        while let Ok(request) = read_request(&mut stream) {
            let response = {
                let wake = wake.clone();
                let limiter = limiter.clone();
                handle_request(&request, &service, move || wake(), &limiter, always)
            };
            if write_response(&mut stream, &response).is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apdu_cache_replays_reads_and_chains_but_not_signing() {
        let select = [0x00, 0xA4, 0x04, 0x00, 0x05, 0xA0, 0x00, 0x00, 0x03, 0x08];
        let get_cert = [
            0x00, 0xCB, 0x3F, 0xFF, 0x05, 0x5C, 0x03, 0x5F, 0xC1, 0x05, 0x00,
        ];
        let more = [0x00, 0xC0, 0x00, 0x00, 0x00];
        let sign = [0x00, 0x87, 0x11, 0x9A, 0x02, 0x7C, 0x00];
        let mut cache = ApduCache::default();
        assert!(cache.learn(&select, &[0x61, 0x11]));
        assert!(cache.learn(&get_cert, &[1, 2, 0x61, 0x10]));
        assert!(cache.learn(&more, &[3, 4, 0x61, 0x02]));
        assert!(cache.learn(&more, &[5, 0x90, 0x00]));
        assert!(
            !cache.learn(&sign, &[9, 0x90, 0x00]),
            "signing is never cached"
        );
        assert!(
            !cache.learn(&select, &[0x6F, 0x00]),
            "errors are not cached"
        );

        // Saved and loaded again.
        let mut cache = ApduCache::parse(&cache.render());
        assert_eq!(cache.answer(&select), Ok(vec![0x61, 0x11]));
        assert_eq!(cache.answer(&get_cert), Ok(vec![1, 2, 0x61, 0x10]));
        assert_eq!(cache.answer(&more), Ok(vec![3, 4, 0x61, 0x02]));
        assert_eq!(cache.answer(&more), Ok(vec![5, 0x90, 0x00]));
        assert_eq!(cache.answer(&more), Err(()), "past the end of the chain");
        assert_eq!(cache.answer(&sign), Err(()));
        assert_eq!(cache.answer(&more), Err(()), "no chain after signing");
        assert_eq!(cache.last_select.as_deref(), Some(&select[..]));
    }

    #[test]
    fn always_present_needs_a_card_read_and_drops_until_a_tap() {
        let always = AlwaysPresent::default();
        always.set_atr(vec![0x3B, 0x80]);
        assert_eq!(always.atr(), None, "off by default");
        always.set_enabled(true);
        assert_eq!(always.atr(), None, "nothing learned from a real tap yet");
        always
            .cache
            .lock()
            .unwrap()
            .learn(&[0x00, 0xA4, 0x04, 0x00], &[0x90, 0x00]);
        assert_eq!(always.atr(), Some(vec![0x3B, 0x80]));
        always.drop_card();
        assert_eq!(always.atr(), None);
        always.saw_card();
        assert_eq!(always.atr(), Some(vec![0x3B, 0x80]));
        always.set_atr(vec![0x3B, 0x81]);
        assert_eq!(always.atr(), None, "another card starts a fresh cache");
    }

    #[test]
    fn wake_limiter_allows_one_per_interval() {
        let limiter = WakeLimiter::default();
        assert!(limiter.allowed());
        assert!(!limiter.allowed());
    }

    #[test]
    fn launch_agent_plist_names_the_label_and_exe() {
        let plist = launch_agent_plist("/usr/local/bin/tapbridge");
        assert!(plist.contains("<string>computer.ethernet.tapbridge</string>"));
        assert!(plist.contains("<string>/usr/local/bin/tapbridge</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn plist_validates_with_plutil_when_available() {
        // Skips silently when plutil is missing (it ships with macOS).
        let Ok(mut output) = std::process::Command::new("plutil")
            .args(["-lint", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return;
        };
        let mut stdin = output.stdin.take().unwrap();
        use std::io::Write as _;
        let _ = stdin.write_all(launch_agent_plist("/usr/local/bin/tapbridge").as_bytes());
        drop(stdin); // plutil parses stdin at EOF; EOF comes from dropping it
        let Ok(finished) = output.wait_with_output() else {
            return;
        };
        assert!(
            finished.status.success(),
            "plutil rejected the LaunchAgent plist: {}",
            String::from_utf8_lossy(&finished.stdout)
        );
    }

    /// End-to-end IFD socket path against a real host service with a fake
    /// phone session (mirrors the service.rs test harness).
    mod ifd_path {
        use super::super::*;
        use iroh::{Endpoint, SecretKey};
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use tapbridge_core::{ALPN, transport, wire, wire::Frame};
        use tapbridge_host::service;

        async fn start_service(dir: &std::path::Path) -> service::ServiceHandle {
            service::start(service::Settings {
                state_dir: dir.to_path_buf(),
                usbip_addr: "127.0.0.1:0".parse().unwrap(),
                host_name: "Test Mac".into(),
                trace: false,
            })
            .await
            .unwrap()
        }

        /// Connects + pairs a fake phone; returns its hello-stream halves so
        /// the test can keep writing frames on the same bi stream (the
        /// service only reads this one).
        async fn connect_phone(
            handle: &service::ServiceHandle,
            name: &str,
            push: Option<&str>,
        ) -> (
            Endpoint,
            iroh::endpoint::Connection,
            iroh::endpoint::SendStream,
            iroh::endpoint::RecvStream,
        ) {
            let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
                .secret_key(SecretKey::generate())
                .alpns(vec![ALPN.to_vec()])
                .bind()
                .await
                .unwrap();
            let host_addr = handle.endpoint_addr().unwrap();
            let invitation = handle.mint_invitation().unwrap();
            let conn = endpoint.connect(host_addr, ALPN).await.unwrap();
            let (mut send, mut recv) = conn.open_bi().await.unwrap();
            let reply = transport::client_hello(
                &mut send,
                &mut recv,
                transport::ClientHello {
                    capability: Some(invitation.capability()),
                    name: name.into(),
                    push: push.map(str::to_owned),
                },
            )
            .await
            .unwrap();
            assert!(matches!(reply, transport::HelloReply::Accepted { .. }));
            (endpoint, conn, send, recv)
        }

        async fn wait_for(handle: &service::ServiceHandle, what: &str) {
            for _ in 0..400 {
                let status = handle.status();
                let ok = match what {
                    "phone" => status.phone_connected,
                    "card" => status.card_present,
                    "nocard" => !status.card_present,
                    _ => unreachable!(),
                };
                if ok {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            panic!("never reached state {what}");
        }

        /// One request through a fresh socket connection to the serve loop.
        fn roundtrip(socket_path: &std::path::Path, request: &Request) -> (u8, Vec<u8>) {
            let mut stream = UnixStream::connect(socket_path).unwrap();
            stream
                .write_all(&tapbridge_host::ifd::encode_request(request))
                .unwrap();
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut body = vec![0u8; len];
            stream.read_exact(&mut body).unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Write);
            tapbridge_host::ifd::decode_response(&body).unwrap()
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn reader_reports_absent_then_card_and_apdu_flow() {
            // Isolate the host record key per test process: the keychain ACL
            // pins the creating binary's cdhash, and reading a foreign item
            // from an unsigned test binary would block on a UI prompt. The
            // guard deletes the item when the test ends so runs do not leak
            // items into the login keychain.
            struct KeychainItem;
            impl Drop for KeychainItem {
                fn drop(&mut self) {
                    // SAFETY: test teardown; no other thread reads the env.
                    if let Ok(service) = std::env::var("TAPBRIDGE_TEST_KEYCHAIN") {
                        let _ = std::process::Command::new("security")
                            .args([
                                "delete-generic-password",
                                "-s",
                                &format!("computer.ethernet.tapbridge.host-record#{service}"),
                            ])
                            .status();
                    }
                }
            }
            let _keychain_item = KeychainItem;
            // SAFETY: single-threaded setup before any worker threads spawn.
            unsafe {
                std::env::set_var(
                    "TAPBRIDGE_TEST_KEYCHAIN",
                    format!("e2e{}", std::process::id()),
                );
            }
            let dir = tempfile::TempDir::new().unwrap();
            let handle = start_service(dir.path()).await;

            // Reader present at launch with zero cards: presence = absent,
            // and a wake fires (rate-limited) because no session is live.
            let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let listener = listen_at(dir.path().join("ifd-test.sock"));
            let service = handle.clone();
            let wake_counter = wakes.clone();
            std::thread::spawn(move || {
                serve(
                    listener,
                    service,
                    move || {
                        wake_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    },
                    Box::leak(Box::default()),
                );
            });

            // No card yet.
            let (status, data) = roundtrip(&dir.path().join("ifd-test.sock"), &Request::Presence);
            assert_eq!(status, tapbridge_host::ifd::E_NO_CARD);
            assert!(data.is_empty());
            let (status, _) = roundtrip(&dir.path().join("ifd-test.sock"), &Request::PowerOn);
            assert_eq!(status, tapbridge_host::ifd::E_NO_CARD);
            // The wake fired for the power-on (absence) path.
            for _ in 0..200 {
                if wakes.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            assert!(wakes.load(std::sync::atomic::Ordering::SeqCst) > 0);

            // Pair a phone, present a card (valid T=1 ATR).
            let (endpoint, conn, mut send, mut recv) =
                connect_phone(&handle, "Pixel", Some("tok")).await;
            wait_for(&handle, "phone").await;
            let atr = vec![
                0x3B, 0x8A, 0x80, 0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0B,
            ];
            wire::write_frame(
                &mut send,
                &Frame::Card {
                    generation: 1,
                    atr: atr.clone(),
                    max_apdu: 512,
                    uid: vec![],
                },
            )
            .await
            .unwrap();
            wait_for(&handle, "card").await;

            // Presence + ATR + power-on now succeed.
            let (status, _) = roundtrip(&dir.path().join("ifd-test.sock"), &Request::Presence);
            assert_eq!(status, tapbridge_host::ifd::OK);
            let (status, data) = roundtrip(&dir.path().join("ifd-test.sock"), &Request::Atr);
            assert_eq!(status, tapbridge_host::ifd::OK);
            assert_eq!(data, atr);
            let (status, data) = roundtrip(&dir.path().join("ifd-test.sock"), &Request::PowerOn);
            assert_eq!(status, tapbridge_host::ifd::OK);
            assert_eq!(data, atr);

            // APDU: the phone answers with an R-APDU; the IFD path relays it.
            let apdu = vec![0x00, 0xA4, 0x04, 0x00, 0x02, 0x3F, 0x00];
            let socket_path = dir.path().join("ifd-test.sock");
            let relay = std::thread::spawn({
                let apdu = apdu.clone();
                move || roundtrip(&socket_path, &Request::Transmit(apdu))
            });
            let frame = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tapbridge_core::wire::read_frame(&mut recv),
            )
            .await
            .unwrap()
            .unwrap();
            match frame {
                Frame::Apdu {
                    id,
                    generation,
                    data,
                } => {
                    assert_eq!(data, apdu);
                    // Answer on the SAME stream the service reads.
                    wire::write_frame(
                        &mut send,
                        &Frame::Response {
                            id,
                            generation,
                            data: vec![0x90, 0x00],
                        },
                    )
                    .await
                    .unwrap();
                }
                other => panic!("expected Apdu frame, got {other:?}"),
            }
            let (status, rapdu) = relay.join().unwrap();
            assert_eq!(status, tapbridge_host::ifd::OK);
            assert_eq!(rapdu, vec![0x90, 0x00]);

            handle.shutdown();
            transport::close(&conn);
            drop(endpoint);
            let _ = dir.close();
        }
    }
}
