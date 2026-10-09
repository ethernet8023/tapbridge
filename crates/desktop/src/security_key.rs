//! macOS virtual FIDO security key: browsers (Safari included) see a USB
//! security key; every CTAP request is relayed to the card on the phone
//! over the live session. Keys never leave the card.
//!
//! Layers: [`tapbridge_host::ctaphid::Engine`] (HID framing),
//! [`run_request`] (wait for a card, wake the phone, relay with
//! [`tapbridge_host::ctap_nfc::relay`]), and [`hid`] (the IOKit virtual
//! device). Creating the device needs Apple's restricted
//! `com.apple.developer.hid.virtual.device` entitlement; without it the
//! device is simply not created and the rest of the app runs as before.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tapbridge_host::{
    ctap_nfc::{self, RelayError},
    ctaphid::{self, Engine, Report},
    service::ServiceHandle,
};

/// How long a request waits for a card on the phone before giving up.
const TAP_TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// CTAP status bytes the relay itself produces.
const CTAP2_ERR_KEEPALIVE_CANCEL: u8 = 0x2D;
const CTAP2_ERR_USER_ACTION_TIMEOUT: u8 = 0x2F;
const CTAP1_ERR_OTHER: u8 = 0x7F;

/// What the request worker needs from the outside world (a trait so the
/// waiting/relay rules are tested without a phone).
pub trait Card {
    fn present(&self) -> bool;
    /// Ask the phone for a tap (wake it or show "tap now"). Called once per
    /// request that finds no card.
    fn request_tap(&self);
    fn transmit(&self, apdu: Vec<u8>) -> Result<Vec<u8>, String>;
}

/// Carries out one CTAP request: waits (up to [`TAP_TIMEOUT`]) for a card,
/// then relays. Always returns a CTAP reply (status byte first): relay
/// failures become CTAP errors the browser understands.
pub fn run_request(
    request: &[u8],
    card: &dyn Card,
    cancelled: &AtomicBool,
    mut waiting_for_tap: impl FnMut(),
    timeout: Duration,
) -> Vec<u8> {
    let started = Instant::now();
    if !card.present() {
        card.request_tap();
        waiting_for_tap();
        while !card.present() {
            if cancelled.load(Ordering::SeqCst) {
                return vec![CTAP2_ERR_KEEPALIVE_CANCEL];
            }
            if started.elapsed() >= timeout {
                return vec![CTAP2_ERR_USER_ACTION_TIMEOUT];
            }
            std::thread::sleep(POLL);
        }
    }
    if cancelled.load(Ordering::SeqCst) {
        return vec![CTAP2_ERR_KEEPALIVE_CANCEL];
    }
    match ctap_nfc::relay(request, |apdu| card.transmit(apdu)) {
        Ok(reply) => reply,
        Err(RelayError::Transport(_)) | Err(RelayError::Card(_)) | Err(RelayError::Empty) => {
            vec![CTAP1_ERR_OTHER]
        }
    }
}

/// The running service as a [`Card`].
struct ServiceCard {
    service: ServiceHandle,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Card for ServiceCard {
    fn present(&self) -> bool {
        self.service.status().card_present
    }
    fn request_tap(&self) {
        (self.wake)();
    }
    fn transmit(&self, apdu: Vec<u8>) -> Result<Vec<u8>, String> {
        self.service
            .ctap_transmit(apdu)
            .map_err(|error| format!("{error:?}"))
    }
}

enum Input {
    Report(Vec<u8>),
    Done { cid: u32, reply: Vec<u8> },
    WaitingForTap { cid: u32 },
}

/// Creates the virtual security key and serves it on background threads.
/// `wake` asks the phone for a tap (the tray's request-tap path). Returns
/// an error (and changes nothing) when macOS refuses the device, e.g.
/// without the HID virtual device entitlement.
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
    let (tx, rx) = mpsc::channel::<Input>();
    let reports = tx.clone();
    let device = hid::Device::create(move |report| {
        let _ = reports.send(Input::Report(report.to_vec()));
    })
    .ok_or("macOS refused the virtual HID device (missing entitlement?)")?;
    std::thread::Builder::new()
        .name("security-key".into())
        .spawn(move || drive(device, rx, tx, card, log))
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The driver loop: owns the engine and the device; one worker thread per
/// CTAP request.
fn drive(
    device: hid::Device,
    rx: mpsc::Receiver<Input>,
    tx: mpsc::Sender<Input>,
    card: Arc<dyn Card + Send + Sync>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let mut engine = Engine::new();
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
                    log(&format!(
                        "security key: CTAP2 command 0x{:02x} ({} bytes)",
                        request.payload.first().copied().unwrap_or(0),
                        request.payload.len()
                    ));
                    let flag = Arc::new(AtomicBool::new(false));
                    cancel_flag = Some(flag.clone());
                    let card = card.clone();
                    let tx = tx.clone();
                    let cid = request.cid;
                    std::thread::spawn(move || {
                        let waiting = tx.clone();
                        let reply = run_request(
                            &request.payload,
                            card.as_ref(),
                            &flag,
                            || {
                                let _ = waiting.send(Input::WaitingForTap { cid });
                            },
                            TAP_TIMEOUT,
                        );
                        let _ = tx.send(Input::Done { cid, reply });
                    });
                }
            }
            Ok(Input::WaitingForTap { cid }) => {
                if engine.busy_cid() == Some(cid) {
                    engine.set_status(ctaphid::STATUS_UPNEEDED);
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
        send(engine.on_tick(Instant::now()));
    }
}

/// The IOKit virtual HID device (`IOHIDUserDevice`), FIDO report
/// descriptor, 64-byte reports both ways.
mod hid {
    use block2::RcBlock;
    use core_foundation::{
        base::{CFType, TCFType},
        data::CFData,
        dictionary::CFDictionary,
        number::CFNumber,
        string::CFString,
    };
    use dispatch2::{DispatchQueue, DispatchRetained};
    use std::ffi::c_void;

    /// FIDO HID report descriptor (CTAP 2.2 §11.2.8.1): usage page 0xF1D0,
    /// 64-byte input and output reports, no report IDs.
    const DESCRIPTOR: [u8; 34] = [
        0x06, 0xD0, 0xF1, 0x09, 0x01, 0xA1, 0x01, // FIDO page, U2F device, app collection
        0x09, 0x20, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x81, 0x02, // input
        0x09, 0x21, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91,
        0x02, // output
        0xC0,
    ];
    /// pid.codes test VID; distinct PID from the IFD reader's.
    const VENDOR_ID: i32 = 0x1209;
    const PRODUCT_ID: i32 = 0x7A72;
    const OUTPUT_REPORT: u32 = 1; // kIOHIDReportTypeOutput

    type Ref = *mut c_void;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOHIDUserDeviceCreateWithProperties(
            allocator: *const c_void,
            properties: *const c_void,
            options: u32,
        ) -> Ref;
        fn IOHIDUserDeviceRegisterSetReportBlock(device: Ref, block: *mut c_void);
        fn IOHIDUserDeviceSetDispatchQueue(device: Ref, queue: *mut c_void);
        fn IOHIDUserDeviceActivate(device: Ref);
        fn IOHIDUserDeviceHandleReportWithTimeStamp(
            device: Ref,
            timestamp: u64,
            report: *const u8,
            length: isize,
        ) -> i32;
    }
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
    }

    pub struct Device {
        raw: Ref,
        _queue: DispatchRetained<DispatchQueue>,
        _block: RcBlock<dyn Fn(u32, u32, *const u8, isize) -> i32>,
    }

    // SAFETY: IOHIDUserDevice is a CF object; handle-report calls are
    // thread-safe per IOKit, and the set-report block runs on our queue.
    unsafe impl Send for Device {}

    impl Device {
        /// Creates and activates the device; `on_output` receives each
        /// output report (on a private dispatch queue). None when macOS
        /// refuses (no entitlement).
        pub fn create(on_output: impl Fn(&[u8]) + 'static) -> Option<Self> {
            let number = |n: i32| CFNumber::from(n).as_CFType();
            let text = |s: &str| CFString::new(s).as_CFType();
            let properties: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[
                (
                    CFString::new("ReportDescriptor"),
                    CFData::from_buffer(&DESCRIPTOR).as_CFType(),
                ),
                (CFString::new("VendorID"), number(VENDOR_ID)),
                (CFString::new("ProductID"), number(PRODUCT_ID)),
                (CFString::new("Transport"), text("USB")),
                (CFString::new("Product"), text("tapbridge security key")),
                (CFString::new("Manufacturer"), text("tapbridge")),
                (CFString::new("SerialNumber"), text("tapbridge-1")),
            ]);
            // SAFETY: valid CFDictionary; returns a +1 ref or null.
            let raw = unsafe {
                IOHIDUserDeviceCreateWithProperties(
                    std::ptr::null(),
                    properties.as_concrete_TypeRef() as *const c_void,
                    0,
                )
            };
            if raw.is_null() {
                return None;
            }
            let block = RcBlock::new(
                move |kind: u32, _id: u32, report: *const u8, length: isize| -> i32 {
                    if kind == OUTPUT_REPORT && !report.is_null() && length > 0 {
                        // SAFETY: IOKit passes a buffer of `length` bytes.
                        on_output(unsafe { std::slice::from_raw_parts(report, length as usize) });
                    }
                    0 // kIOReturnSuccess
                },
            );
            let queue = DispatchQueue::new("computer.ethernet.tapbridge.security-key", None);
            // SAFETY: registration before activation, as the header requires;
            // the block and queue outlive the device (owned by `Device`).
            unsafe {
                IOHIDUserDeviceRegisterSetReportBlock(raw, RcBlock::as_ptr(&block).cast());
                IOHIDUserDeviceSetDispatchQueue(
                    raw,
                    (&*queue as *const DispatchQueue).cast_mut().cast(),
                );
                IOHIDUserDeviceActivate(raw);
            }
            Some(Self {
                raw,
                _queue: queue,
                _block: block,
            })
        }

        /// Delivers one input report to the host.
        pub fn send(&self, report: &[u8]) {
            // SAFETY: live device; the report buffer is valid for the call.
            unsafe {
                IOHIDUserDeviceHandleReportWithTimeStamp(
                    self.raw,
                    mach_absolute_time(),
                    report.as_ptr(),
                    report.len() as isize,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    /// Fake phone card: becomes present after `present_after` polls,
    /// answers SELECT + one NFCCTAP_MSG.
    struct FakeCard {
        polls: AtomicUsize,
        present_after: usize,
        taps: AtomicUsize,
        apdus: Mutex<Vec<Vec<u8>>>,
        fail: bool,
    }

    impl FakeCard {
        fn new(present_after: usize) -> Self {
            Self {
                polls: AtomicUsize::new(0),
                present_after,
                taps: AtomicUsize::new(0),
                apdus: Mutex::new(Vec::new()),
                fail: false,
            }
        }
    }

    impl Card for FakeCard {
        fn present(&self) -> bool {
            self.polls.fetch_add(1, Ordering::SeqCst) >= self.present_after
        }
        fn request_tap(&self) {
            self.taps.fetch_add(1, Ordering::SeqCst);
        }
        fn transmit(&self, apdu: Vec<u8>) -> Result<Vec<u8>, String> {
            if self.fail {
                return Err("card gone".into());
            }
            let reply = if apdu[1] == 0xA4 {
                b"FIDO_2_0\x90\x00".to_vec()
            } else {
                vec![0x00, 0xA1, 0x90, 0x00]
            };
            self.apdus.lock().unwrap().push(apdu);
            Ok(reply)
        }
    }

    /// Live check: creates the real virtual device and finds it in the IO
    /// registry. Needs the entitlement (sign the test binary with
    /// packaging/macos/tapbridge.entitlements; AMFI off or a profile).
    #[test]
    #[ignore = "needs the HID virtual device entitlement"]
    fn virtual_device_registers_with_iokit() {
        let device = hid::Device::create(|_| {}).expect("device created (entitlement present?)");
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
            fn present(&self) -> bool {
                true
            }
            fn request_tap(&self) {}
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
        spawn_with_card(
            Arc::new(InfoCard(info)),
            Arc::new(|line: &str| eprintln!("{line}")),
        )
        .expect("device created (entitlement present?)");
        std::thread::sleep(Duration::from_millis(500));
        let script = r#"
from fido2.hid import CtapHidDevice
from fido2.ctap2 import Ctap2
dev = next(d for d in CtapHidDevice.list_devices() if d.descriptor.product_name == "tapbridge security key")
info = Ctap2(dev).get_info()
print("versions", info.versions)
print("extensions", info.extensions)
print("aaguid", info.aaguid.hex())
print("max_msg_size", info.max_msg_size)
"#;
        let output = std::process::Command::new(python)
            .args(["-c", script])
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

    #[test]
    fn card_already_present_relays_without_asking_for_a_tap() {
        let card = FakeCard::new(0);
        let mut waited = false;
        let reply = run_request(
            &[0x04],
            &card,
            &AtomicBool::new(false),
            || waited = true,
            TAP_TIMEOUT,
        );
        assert_eq!(reply, [0x00, 0xA1]);
        assert_eq!(card.taps.load(Ordering::SeqCst), 0);
        assert!(!waited);
        assert_eq!(card.apdus.lock().unwrap().len(), 2, "SELECT + MSG");
    }

    #[test]
    fn no_card_asks_for_one_tap_then_relays_when_it_arrives() {
        let card = FakeCard::new(3);
        let mut waited = 0;
        let reply = run_request(
            &[0x04],
            &card,
            &AtomicBool::new(false),
            || waited += 1,
            TAP_TIMEOUT,
        );
        assert_eq!(reply, [0x00, 0xA1]);
        assert_eq!(card.taps.load(Ordering::SeqCst), 1);
        assert_eq!(waited, 1);
    }

    #[test]
    fn waiting_times_out_with_user_action_timeout() {
        let card = FakeCard::new(usize::MAX);
        let reply = run_request(
            &[0x04],
            &card,
            &AtomicBool::new(false),
            || {},
            Duration::from_millis(250),
        );
        assert_eq!(reply, [CTAP2_ERR_USER_ACTION_TIMEOUT]);
    }

    #[test]
    fn cancel_while_waiting_returns_keepalive_cancel() {
        let card = FakeCard::new(usize::MAX);
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            run_request(&[0x04], &card, &cancelled, || {}, TAP_TIMEOUT),
            [CTAP2_ERR_KEEPALIVE_CANCEL]
        );
    }

    #[test]
    fn relay_failures_become_ctap_other() {
        let mut card = FakeCard::new(0);
        card.fail = true;
        assert_eq!(
            run_request(&[0x04], &card, &AtomicBool::new(false), || {}, TAP_TIMEOUT),
            [CTAP1_ERR_OTHER]
        );
    }
}
