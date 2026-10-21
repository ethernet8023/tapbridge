//! Virtual FIDO security key (macOS and Linux): browsers see a USB security
//! key; every CTAP request is relayed to the card on the phone over the live
//! session. Keys never leave the card.
//!
//! Layers: [`tapbridge_host::ctaphid::Engine`] (HID framing),
//! [`run_request`] (wait for a card, wake the phone, relay with
//! [`tapbridge_host::ctap_nfc::relay`]), and [`hid`] (the virtual device:
//! IOKit on macOS, `/dev/uhid` on Linux). If the OS refuses the device
//! (macOS without Apple's restricted
//! `com.apple.developer.hid.virtual.device` entitlement; Linux without
//! access to `/dev/uhid`) it is simply not created and the rest of the app
//! runs as before.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tapbridge_host::{
    ctap_nfc::{self, RelayError},
    ctap_pin::proxy::{CardPort, PinProxy, PortError},
    ctaphid::{self, Engine, Report},
    service::ServiceHandle,
};

/// How long a request waits for a card on the phone before giving up.
const TAP_TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// CTAP status bytes the relay itself produces.
const CTAP2_ERR_KEEPALIVE_CANCEL: u8 = 0x2D;
const CTAP2_ERR_USER_ACTION_TIMEOUT: u8 = 0x2F;
#[cfg(test)]
const CTAP1_ERR_OTHER: u8 = 0x7F;

/// What the request worker needs from the outside world (a trait so the
/// waiting/relay rules are tested without a phone).
pub trait Card {
    /// The current tap's generation, None when no card is on the phone.
    /// Each tap is a new generation: the card powered up afresh.
    fn generation(&self) -> Option<u64>;
    /// Ask the phone for a tap (wake it or show "tap now").
    fn request_tap(&self);
    fn transmit(&self, apdu: Vec<u8>) -> Result<Vec<u8>, String>;
    /// Shows a sign-in state ([`tapbridge_core::wire::auth`]) on the
    /// phone's tap animation.
    fn auth_state(&self, state: u8);
}

/// Carries out one CTAP request through the PIN proxy: waits (up to
/// `timeout`) for a card, retries on a new tap when the card is lost.
/// `status` receives the keepalive status to report (UPNEEDED while
/// waiting for a tap). Always returns a CTAP reply (status byte first):
/// failures become CTAP errors the browser understands.
pub fn run_request(
    request: &[u8],
    cid: u32,
    proxy: &mut PinProxy,
    card: &dyn Card,
    cancelled: &AtomicBool,
    status: impl FnMut(u8),
    timeout: Duration,
) -> Vec<u8> {
    let mut port = RelayPort {
        card,
        cancelled,
        status: Box::new(status),
        deadline: Instant::now() + timeout,
        opened: None,
        lost: None,
    };
    proxy.handle(cid, request, &mut port, Instant::now())
}

/// The proxy's view of the card: tap waiting, SELECT and the generation
/// checks that turn a re-tap into a lost card.
struct RelayPort<'a> {
    card: &'a dyn Card,
    cancelled: &'a AtomicBool,
    status: Box<dyn FnMut(u8) + 'a>,
    deadline: Instant,
    /// Generation of the tap the applet was SELECTed on.
    opened: Option<u64>,
    /// Generation that was lost: the next open waits for a different one.
    lost: Option<u64>,
}

impl RelayPort<'_> {
    fn check_cancel(&self) -> Result<(), PortError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(PortError::Ended(CTAP2_ERR_KEEPALIVE_CANCEL));
        }
        Ok(())
    }

    /// A usable tap: present, and not the one that was lost.
    fn usable(&self) -> Option<u64> {
        self.card
            .generation()
            .filter(|generation| Some(*generation) != self.lost)
    }

    /// Waits for a usable tap, asking the phone for one first.
    fn wait_for_tap(&mut self) -> Result<u64, PortError> {
        if let Some(generation) = self.usable() {
            return Ok(generation);
        }
        self.card.request_tap();
        (self.status)(ctaphid::STATUS_UPNEEDED);
        loop {
            self.check_cancel()?;
            if let Some(generation) = self.usable() {
                (self.status)(ctaphid::STATUS_PROCESSING);
                return Ok(generation);
            }
            if Instant::now() >= self.deadline {
                return Err(PortError::Ended(CTAP2_ERR_USER_ACTION_TIMEOUT));
            }
            std::thread::sleep(POLL);
        }
    }

    /// Marks the opened tap lost (the next open waits for another).
    fn lose(&mut self) -> PortError {
        self.lost = self.opened.take().or(self.lost);
        PortError::Lost
    }
}

impl CardPort for RelayPort<'_> {
    fn open(&mut self, new_tap: bool) -> Result<u64, PortError> {
        self.check_cancel()?;
        if new_tap && self.lost.is_none() {
            self.lost = self.opened.or_else(|| self.card.generation());
        }
        let generation = self.wait_for_tap()?;
        self.opened = Some(generation);
        let card = self.card;
        match ctap_nfc::select(|apdu| card.transmit(apdu)) {
            Ok(()) if self.card.generation() == Some(generation) => Ok(generation),
            Ok(()) | Err(RelayError::Transport(_)) => Err(self.lose()),
            Err(_) if self.card.generation() != Some(generation) => Err(self.lose()),
            Err(_) => Err(PortError::Card),
        }
    }

    fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, PortError> {
        self.check_cancel()?;
        let Some(opened) = self.opened else {
            return Err(PortError::Lost);
        };
        let card = self.card;
        // Every APDU checks the tap first: a re-tap means a card that was
        // never SELECTed and has a new PIN key, so nothing may go to it.
        let mut sent = false;
        let result = ctap_nfc::exchange(request, |apdu| {
            if card.generation() != Some(opened) {
                return Err("card re-tapped".to_owned());
            }
            sent = true;
            card.transmit(apdu)
        });
        match result {
            Ok(reply) => Ok(reply),
            Err(RelayError::Transport(_)) if !sent => {
                self.lose();
                Err(PortError::NotSent)
            }
            Err(RelayError::Transport(_)) => Err(self.lose()),
            Err(_) if card.generation() != Some(opened) => Err(self.lose()),
            Err(_) => Err(PortError::Card),
        }
    }

    fn auth_state(&mut self, state: u8) {
        self.card.auth_state(state);
    }
}

/// The running service as a [`Card`].
struct ServiceCard {
    service: ServiceHandle,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Card for ServiceCard {
    fn generation(&self) -> Option<u64> {
        let status = self.service.status();
        status.card_present.then_some(status.card_generation)
    }
    fn request_tap(&self) {
        (self.wake)();
    }
    fn transmit(&self, apdu: Vec<u8>) -> Result<Vec<u8>, String> {
        self.service
            .ctap_transmit(apdu)
            .map_err(|error| format!("{error:?}"))
    }
    fn auth_state(&self, state: u8) {
        self.service.send_auth_state(state);
    }
}

/// The PIN proxy shared by the driver and its request workers. The
/// driver never blocks on the proxy (a worker may hold it for a whole
/// request): resyncs queue up and are applied on the next tick or
/// request.
#[derive(Default)]
struct Shared {
    proxy: Mutex<PinProxy>,
    resyncs: Mutex<Vec<u32>>,
}

impl Shared {
    /// CTAPHID INIT resynced `cid`: forget its cached PIN state.
    fn resync(&self, cid: u32) {
        if let Ok(mut resyncs) = self.resyncs.lock() {
            resyncs.push(cid);
        }
    }

    /// Applies queued resyncs and the idle timeout when no request holds
    /// the proxy.
    fn tick(&self, now: Instant) {
        if let Ok(mut proxy) = self.proxy.try_lock() {
            self.apply(&mut proxy, now);
        }
    }

    fn apply(&self, proxy: &mut PinProxy, now: Instant) {
        if let Ok(mut resyncs) = self.resyncs.lock() {
            for cid in resyncs.drain(..) {
                proxy.on_resync(cid);
            }
        }
        proxy.expire(now);
    }
}

enum Input {
    Report(Vec<u8>),
    /// The last program closed the device: whatever it was waiting for is
    /// moot, so a pending tap request must not keep the phone waiting.
    Released,
    Done {
        cid: u32,
        reply: Vec<u8>,
    },
    Status {
        cid: u32,
        status: u8,
    },
}

/// Creates the virtual security key and serves it on background threads.
/// `wake` asks the phone for a tap (the tray's request-tap path). Returns
/// an error (and changes nothing) when the OS refuses the device.
pub fn spawn(
    service: ServiceHandle,
    wake: impl Fn() + Send + Sync + 'static,
    log: impl Fn(&str) + Send + Sync + 'static,
) -> Result<(), String> {
    let card: Arc<dyn Card + Send + Sync> = Arc::new(ServiceCard {
        service,
        wake: Arc::new(wake),
    });
    spawn_with_card(card, Arc::new(log))
}

fn spawn_with_card(
    card: Arc<dyn Card + Send + Sync>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<(), String> {
    start(hid::Device::create, card, log)
}

/// [`spawn_with_card`] with the device factory as a parameter, so tests can
/// back the key with a fake kernel side instead of the real OS device.
fn start<D>(
    create: impl FnOnce(Box<dyn Fn(&[u8]) + Send>, Box<dyn Fn() + Send>) -> Result<D, String>,
    card: Arc<dyn Card + Send + Sync>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<(), String>
where
    D: Send + 'static + Sink,
{
    let (tx, rx) = mpsc::channel::<Input>();
    let reports = tx.clone();
    let released = tx.clone();
    let device = create(
        Box::new(move |report| {
            let _ = reports.send(Input::Report(report.to_vec()));
        }),
        Box::new(move || {
            let _ = released.send(Input::Released);
        }),
    )?;
    std::thread::Builder::new()
        .name("security-key".into())
        .spawn(move || drive(device, rx, tx, card, log))
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The part of a virtual device the driver loop uses.
trait Sink {
    fn send(&self, report: &[u8]);
}

/// The driver loop: owns the engine and the device; one worker thread per
/// CTAP request.
fn drive(
    device: impl Sink,
    rx: mpsc::Receiver<Input>,
    tx: mpsc::Sender<Input>,
    card: Arc<dyn Card + Send + Sync>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let mut engine = Engine::new();
    let shared = Arc::new(Shared::default());
    let mut cancel_flag: Option<Arc<AtomicBool>> = None;
    let send = |reports: Vec<Report>| {
        for report in reports {
            device.send(&report);
        }
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Input::Report(report)) => {
                let out = engine.on_report(&report, Instant::now());
                send(out.reports);
                if let Some(cid) = out.resync {
                    shared.resync(cid);
                }
                if out.cancel
                    && let Some(flag) = cancel_flag.take()
                {
                    flag.store(true, Ordering::SeqCst);
                    if let Some(cid) = engine.busy_cid() {
                        // CANCEL: answer now; the worker's late reply is
                        // dropped by the engine.
                        send(engine.on_response(cid, &[CTAP2_ERR_KEEPALIVE_CANCEL]));
                    }
                }
                if let Some(request) = out.start {
                    // Command byte and length only: payloads carry PIN
                    // material.
                    log(&format!(
                        "security key: CTAP2 command 0x{:02x} ({} bytes)",
                        request.payload.first().copied().unwrap_or(0),
                        request.payload.len()
                    ));
                    let flag = Arc::new(AtomicBool::new(false));
                    cancel_flag = Some(flag.clone());
                    let card = card.clone();
                    let shared = shared.clone();
                    let tx = tx.clone();
                    let cid = request.cid;
                    std::thread::spawn(move || {
                        let statuses = tx.clone();
                        let Ok(mut proxy) = shared.proxy.lock() else {
                            let _ = tx.send(Input::Done {
                                cid,
                                reply: vec![0x7F],
                            });
                            return;
                        };
                        shared.apply(&mut proxy, Instant::now());
                        let reply = run_request(
                            &request.payload,
                            cid,
                            &mut proxy,
                            card.as_ref(),
                            &flag,
                            |status| {
                                let _ = statuses.send(Input::Status { cid, status });
                            },
                            TAP_TIMEOUT,
                        );
                        drop(proxy);
                        let _ = tx.send(Input::Done { cid, reply });
                    });
                }
            }
            Ok(Input::Released) => {
                if let Some(flag) = cancel_flag.take() {
                    log("security key: device closed, cancelling the pending request");
                    flag.store(true, Ordering::SeqCst);
                }
            }
            Ok(Input::Status { cid, status }) => {
                if engine.busy_cid() == Some(cid) {
                    engine.set_status(status);
                }
            }
            Ok(Input::Done { cid, reply }) => {
                log(&format!(
                    "security key: reply status 0x{:02x} ({} bytes)",
                    reply.first().copied().unwrap_or(0),
                    reply.len()
                ));
                if engine.busy_cid() == Some(cid) {
                    cancel_flag = None;
                }
                send(engine.on_response(cid, &reply));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        shared.tick(Instant::now());
        send(engine.on_tick(Instant::now()));
    }
}

#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "linux")]
mod uhid;
#[cfg(target_os = "macos")]
use iokit as hid;
#[cfg(target_os = "linux")]
use uhid as hid;

#[cfg(test)]
mod tests {
    use super::*;
    use tapbridge_core::wire::auth;
    use tapbridge_host::ctap_pin::fake::{Authenticator, Platform};
    use tapbridge_host::ctap_pin::protocol::Protocol;

    /// Live check: creates the real virtual device and finds it in the IO
    /// registry. Needs the entitlement (sign the test binary with
    /// packaging/macos/tapbridge.entitlements; AMFI off or a profile).
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "needs the HID virtual device entitlement"]
    fn virtual_device_registers_with_iokit() {
        let device =
            hid::Device::create(|_| {}, || {}).expect("device created (entitlement present?)");
        let listing = std::process::Command::new("ioreg")
            .args(["-r", "-c", "IOHIDUserDevice", "-l"])
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(listing.contains("tapbridge security key"), "{listing}");
        assert!(
            listing.contains("\"PrimaryUsagePage\" = 61904"),
            "FIDO usage page 0xF1D0"
        );
        drop(device);
    }

    /// HID paths of "tapbridge security key" devices that already exist
    /// (an installed tapbridge app has one): the live tests skip them and
    /// talk only to the device they created.
    fn existing_key_paths(python: &str) -> String {
        let script = "from fido2.hid import CtapHidDevice\nprint(','.join(str(d.descriptor.path) for d in CtapHidDevice.list_devices() if d.descriptor.product_name == 'tapbridge security key'))";
        let output = std::process::Command::new(python)
            .args(["-c", script])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Live end to end: a real FIDO client (python-fido2, path in
    /// `FIDO_PYTHON`) finds the virtual key over HID and runs GetInfo; the
    /// fake card answers with a 100+ byte reply (multi-packet both ways).
    #[test]
    #[ignore = "needs the HID virtual device entitlement and FIDO_PYTHON"]
    fn real_fido_client_gets_info_through_the_virtual_key() {
        // {1: ["FIDO_2_0", "FIDO_2_1"], 2: ["credProtect", "hmac-secret",
        //  "largeBlobKey"], 3: aaguid(16 x 0x42)} + padding via key 5.
        let mut info = vec![0x00, 0xA4, 0x01, 0x82, 0x68];
        info.extend_from_slice(b"FIDO_2_0");
        info.push(0x68);
        info.extend_from_slice(b"FIDO_2_1");
        info.extend_from_slice(&[0x02, 0x83, 0x6B]);
        info.extend_from_slice(b"credProtect");
        info.push(0x6B);
        info.extend_from_slice(b"hmac-secret");
        info.push(0x6C);
        info.extend_from_slice(b"largeBlobKey");
        info.extend_from_slice(&[0x03, 0x50]);
        info.extend_from_slice(&[0x42; 16]);
        info.extend_from_slice(&[0x05, 0x19, 0x04, 0x00]); // maxMsgSize 1024
        struct InfoCard(Vec<u8>);
        impl Card for InfoCard {
            fn generation(&self) -> Option<u64> {
                Some(1)
            }
            fn request_tap(&self) {}
            fn auth_state(&self, _: u8) {}
            fn transmit(&self, apdu: Vec<u8>) -> Result<Vec<u8>, String> {
                let mut reply = if apdu[1] == 0xA4 {
                    b"FIDO_2_0".to_vec()
                } else {
                    self.0.clone()
                };
                reply.extend_from_slice(&[0x90, 0x00]);
                Ok(reply)
            }
        }
        let python = std::env::var("FIDO_PYTHON").expect("FIDO_PYTHON=path to python with fido2");
        let skip = existing_key_paths(&python);
        spawn_with_card(
            Arc::new(InfoCard(info)),
            Arc::new(|line: &str| eprintln!("{line}")),
        )
        .expect("device created (entitlement present?)");
        std::thread::sleep(Duration::from_millis(500));
        let script = r#"
from fido2.hid import CtapHidDevice
from fido2.ctap2 import Ctap2
import os
skip = set(os.environ.get("TAPBRIDGE_SKIP_PATHS", "").split(","))
dev = next(d for d in CtapHidDevice.list_devices() if d.descriptor.product_name == "tapbridge security key" and str(d.descriptor.path) not in skip)
info = Ctap2(dev).get_info()
print("versions", info.versions)
print("extensions", info.extensions)
print("aaguid", info.aaguid.hex())
print("max_msg_size", info.max_msg_size)
"#;
        let output = std::process::Command::new(python)
            .args(["-c", script])
            .env("TAPBRIDGE_SKIP_PATHS", &skip)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("{stdout}{stderr}");
        assert!(output.status.success());
        assert!(stdout.contains("FIDO_2_1"));
        assert!(stdout.contains("largeBlobKey"));
        assert!(stdout.contains(&"42".repeat(16)));
        assert!(stdout.contains("max_msg_size 1024"));
    }

    /// The phone around the software authenticator, at the APDU level.
    /// Taps power-cycle the card and bump the generation; `lose_at` makes
    /// the n-th APDU fail (the card left mid-request) and leaves the
    /// phone without a card until the next `request_tap`.
    pub(super) struct SimCard {
        inner: Mutex<Sim>,
    }

    pub(super) struct Sim {
        card: Authenticator,
        generation: u64,
        pub(super) present: bool,
        apdus: usize,
        lose_at: Vec<usize>,
        /// Whether a requested tap actually happens (the user taps).
        pub(super) user_taps: bool,
        /// Silent re-tap right after the n-th APDU was answered.
        retap_after: Vec<usize>,
        /// A lost card keeps showing as present (same generation) until
        /// the next real tap: the stale flag of the live failure.
        stale_presence: bool,
        stale_presence_on_loss: bool,
        pub(super) taps: usize,
        retaps: usize,
        states: Vec<u8>,
    }

    impl SimCard {
        pub(super) fn new(card: Authenticator) -> Self {
            Self {
                inner: Mutex::new(Sim {
                    card,
                    generation: 1,
                    present: true,
                    apdus: 0,
                    lose_at: Vec::new(),
                    user_taps: true,
                    retap_after: Vec::new(),
                    stale_presence: false,
                    stale_presence_on_loss: false,
                    taps: 0,
                    retaps: 0,
                    states: Vec::new(),
                }),
            }
        }

        pub(super) fn sim(&self) -> std::sync::MutexGuard<'_, Sim> {
            self.inner.lock().unwrap()
        }

        /// The user lifts the card and taps it again.
        fn retap(&self) {
            let mut sim = self.sim();
            sim.card.power_cycle();
            sim.generation += 1;
            sim.present = true;
        }
    }

    impl Card for SimCard {
        fn generation(&self) -> Option<u64> {
            let sim = self.sim();
            sim.present.then_some(sim.generation)
        }
        fn request_tap(&self) {
            let mut sim = self.sim();
            sim.taps += 1;
            if sim.user_taps && (!sim.present || sim.stale_presence) {
                sim.stale_presence = false;
                sim.card.power_cycle();
                sim.generation += 1;
                sim.present = true;
            }
        }
        fn transmit(&self, apdu: Vec<u8>) -> Result<Vec<u8>, String> {
            let mut sim = self.sim();
            sim.apdus += 1;
            let n = sim.apdus;
            if !sim.present || sim.stale_presence {
                return Err("no card".into());
            }
            if sim.lose_at.contains(&n) {
                sim.card.apdu(&apdu);
                sim.card.power_cycle();
                sim.present = false;
                if sim.stale_presence_on_loss {
                    sim.present = true;
                    sim.stale_presence = true;
                }
                return Err("card gone".into());
            }
            let reply = sim.card.apdu(&apdu);
            if sim.retap_after.contains(&n) {
                sim.card.power_cycle();
                sim.generation += 1;
            }
            Ok(reply)
        }
        fn auth_state(&self, state: u8) {
            self.sim().states.push(state);
        }
    }

    /// Runs one request against `card` with a fresh status log.
    fn run(proxy: &mut PinProxy, card: &SimCard, request: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut statuses = Vec::new();
        let reply = run_request(
            request,
            7,
            proxy,
            card,
            &AtomicBool::new(false),
            |status| statuses.push(status),
            TAP_TIMEOUT,
        );
        (reply, statuses)
    }

    const PIN: &str = "1234";

    /// Live PIN flow: python-fido2 (`FIDO_PYTHON`) runs ClientPin over the
    /// virtual key against the software card, which is re-tapped (power
    /// cycled, new generation) before every card session, then a wrong
    /// PIN. Needs the entitlement like the tests above.
    #[test]
    #[ignore = "needs the HID virtual device entitlement and FIDO_PYTHON"]
    fn real_fido_client_pin_flow_survives_retaps() {
        /// Re-taps (power cycle, new generation) whenever the card sat
        /// idle for a while: the user lifting and re-tapping between the
        /// client's requests (the script sleeps between steps).
        struct RetapWhenIdle(SimCard, Mutex<Instant>);
        impl RetapWhenIdle {
            fn maybe_retap(&self) {
                let mut last = self.1.lock().unwrap();
                if last.elapsed() > Duration::from_millis(150) {
                    self.0.retap();
                    self.0.sim().retaps += 1;
                }
                *last = Instant::now();
            }
        }
        impl Card for RetapWhenIdle {
            fn generation(&self) -> Option<u64> {
                self.maybe_retap();
                self.0.generation()
            }
            fn request_tap(&self) {
                self.0.request_tap();
            }
            fn transmit(&self, apdu: Vec<u8>) -> Result<Vec<u8>, String> {
                *self.1.lock().unwrap() = Instant::now();
                self.0.transmit(apdu)
            }
            fn auth_state(&self, state: u8) {
                self.0.auth_state(state);
            }
        }
        let card = Arc::new(RetapWhenIdle(
            SimCard::new(Authenticator::new(Some(PIN), &[1, 2])),
            Mutex::new(Instant::now()),
        ));
        let python = std::env::var("FIDO_PYTHON").expect("FIDO_PYTHON=path to python with fido2");
        let skip = existing_key_paths(&python);
        spawn_with_card(card.clone(), Arc::new(|line: &str| eprintln!("{line}")))
            .expect("device created (entitlement present?)");
        std::thread::sleep(Duration::from_millis(500));
        let script = r#"
from fido2.hid import CtapHidDevice
from fido2.ctap import CtapError
from fido2.ctap2 import Ctap2, ClientPin
import time
import os
skip = set(os.environ.get("TAPBRIDGE_SKIP_PATHS", "").split(","))
dev = next(d for d in CtapHidDevice.list_devices() if d.descriptor.product_name == "tapbridge security key" and str(d.descriptor.path) not in skip)
ctap = Ctap2(dev)
print("protocols", ctap.info.pin_uv_protocols)
time.sleep(0.3)
pin = ClientPin(ctap)
print("retries", pin.get_pin_retries()[0])
time.sleep(0.3)
token = pin.get_pin_token("1234", ClientPin.PERMISSION.MAKE_CREDENTIAL, "example.com")
cdh = b"\x01" * 32
time.sleep(0.3)
att = ctap.make_credential(cdh, {"id": "example.com", "name": "x"}, {"id": b"u", "name": "u"},
    [{"type": "public-key", "alg": -7}], pin_uv_param=pin.protocol.authenticate(token, cdh),
    pin_uv_protocol=pin.protocol.VERSION)
print("make_credential", att.fmt)
time.sleep(0.3)
try:
    pin.get_pin_token("0000")
except CtapError as e:
    print("wrong pin", e.code.name)
print("retries after", pin.get_pin_retries()[0])
"#;
        let output = std::process::Command::new(python)
            .args(["-c", script])
            .env("TAPBRIDGE_SKIP_PATHS", &skip)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("{stdout}{stderr}");
        assert!(output.status.success());
        assert!(stdout.contains("protocols [2, 1]"));
        assert!(stdout.contains("make_credential none"));
        assert!(stdout.contains("wrong pin PIN_INVALID"));
        assert!(stdout.contains("retries after 7"));
        let sim = card.0.sim();
        assert_eq!(sim.card.counters.wrong_pin, 1);
        assert_eq!(sim.card.counters.signatures, 1);
        assert!(sim.retaps >= 4, "re-tapped between steps: {}", sim.retaps);
    }

    #[test]
    fn card_already_present_relays_without_asking_for_a_tap() {
        let card = SimCard::new(Authenticator::new(None, &[2]));
        let (reply, statuses) = run(&mut PinProxy::new(), &card, &[0x04]);
        assert_eq!(reply[0], 0x00);
        assert_eq!(card.sim().taps, 0);
        assert!(statuses.is_empty());
        assert_eq!(card.sim().apdus, 2, "SELECT + MSG");
    }

    #[test]
    fn no_card_asks_for_one_tap_then_relays_when_it_arrives() {
        let card = SimCard::new(Authenticator::new(None, &[2]));
        card.sim().present = false;
        let (reply, statuses) = run(&mut PinProxy::new(), &card, &[0x04]);
        assert_eq!(reply[0], 0x00);
        assert_eq!(card.sim().taps, 1);
        assert_eq!(
            statuses,
            [ctaphid::STATUS_UPNEEDED, ctaphid::STATUS_PROCESSING]
        );
    }

    #[test]
    fn waiting_times_out_with_user_action_timeout() {
        let card = SimCard::new(Authenticator::new(None, &[2]));
        card.sim().present = false;
        card.sim().user_taps = false;
        let reply = run_request(
            &[0x04],
            7,
            &mut PinProxy::new(),
            &card,
            &AtomicBool::new(false),
            |_| {},
            Duration::from_millis(250),
        );
        assert_eq!(reply, [CTAP2_ERR_USER_ACTION_TIMEOUT]);
        assert_eq!(card.sim().states.last(), Some(&auth::FAILED));
    }

    #[test]
    fn cancel_while_waiting_returns_keepalive_cancel() {
        let card = SimCard::new(Authenticator::new(None, &[2]));
        card.sim().present = false;
        card.sim().user_taps = false;
        let reply = run_request(
            &[0x04],
            7,
            &mut PinProxy::new(),
            &card,
            &AtomicBool::new(true),
            |_| {},
            TAP_TIMEOUT,
        );
        assert_eq!(reply, [CTAP2_ERR_KEEPALIVE_CANCEL]);
    }

    #[test]
    fn a_card_without_the_fido_applet_is_ctap_other() {
        struct NoApplet;
        impl Card for NoApplet {
            fn generation(&self) -> Option<u64> {
                Some(1)
            }
            fn request_tap(&self) {}
            fn transmit(&self, _: Vec<u8>) -> Result<Vec<u8>, String> {
                Ok(vec![0x6A, 0x82])
            }
            fn auth_state(&self, _: u8) {}
        }
        let reply = run_request(
            &[0x04],
            7,
            &mut PinProxy::new(),
            &NoApplet,
            &AtomicBool::new(false),
            |_| {},
            TAP_TIMEOUT,
        );
        assert_eq!(reply, [CTAP1_ERR_OTHER]);
    }

    /// The live failure, end to end at the APDU level: GetInfo, the
    /// probe, getRetries, getKeyAgreement, the user types the PIN while
    /// the card leaves and comes back, getPinToken, another re-tap,
    /// makeCredential.
    #[test]
    fn safari_pin_sequence_survives_retaps_through_the_relay() {
        let card = SimCard::new(Authenticator::new(Some(PIN), &[1, 2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let cdh = [0x42u8; 32];
        assert_eq!(run(&mut proxy, &card, &[0x04]).0[0], 0x00);
        let probe = platform.make_credential(&cdh, "webauthn.io", Some(vec![]));
        assert_eq!(run(&mut proxy, &card, &probe).0, [0x31]);
        assert_eq!(
            run(&mut proxy, &card, &platform.client_pin(0x01, vec![])).0[0],
            0x00
        );
        let mut send = |request: &[u8]| run(&mut proxy, &card, request).0;
        let (shared, cose) = platform.agree(&mut send).unwrap();
        card.retap();
        let request = platform.token_request(&shared, cose, PIN, None);
        let token = platform
            .open_token(&shared, &run(&mut proxy, &card, &request).0)
            .unwrap();
        card.retap();
        let param = Protocol::Two.authenticate(&token, &cdh);
        let request = platform.make_credential(&cdh, "webauthn.io", Some(param));
        assert_eq!(run(&mut proxy, &card, &request).0[0], 0x00);
        let sim = card.sim();
        assert_eq!(sim.card.counters.wrong_pin, 0);
        assert_eq!(sim.card.counters.signatures, 1);
        assert_eq!(sim.states.last(), Some(&auth::SUCCESS));
        assert!(!sim.states.contains(&auth::FAILED));
    }

    #[test]
    fn a_card_lost_mid_request_is_retried_after_a_new_tap() {
        let card = SimCard::new(Authenticator::new(None, &[2]));
        card.sim().lose_at = vec![2]; // the makeCredential APDU
        let platform = Platform::new(Protocol::Two);
        let request = platform.make_credential(&[1; 32], "example.com", None);
        let (reply, statuses) = run(&mut PinProxy::new(), &card, &request);
        assert_eq!(reply[0], 0x00);
        let sim = card.sim();
        assert_eq!(sim.taps, 1, "the phone was asked for a tap");
        assert_eq!(
            sim.states,
            [auth::WORKING, auth::REMOVED, auth::WORKING, auth::SUCCESS]
        );
        assert_eq!(
            statuses,
            [ctaphid::STATUS_UPNEEDED, ctaphid::STATUS_PROCESSING]
        );
    }

    #[test]
    fn a_generation_change_mid_sequence_counts_as_a_lost_card() {
        let card = SimCard::new(Authenticator::new(None, &[2]));
        // The card is re-tapped between SELECT and the command: the new
        // tap was never SELECTed, so the proxy must reopen it.
        card.sim().retap_after = vec![1];
        let (reply, _) = run(&mut PinProxy::new(), &card, &[0x04]);
        assert_eq!(reply[0], 0x00);
        assert!(card.sim().states.contains(&auth::REMOVED));
    }

    /// A re-tap between the card's getKeyAgreement and its getPinToken,
    /// on a card that needs no SELECT after power-up: the pinHash must
    /// not go out encrypted to the old tap's key (the card would count a
    /// wrong PIN). Caught before sending, so it is retried safely.
    #[test]
    fn a_retap_before_get_pin_token_never_sends_a_stale_pin_hash() {
        let mut authenticator = Authenticator::new(Some(PIN), &[2]);
        authenticator.auto_select = true;
        let card = SimCard::new(authenticator);
        let mut proxy = PinProxy::new();
        assert_eq!(run(&mut proxy, &card, &[0x04]).0[0], 0x00); // APDUs 1-2
        // getPinToken: SELECT (3), card getKeyAgreement (4), re-tap,
        // getPinToken (5).
        card.sim().retap_after = vec![4];
        let platform = Platform::new(Protocol::Two);
        let mut send = |request: &[u8]| run(&mut proxy, &card, request).0;
        let token = platform.token(&mut send, PIN, None);
        assert!(token.is_ok(), "{token:?}");
        assert_eq!(card.sim().card.counters.wrong_pin, 0);
    }

    /// The live failure: the transport says the card is gone but the
    /// presence flag still shows the old tap. The relay must not reuse
    /// that tap; it asks the phone for a new one.
    #[test]
    fn a_stale_presence_flag_after_a_loss_waits_for_a_real_retap() {
        let card = SimCard::new(Authenticator::new(None, &[2]));
        card.sim().lose_at = vec![2];
        card.sim().stale_presence_on_loss = true;
        let (reply, _) = run(&mut PinProxy::new(), &card, &[0x04]);
        assert_eq!(reply[0], 0x00);
        assert_eq!(card.sim().taps, 1);
    }

    #[test]
    fn a_lost_card_that_never_comes_back_times_out() {
        let card = SimCard::new(Authenticator::new(None, &[2]));
        card.sim().lose_at = vec![2];
        card.sim().user_taps = false;
        let reply = run_request(
            &[0x04],
            7,
            &mut PinProxy::new(),
            &card,
            &AtomicBool::new(false),
            |_| {},
            Duration::from_millis(300),
        );
        assert_eq!(reply, [CTAP2_ERR_USER_ACTION_TIMEOUT]);
        assert_eq!(card.sim().states.last(), Some(&auth::FAILED));
    }

    #[test]
    fn pending_resyncs_reach_the_proxy() {
        let card = SimCard::new(Authenticator::new(Some(PIN), &[2]));
        let shared = Shared::default();
        let platform = Platform::new(Protocol::Two);
        {
            let mut proxy = shared.proxy.lock().unwrap();
            let mut send = |request: &[u8]| run(&mut proxy, &card, request).0;
            platform.token(&mut send, PIN, None).unwrap();
            assert!(proxy.has_session());
        }
        shared.resync(7);
        shared.tick(Instant::now());
        assert!(!shared.proxy.lock().unwrap().has_session());
    }
}

/// The whole key against a fake `/dev/uhid` kernel: HID reports in and out
/// through the real `Engine`, the PIN proxy and a simulated card.
#[cfg(all(test, target_os = "linux"))]
mod uhid_driver_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use tapbridge_host::ctap_pin::fake::Authenticator;

    const EVENT: usize = 4 + 4372;

    struct Kernel(UnixStream);

    impl Kernel {
        fn send_event(&mut self, kind: u32, body: &[u8]) {
            let mut buffer = vec![0u8; EVENT];
            buffer[..4].copy_from_slice(&kind.to_le_bytes());
            buffer[4..4 + body.len()].copy_from_slice(body);
            self.0.write_all(&buffer).unwrap();
        }

        /// A hidraw write: report ID 0 plus the 64-byte report.
        fn write_report(&mut self, report: &[u8; 64]) {
            let mut body = vec![0u8; 4099];
            body[1..65].copy_from_slice(report);
            body[4096..4098].copy_from_slice(&65u16.to_le_bytes());
            self.send_event(6, &body);
        }

        /// Next event of `kind`, skipping others (CREATE2, DESTROY...).
        fn next_event(&mut self, kind: u32) -> Vec<u8> {
            loop {
                let mut buffer = vec![0u8; EVENT];
                self.0.read_exact(&mut buffer).unwrap();
                if u32::from_le_bytes(buffer[..4].try_into().unwrap()) == kind {
                    return buffer[4..].to_vec();
                }
            }
        }

        /// Reads input reports until a message (any command but KEEPALIVE)
        /// is complete; returns (cmd, payload).
        fn read_message(&mut self) -> (u8, Vec<u8>) {
            let (mut cmd, mut want, mut data) = (0u8, 0usize, Vec::new());
            loop {
                let body = self.next_event(12); // UHID_INPUT2
                let size = u16::from_le_bytes([body[0], body[1]]) as usize;
                assert_eq!(size, 64);
                let report = &body[2..2 + size];
                if report[4] & 0x80 != 0 {
                    if report[4] & 0x7F == ctaphid::CMD_KEEPALIVE {
                        continue;
                    }
                    cmd = report[4] & 0x7F;
                    want = u16::from_be_bytes([report[5], report[6]]) as usize;
                    data = report[7..].to_vec();
                } else {
                    data.extend_from_slice(&report[5..]);
                }
                if data.len() >= want {
                    data.truncate(want);
                    return (cmd, data);
                }
            }
        }
    }

    fn init_report(cid: u32, cmd: u8, payload: &[u8]) -> [u8; 64] {
        let mut report = [0u8; 64];
        report[..4].copy_from_slice(&cid.to_be_bytes());
        report[4] = 0x80 | cmd;
        report[5..7].copy_from_slice(&(payload.len() as u16).to_be_bytes());
        report[7..7 + payload.len()].copy_from_slice(payload);
        report
    }

    struct Wired {
        kernel: Kernel,
        cid: u32,
        card: Arc<super::tests::SimCard>,
    }

    fn wire_up(card: super::tests::SimCard) -> Wired {
        let (kernel, ours) = UnixStream::pair().unwrap();
        let card = Arc::new(card);
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
        start(
            move |on_output, on_release| {
                hid::Device::from_streams(
                    Box::new(ours.try_clone().unwrap()),
                    Box::new(ours),
                    on_output,
                    on_release,
                )
            },
            card.clone(),
            log,
        )
        .unwrap();
        let mut kernel = Kernel(kernel);
        kernel.send_event(4, &[]); // UHID_OPEN
        kernel.write_report(&init_report(
            ctaphid::BROADCAST_CID,
            ctaphid::CMD_INIT,
            &[9; 8],
        ));
        let (cmd, payload) = kernel.read_message();
        assert_eq!(cmd, ctaphid::CMD_INIT);
        assert_eq!(&payload[..8], &[9; 8], "nonce echoed");
        let cid = u32::from_be_bytes(payload[8..12].try_into().unwrap());
        Wired { kernel, cid, card }
    }

    #[test]
    fn a_browser_gets_info_through_the_virtual_key() {
        let mut wired = wire_up(super::tests::SimCard::new(Authenticator::new(None, &[2])));
        let cid = wired.cid;
        // CTAPHID_CBOR authenticatorGetInfo (0x04).
        wired
            .kernel
            .write_report(&init_report(cid, ctaphid::CMD_CBOR, &[0x04]));
        let (cmd, reply) = wired.kernel.read_message();
        assert_eq!(cmd, ctaphid::CMD_CBOR);
        assert_eq!(reply[0], 0x00, "CTAP2_OK from the card");
        assert!(reply.len() > 8, "a CBOR GetInfo map follows the status");
        assert_eq!(
            wired.card.sim().taps,
            0,
            "the card was already on the phone"
        );
    }

    #[test]
    fn closing_the_device_cancels_a_request_waiting_for_a_tap() {
        let card = super::tests::SimCard::new(Authenticator::new(None, &[2]));
        {
            let mut sim = card.sim();
            sim.present = false;
            sim.user_taps = false;
        }
        let mut wired = wire_up(card);
        let cid = wired.cid;
        wired
            .kernel
            .write_report(&init_report(cid, ctaphid::CMD_CBOR, &[0x04]));
        while wired.card.sim().taps == 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
        wired.kernel.send_event(5, &[]); // UHID_CLOSE: the browser went away
        let (cmd, reply) = wired.kernel.read_message();
        assert_eq!(cmd, ctaphid::CMD_CBOR);
        assert_eq!(reply, vec![CTAP2_ERR_KEEPALIVE_CANCEL]);
    }
}

/// Against the real kernel and a real FIDO client. Both need root (for
/// `/dev/uhid`) and libfido2's `fido2-token`, so they only run on purpose:
/// the NixOS VM test (`nix flake check`) runs them as root.
#[cfg(all(test, target_os = "linux"))]
mod uhid_live_tests {
    use super::*;
    use tapbridge_host::ctap_pin::fake::Authenticator;

    /// The `/dev/hidrawN` of the virtual key, once the kernel has made it.
    fn wait_for_hidraw() -> String {
        for _ in 0..100 {
            for entry in std::fs::read_dir("/sys/class/hidraw")
                .into_iter()
                .flatten()
                .flatten()
            {
                let uevent =
                    std::fs::read_to_string(entry.path().join("device/uevent")).unwrap_or_default();
                if uevent.contains("HID_NAME=tapbridge security key") {
                    return format!("/dev/{}", entry.file_name().to_string_lossy());
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("no hidraw node named \"tapbridge security key\" appeared");
    }

    #[test]
    #[ignore = "needs root (/dev/uhid) and fido2-token (libfido2) on PATH"]
    fn libfido2_reads_get_info_through_the_real_kernel() {
        let card = Arc::new(super::tests::SimCard::new(Authenticator::new(None, &[2])));
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|line| eprintln!("{line}"));
        start(hid::Device::create, card, log).expect("create the virtual key (root?)");
        let path = wait_for_hidraw();
        let output = std::process::Command::new("fido2-token")
            .args(["-I", &path])
            .output()
            .expect("run fido2-token");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("FIDO_2_0"),
            "versions in the GetInfo reply: {stdout}"
        );
    }

    #[test]
    #[ignore = "needs root (/dev/uhid) and fido2-token (libfido2) on PATH"]
    fn libfido2_lists_the_virtual_key() {
        let card = Arc::new(super::tests::SimCard::new(Authenticator::new(None, &[2])));
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|line| eprintln!("{line}"));
        start(hid::Device::create, card, log).expect("create the virtual key (root?)");
        let path = wait_for_hidraw();
        let listing = std::process::Command::new("fido2-token")
            .arg("-L")
            .output()
            .expect("run fido2-token");
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(
            listing.contains(&path) && listing.contains("vendor=0x1209, product=0x7a73"),
            "fido2-token -L: {listing}"
        );
    }
}
