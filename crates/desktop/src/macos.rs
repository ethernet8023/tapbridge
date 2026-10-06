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
    sync::{Mutex, OnceLock},
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

/// Rate-limit state: last instant a PC/SC-triggered wake was sent.
#[derive(Default)]
struct WakeLimiter {
    last: Mutex<Option<Instant>>,
}

impl WakeLimiter {
    /// True when a wake may be sent now (records it).
    fn allowed(&self) -> bool {
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

/// Rate limiter for the production daemon (single global instance).
fn wake_allowed() -> bool {
    LAST_WAKE_DAEMON.get_or_init(WakeLimiter::default).allowed()
}

/// The daemon's wake limiter (exactly one per process).
static LAST_WAKE_DAEMON: OnceLock<WakeLimiter> = OnceLock::new();

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

/// Serve loop body: dispatch one decoded request against the running
/// service. Pure (given the handle + wake callback) so tests drive it
/// directly with a fake service... the ServiceHandle is the real one; tests
/// start a real service against a temp dir instead.
pub fn handle_request(
    request: &Request,
    service: &ServiceHandle,
    mut wake: impl FnMut(),
    limiter: &WakeLimiter,
) -> Vec<u8> {
    match request {
        Request::Presence => {
            let status = service.status();
            respond(
                if status.card_present {
                    ifd_proto::OK
                } else {
                    ifd_proto::E_NO_CARD
                },
                &[],
            )
        }
        Request::Atr => {
            let status = service.status();
            if status.card_present {
                respond(ifd_proto::OK, &status.atr)
            } else {
                // Trigger a wake: PC/SC (or OpenSC) wants the card and no
                // session/card is live.
                maybe_wake(&status, &mut wake, limiter);
                respond(ifd_proto::E_NO_CARD, &[])
            }
        }
        Request::PowerOn => {
            let status = service.status();
            if status.card_present {
                respond(ifd_proto::OK, &status.atr)
            } else {
                maybe_wake(&status, &mut wake, limiter);
                respond(ifd_proto::E_NO_CARD, &[])
            }
        }
        Request::PowerOff => respond(ifd_proto::OK, &[]),
        Request::Transmit(apdu) => match service.piv_transmit(apdu.clone()) {
            Ok(rapdu) => respond(ifd_proto::OK, &rapdu),
            Err(error) => respond(error.status(), &[]),
        },
    }
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
) {
    let wake = std::sync::Arc::new(wake);
    let limiter = std::sync::Arc::new(WakeLimiter::default());
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        loop {
            let request = match read_request(&mut stream) {
                Ok(request) => request,
                Err(_) => break,
            };
            let wake = wake.clone();
            let limiter = limiter.clone();
            let response = handle_request(&request, &service, move || wake(), &limiter);
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
                    unsafe {
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
                serve(listener, service, move || {
                    wake_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                });
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
