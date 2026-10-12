//! The USB cable link (no network): watches for a phone on the cable,
//! switches it into Android Open Accessory mode and hands the accessory's
//! bulk endpoints to the host service as a [`UsbLink`].
//!
//! Library: `nusb` (pure Rust, IOKit on macOS, WinUSB on Windows), vendored
//! in `third_party/nusb` only to accept the `core-foundation` version gpui
//! pins. On macOS control transfers go to the device directly; on Windows
//! they go through any interface we can claim (WinUSB has no device-level
//! handle), which is usually the phone's ADB interface.
//!
//! Rules (docs/USB-PLAN.md, Track H):
//! - A plugged-in phone is only switched while tapbridge wants it
//!   ([`want_phone`]): the switch opens the app on the phone.
//! - Only phone-like devices are probed: an MTP/PTP (class 6/1) or ADB
//!   (ff/42/01) interface. Each plug-in is probed at most once (keyed by
//!   the OS device id, which changes on re-enumeration) and never again
//!   after it failed or declined, until it is unplugged.
//! - Reads offer a whole [`usb::CHUNK`]; writes are at most one chunk per
//!   transfer; a read timeout is `Ok(0)`.
//!
//! The probe-and-switch sequence follows android-nfc-bridge's `src/usb.rs`
//! (MIT, ~/.hermes/cache/scratch/android-nfc-bridge), ported from rusb to
//! nusb.

use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{Arc, Mutex, Weak},
    thread,
    time::{Duration, Instant},
};

use nusb::{
    DeviceId, DeviceInfo, Endpoint, Interface, MaybeFuture,
    descriptors::TransferType,
    transfer::{
        Buffer, Bulk, ControlIn, ControlOut, ControlType, Direction, In, Out, Recipient,
        TransferError,
    },
};
use tapbridge_core::usb::{self, CHUNK, Link, aoa};
use tapbridge_host::service::{ServiceHandle, UsbLink};

use crate::logging;

const POLL: Duration = Duration::from_secs(1);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
/// A read with nothing to say returns `Ok(0)` this often, so the pump can
/// notice a stop.
const READ_WAIT: Duration = Duration::from_millis(200);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Accessory link retries while the phone stays plugged in: the phone app
/// may open the accessory late (Android's "open with" prompt, app restart,
/// pairing window opened later), and a link that ended is reopened. Never
/// needs a replug.
const ATTEMPTS: u32 = 3;
const RETRY_GAP: Duration = Duration::from_secs(5);
/// Gap after the first few attempts.
const SLOW_RETRY_GAP: Duration = Duration::from_secs(10);

/// How long a request for the phone keeps the cable link wanted.
const DEMAND: Duration = Duration::from_secs(120);

/// Until when tapbridge wants the phone over the cable. Switching a phone
/// into accessory mode makes Android open the tapbridge app, so it only
/// happens while something needs the phone (a card request, the pairing
/// window), not on every plug-in.
static WANTED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Something needs the phone: switch a plugged-in phone to the cable link
/// within the next [`DEMAND`]. A link already up stays up regardless.
pub fn want_phone() {
    if let Ok(mut until) = WANTED_UNTIL.lock() {
        *until = Some(Instant::now() + DEMAND);
    }
}

fn phone_wanted() -> bool {
    WANTED_UNTIL
        .lock()
        .is_ok_and(|until| until.is_some_and(|until| Instant::now() < until))
}

/// Starts the watcher thread. It asks the service for "Use USB cable" on
/// every poll, so turning the setting off stops all probing.
pub fn spawn(service: ServiceHandle, runtime: tokio::runtime::Handle) {
    let started = thread::Builder::new()
        .name("tapbridge-usb-watch".into())
        .spawn(move || Watcher::default().run(&service, &runtime));
    if let Err(error) = started {
        logging::log(&format!("usb: watcher unavailable: {error}"));
    }
}

/// One accessory link we handed to the service.
struct Attached {
    link: Weak<AoaLink>,
    attempts: u32,
    ended_at: Option<Instant>,
}

#[derive(Default)]
struct Watcher {
    /// Phone-like devices already probed this plug-in (switched, declined
    /// or failed): never probed again until they disappear.
    probed: HashSet<DeviceId>,
    /// Accessory-mode devices and their link.
    accessories: HashMap<DeviceId, Attached>,
    #[cfg(windows)]
    driver: windows::DriverSetup,
}

impl Watcher {
    fn run(&mut self, service: &ServiceHandle, runtime: &tokio::runtime::Handle) {
        loop {
            thread::sleep(POLL);
            // A stopped service answers with the default status (use_usb
            // false): idle, and the thread ends with the process.
            if !service.status().use_usb {
                self.probed.clear();
                self.accessories.clear();
                continue;
            }
            let devices: Vec<DeviceInfo> = match nusb::list_devices().wait() {
                Ok(devices) => devices.collect(),
                Err(error) => {
                    logging::log(&format!("usb: cannot list devices: {error}"));
                    continue;
                }
            };
            let present: HashSet<DeviceId> = devices.iter().map(DeviceInfo::id).collect();
            self.probed.retain(|id| present.contains(id));
            self.accessories.retain(|id, _| present.contains(id));
            for info in &devices {
                if aoa::is_accessory(info.vendor_id(), info.product_id()) {
                    self.accessory(info, service, runtime);
                } else if is_phone_like(info) && phone_wanted() && self.probed.insert(info.id()) {
                    match switch_to_accessory(info) {
                        Ok(()) => logging::log(&format!(
                            "usb: {:04x}:{:04x} switching to accessory mode",
                            info.vendor_id(),
                            info.product_id()
                        )),
                        Err(error) => logging::log(&format!(
                            "usb: {:04x}:{:04x} not switched: {error}",
                            info.vendor_id(),
                            info.product_id()
                        )),
                    }
                }
            }
        }
    }

    fn accessory(
        &mut self,
        info: &DeviceInfo,
        service: &ServiceHandle,
        runtime: &tokio::runtime::Handle,
    ) {
        let entry = self.accessories.entry(info.id()).or_insert(Attached {
            link: Weak::new(),
            attempts: 0,
            ended_at: None,
        });
        if entry.link.strong_count() > 0 {
            return;
        }
        if entry.attempts > 0 {
            let ended = *entry.ended_at.get_or_insert_with(Instant::now);
            let gap = if entry.attempts >= ATTEMPTS {
                SLOW_RETRY_GAP
            } else {
                RETRY_GAP
            };
            if ended.elapsed() < gap {
                return;
            }
        }
        entry.attempts += 1;
        entry.ended_at = None;
        #[cfg(windows)]
        if !self.driver.ready(info) {
            // Driver setup declined, failed, or rebinding (the device
            // re-enumerates with a new id).
            return;
        }
        match AoaLink::open(info) {
            Ok(link) => {
                logging::log("usb: accessory open, handing it to the service");
                let link = Arc::new(link);
                entry.link = Arc::downgrade(&link);
                let (stream, guard) = usb::pump(link, runtime.clone());
                service.attach_usb(UsbLink::new(stream, guard));
            }
            Err(error) => {
                logging::log(&format!("usb: accessory not opened: {error}"));
            }
        }
    }
}

/// A phone-like device: one interface is MTP/PTP (still image, 6/1) or
/// ADB (ff/42/01). Nothing else ever gets a vendor request.
fn is_phone_like(info: &DeviceInfo) -> bool {
    info.interfaces().any(|interface| {
        (interface.class() == 6 && interface.subclass() == 1)
            || (interface.class() == 0xff
                && interface.subclass() == 0x42
                && interface.protocol() == 1)
    })
}

/// Sends GET_PROTOCOL, the six identity strings and START. The phone then
/// re-enumerates as 18d1:2d0x.
fn switch_to_accessory(info: &DeviceInfo) -> io::Result<()> {
    let device = info.open().wait().map_err(io::Error::other)?;
    let control = Control::new(&device, info)?;
    let version = control.read(ControlIn {
        control_type: ControlType::Vendor,
        recipient: Recipient::Device,
        request: aoa::GET_PROTOCOL,
        value: 0,
        index: 0,
        length: 2,
    })?;
    if version.len() != 2 || u16::from_le_bytes([version[0], version[1]]) < 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no Android Open Accessory support",
        ));
    }
    for (index, value) in aoa::STRINGS.iter().enumerate() {
        let mut data = value.as_bytes().to_vec();
        data.push(0);
        control.write(ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: aoa::SEND_STRING,
            value: 0,
            index: index as u16,
            data: &data,
        })?;
    }
    control.write(ControlOut {
        control_type: ControlType::Vendor,
        recipient: Recipient::Device,
        request: aoa::START,
        value: 0,
        index: 0,
        data: &[],
    })
}

fn transfer_error(error: TransferError) -> io::Error {
    match error {
        TransferError::Disconnected => io::Error::new(io::ErrorKind::NotConnected, error),
        TransferError::Cancelled => io::Error::new(io::ErrorKind::TimedOut, error),
        TransferError::Stall => io::Error::new(io::ErrorKind::Unsupported, error),
        other => io::Error::other(other),
    }
}

/// Device-level control transfers: on the device (macOS), or through the
/// first interface we can claim (Windows; WinUSB has no device handle).
struct Control {
    #[cfg(not(windows))]
    device: nusb::Device,
    #[cfg(windows)]
    interface: Interface,
}

impl Control {
    #[cfg(not(windows))]
    fn new(device: &nusb::Device, _info: &DeviceInfo) -> io::Result<Self> {
        Ok(Self {
            device: device.clone(),
        })
    }

    #[cfg(windows)]
    fn new(device: &nusb::Device, info: &DeviceInfo) -> io::Result<Self> {
        for interface in info.interfaces() {
            if let Ok(interface) = device.claim_interface(interface.interface_number()).wait() {
                return Ok(Self { interface });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "no WinUSB interface to send the accessory request through (turn on USB debugging, or close adb)",
        ))
    }

    fn read(&self, request: ControlIn) -> io::Result<Vec<u8>> {
        #[cfg(not(windows))]
        let result = self.device.control_in(request, CONTROL_TIMEOUT).wait();
        #[cfg(windows)]
        let result = self.interface.control_in(request, CONTROL_TIMEOUT).wait();
        result.map_err(transfer_error)
    }

    fn write(&self, request: ControlOut<'_>) -> io::Result<()> {
        #[cfg(not(windows))]
        let result = self.device.control_out(request, CONTROL_TIMEOUT).wait();
        #[cfg(windows)]
        let result = self.interface.control_out(request, CONTROL_TIMEOUT).wait();
        result.map_err(transfer_error)
    }
}

/// The accessory's bulk endpoint pair as a blocking [`Link`].
struct AoaLink {
    input: Mutex<Endpoint<Bulk, In>>,
    output: Mutex<Endpoint<Bulk, Out>>,
    _interface: Interface,
}

impl AoaLink {
    /// Claims the accessory interface: vendor class (ff), not ADB (42),
    /// with one bulk IN and one bulk OUT endpoint.
    fn open(info: &DeviceInfo) -> io::Result<Self> {
        let device = info.open().wait().map_err(io::Error::other)?;
        let config = device.active_configuration().map_err(io::Error::other)?;
        let found = config.interface_alt_settings().find_map(|alt| {
            if alt.class() != 0xff || alt.subclass() == 0x42 {
                return None;
            }
            let bulk = |dir: Direction| {
                alt.endpoints()
                    .find(|ep| ep.transfer_type() == TransferType::Bulk && ep.direction() == dir)
                    .map(|ep| ep.address())
            };
            Some((
                alt.interface_number(),
                alt.alternate_setting(),
                bulk(Direction::In)?,
                bulk(Direction::Out)?,
            ))
        });
        let Some((number, alt, input, output)) = found else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no accessory bulk interface",
            ));
        };
        let interface = device
            .detach_and_claim_interface(number)
            .wait()
            .map_err(io::Error::other)?;
        if alt != 0 {
            interface
                .set_alt_setting(alt)
                .wait()
                .map_err(io::Error::other)?;
        }
        let input = interface
            .endpoint::<Bulk, In>(input)
            .map_err(io::Error::other)?;
        let output = interface
            .endpoint::<Bulk, Out>(output)
            .map_err(io::Error::other)?;
        Ok(Self {
            input: Mutex::new(input),
            output: Mutex::new(output),
            _interface: interface,
        })
    }
}

impl Link for AoaLink {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut input = self
            .input
            .lock()
            .map_err(|_| io::Error::other("usb reader poisoned"))?;
        // One whole-chunk transfer stays queued across timeouts, so a
        // timeout never cancels (and loses) a transfer in flight.
        if input.pending() == 0 {
            let size = CHUNK.min(buf.len());
            let size = size - size % input.max_packet_size().max(1);
            input.submit(Buffer::new(size));
        }
        let Some(completion) = input.wait_next_complete(READ_WAIT) else {
            return Ok(0);
        };
        completion.status.map_err(transfer_error)?;
        let data = &completion.buffer[..completion.actual_len.min(completion.buffer.len())];
        buf[..data.len()].copy_from_slice(data);
        if data.is_empty() {
            // A zero-length packet: nothing to deliver; not an EOF.
            return Ok(0);
        }
        Ok(data.len())
    }

    fn write_all(&self, data: &[u8]) -> io::Result<()> {
        debug_assert!(data.len() <= CHUNK);
        let mut output = self
            .output
            .lock()
            .map_err(|_| io::Error::other("usb writer poisoned"))?;
        let completion = output.transfer_blocking(Buffer::from(data.to_vec()), WRITE_TIMEOUT);
        completion.status.map_err(transfer_error)?;
        if completion.actual_len != data.len() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short usb write"));
        }
        // The phone reads whole chunks: a transfer that is a multiple of the
        // packet size but shorter than a chunk needs a zero-length packet to
        // end its read.
        if !data.is_empty()
            && data.len() < CHUNK
            && data.len().is_multiple_of(output.max_packet_size())
        {
            output
                .transfer_blocking(Buffer::new(0), WRITE_TIMEOUT)
                .status
                .map_err(transfer_error)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
mod windows;
