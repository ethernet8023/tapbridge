//! The Linux virtual HID device: `/dev/uhid`, FIDO report descriptor,
//! 64-byte reports both ways. Browsers (libfido2, Chromium) find it as a
//! hidraw node of a USB security key.
//!
//! Lifecycle notes taken from BryanJacobs/fido2-hid-bridge (MIT): the device
//! is `Bus::USB`, the kernel counts opens and closes, and when the last
//! program closes the node anything it was waiting for is abandoned (here:
//! `on_release` cancels a pending tap request). Unlike that bridge, channel
//! state lives in `ctaphid::Engine`, which serves one transaction at a time,
//! so there is no channel table to evict.
//!
//! The `uhid_event` structs are packed and fixed-size (`linux/uhid.h`), so
//! events are built and parsed as plain byte buffers: no unsafe, no libc.

use std::{
    fs::OpenOptions,
    io::{Read, Write},
    sync::{Arc, Mutex},
};

/// FIDO HID report descriptor (CTAP 2.2 §11.2.8.1): usage page 0xF1D0,
/// 64-byte input and output reports, no report IDs.
const DESCRIPTOR: [u8; 34] = [
    0x06, 0xD0, 0xF1, 0x09, 0x01, 0xA1, 0x01, // FIDO page, U2F device, app collection
    0x09, 0x20, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x81, 0x02, // input
    0x09, 0x21, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91, 0x02, // output
    0xC0,
];
/// pid.codes test VID; distinct PID from the IFD reader's (0x7A71) and the
/// macOS key's (0x7A72, same descriptor, different transport).
const VENDOR_ID: u32 = 0x1209;
const PRODUCT_ID: u32 = 0x7A73;
const BUS_USB: u16 = 3;

// `linux/uhid.h` event types and sizes.
const UHID_DESTROY: u32 = 1;
const UHID_OPEN: u32 = 4;
const UHID_CLOSE: u32 = 5;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;
const DATA_MAX: usize = 4096;
/// sizeof(struct uhid_event): a u32 type plus the largest union member
/// (`uhid_create2_req`: 128+64+64 name/phys/uniq, rd_size, bus, 4 u32s, 4096 data).
const EVENT_LEN: usize = 4 + 4372;
const EIO: u16 = 5;
const REPORT_LEN: usize = 64;

/// One event buffer: type at 0, `body` right after, zero padded to full size.
fn event(kind: u32, body: &[u8]) -> Vec<u8> {
    let mut buffer = vec![0u8; EVENT_LEN];
    buffer[..4].copy_from_slice(&kind.to_le_bytes());
    buffer[4..4 + body.len()].copy_from_slice(body);
    buffer
}

fn create2_event() -> Vec<u8> {
    let mut body = vec![0u8; 4372];
    let name = b"tapbridge security key";
    body[..name.len()].copy_from_slice(name);
    // phys[128..192] stays empty; uniq[192..256] is the serial.
    let uniq = b"tapbridge-1";
    body[192..192 + uniq.len()].copy_from_slice(uniq);
    body[256..258].copy_from_slice(&(DESCRIPTOR.len() as u16).to_le_bytes());
    body[258..260].copy_from_slice(&BUS_USB.to_le_bytes());
    body[260..264].copy_from_slice(&VENDOR_ID.to_le_bytes());
    body[264..268].copy_from_slice(&PRODUCT_ID.to_le_bytes());
    // version @268 and country @272 stay 0.
    body[276..276 + DESCRIPTOR.len()].copy_from_slice(&DESCRIPTOR);
    event(UHID_CREATE2, &body)
}

fn input2_event(report: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(2 + report.len());
    body.extend_from_slice(&(report.len() as u16).to_le_bytes());
    body.extend_from_slice(report);
    event(UHID_INPUT2, &body)
}

/// The report out of a `UHID_OUTPUT` body (`data[4096]`, `size`, `rtype`).
/// hidraw writes carry a leading report-ID byte (0: the descriptor has no
/// IDs), which the engine must not see.
fn output_report(body: &[u8]) -> Option<&[u8]> {
    let size = u16::from_le_bytes(body.get(DATA_MAX..DATA_MAX + 2)?.try_into().ok()?) as usize;
    let data = body.get(..size.min(DATA_MAX))?;
    match data {
        [0, rest @ ..] if rest.len() == REPORT_LEN => Some(rest),
        _ if data.len() == REPORT_LEN => Some(data),
        _ => None,
    }
}

/// Answers a GET_REPORT / SET_REPORT with an I/O error: the key has no
/// feature reports, and an unanswered request stalls the caller for seconds.
fn error_reply(kind: u32, body: &[u8]) -> Option<Vec<u8>> {
    let id = body.get(..4)?;
    let mut reply = id.to_vec();
    reply.extend_from_slice(&EIO.to_le_bytes());
    // GET_REPORT_REPLY also carries a size (0); SET_REPORT_REPLY ends at err.
    let reply_kind = if kind == UHID_GET_REPORT {
        reply.extend_from_slice(&0u16.to_le_bytes());
        UHID_GET_REPORT_REPLY
    } else {
        UHID_SET_REPORT_REPLY
    };
    Some(event(reply_kind, &reply))
}

pub struct Device {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Device {
    /// Opens `/dev/uhid` and registers the key. `on_output` receives each
    /// 64-byte output report; `on_release` fires when the last program
    /// closes the device. Err names why the key could not be created
    /// (usually: no access to `/dev/uhid`, see the udev rules).
    pub fn create(
        on_output: impl Fn(&[u8]) + Send + 'static,
        on_release: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uhid")
            .map_err(|error| format!("/dev/uhid: {error}"))?;
        let reader = file.try_clone().map_err(|error| error.to_string())?;
        Self::from_streams(Box::new(reader), Box::new(file), on_output, on_release)
    }

    /// The kernel side as a stream pair (the real device, or a fake in tests).
    pub(super) fn from_streams(
        mut reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        on_output: impl Fn(&[u8]) + Send + 'static,
        on_release: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let writer = Arc::new(Mutex::new(writer));
        write_event(&writer, &create2_event()).map_err(|error| format!("uhid create: {error}"))?;
        let replies = writer.clone();
        std::thread::Builder::new()
            .name("uhid-reader".into())
            .spawn(move || {
                let mut buffer = vec![0u8; EVENT_LEN];
                let mut opens = 0usize;
                // Ends when the device is destroyed or the stream closes.
                while reader.read_exact(&mut buffer).is_ok() {
                    let kind = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
                    let body = &buffer[4..];
                    match kind {
                        UHID_OPEN => opens += 1,
                        UHID_CLOSE => {
                            opens = opens.saturating_sub(1);
                            if opens == 0 {
                                on_release();
                            }
                        }
                        UHID_OUTPUT => {
                            if let Some(report) = output_report(body) {
                                on_output(report);
                            }
                        }
                        UHID_GET_REPORT | UHID_SET_REPORT => {
                            if let Some(reply) = error_reply(kind, body) {
                                let _ = write_event(&replies, &reply);
                            }
                        }
                        _ => {} // START, STOP: nothing to do.
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { writer })
    }

    /// Delivers one input report to whichever program has the key open.
    pub fn send(&self, report: &[u8]) {
        let _ = write_event(&self.writer, &input2_event(report));
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = write_event(&self.writer, &event(UHID_DESTROY, &[]));
    }
}

impl super::Sink for Device {
    fn send(&self, report: &[u8]) {
        Device::send(self, report);
    }
}

fn write_event(writer: &Mutex<Box<dyn Write + Send>>, event: &[u8]) -> std::io::Result<()> {
    let mut writer = writer
        .lock()
        .map_err(|_| std::io::Error::other("uhid writer poisoned"))?;
    writer.write_all(event)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(event: &[u8]) -> u32 {
        u32::from_le_bytes(event[..4].try_into().unwrap())
    }

    #[test]
    fn create2_describes_a_usb_fido_device() {
        let create = create2_event();
        assert_eq!(create.len(), EVENT_LEN);
        assert_eq!(kind(&create), UHID_CREATE2);
        let body = &create[4..];
        assert_eq!(&body[..22], b"tapbridge security key");
        assert_eq!(u16::from_le_bytes([body[256], body[257]]), 34);
        assert_eq!(u16::from_le_bytes([body[258], body[259]]), BUS_USB);
        assert_eq!(&body[276..279], &[0x06, 0xD0, 0xF1], "FIDO usage page");
    }

    #[test]
    fn output_reports_lose_the_hidraw_report_id() {
        let mut body = vec![0u8; 4099];
        body[1] = 0xAB; // report ID 0, then 64 bytes of report
        body[DATA_MAX..DATA_MAX + 2].copy_from_slice(&65u16.to_le_bytes());
        let report = output_report(&body).unwrap();
        assert_eq!(report.len(), 64);
        assert_eq!(report[0], 0xAB);
        // A bare 64-byte report passes through; anything else is dropped.
        body[DATA_MAX..DATA_MAX + 2].copy_from_slice(&64u16.to_le_bytes());
        assert_eq!(output_report(&body).unwrap().len(), 64);
        body[DATA_MAX..DATA_MAX + 2].copy_from_slice(&10u16.to_le_bytes());
        assert!(output_report(&body).is_none());
    }

    #[test]
    fn feature_report_requests_get_an_io_error_reply() {
        let mut body = vec![0u8; 8];
        body[..4].copy_from_slice(&9u32.to_le_bytes());
        let reply = error_reply(UHID_GET_REPORT, &body).unwrap();
        assert_eq!(kind(&reply), UHID_GET_REPORT_REPLY);
        assert_eq!(&reply[4..8], &9u32.to_le_bytes());
        assert_eq!(u16::from_le_bytes([reply[8], reply[9]]), EIO);
        let reply = error_reply(UHID_SET_REPORT, &body).unwrap();
        assert_eq!(kind(&reply), UHID_SET_REPORT_REPLY);
    }

    /// The reader thread against a fake kernel: opens/closes are counted,
    /// output reports arrive without the report ID, release fires once the
    /// last closer is gone.
    #[test]
    fn open_close_counting_and_output_flow() {
        use std::os::unix::net::UnixStream;
        use std::sync::mpsc;
        use std::time::Duration;
        let (kernel, ours) = UnixStream::pair().unwrap();
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
        let (rel_tx, rel_rx) = mpsc::channel::<()>();
        let device = Device::from_streams(
            Box::new(ours.try_clone().unwrap()),
            Box::new(ours),
            move |report| {
                let _ = out_tx.send(report.to_vec());
            },
            move || {
                let _ = rel_tx.send(());
            },
        )
        .unwrap();
        let mut kernel = kernel;
        let mut created = vec![0u8; EVENT_LEN];
        kernel.read_exact(&mut created).unwrap();
        assert_eq!(kind(&created), UHID_CREATE2);

        let mut output = vec![0u8; 4099];
        output[1] = 0x42;
        output[DATA_MAX..DATA_MAX + 2].copy_from_slice(&65u16.to_le_bytes());
        for (open_kind, body) in [
            (UHID_OPEN, vec![]),
            (UHID_OPEN, vec![]),
            (UHID_OUTPUT, output),
            (UHID_CLOSE, vec![]),
        ] {
            kernel.write_all(&event(open_kind, &body)).unwrap();
        }
        let report = out_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((report.len(), report[0]), (64, 0x42));
        assert!(
            rel_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "one opener is still left"
        );
        kernel.write_all(&event(UHID_CLOSE, &[])).unwrap();
        rel_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        device.send(&[1, 2, 3]);
        let mut input = vec![0u8; EVENT_LEN];
        kernel.read_exact(&mut input).unwrap();
        assert_eq!(kind(&input), UHID_INPUT2);
        assert_eq!(&input[4..9], &[3, 0, 1, 2, 3]);
        drop(device);
        let mut destroy = vec![0u8; EVENT_LEN];
        kernel.read_exact(&mut destroy).unwrap();
        assert_eq!(kind(&destroy), UHID_DESTROY);
    }
}
