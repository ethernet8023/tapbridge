//! CTAPHID (FIDO CTAP 2.2 §11.2): the USB HID framing of the virtual
//! security key. Pure state machine: 64-byte reports in, reports out, plus
//! "start this CTAP request" / "cancel it" signals for the driver. No I/O,
//! no clocks of its own (callers pass `now`), so every rule is unit-tested.
//!
//! One transaction at a time across all channels (like a real key): while a
//! message is being reassembled or a request is with the card, other
//! channels get `ERR_CHANNEL_BUSY`; `INIT` is always answered.

use std::time::{Duration, Instant};

pub const REPORT_LEN: usize = 64;
const INIT_DATA: usize = REPORT_LEN - 7;
const CONT_DATA: usize = REPORT_LEN - 5;
/// Largest message: 57 + 128 * 59 bytes (§11.2.4).
pub const MAX_MESSAGE: usize = INIT_DATA + 128 * CONT_DATA;
pub const BROADCAST_CID: u32 = 0xFFFF_FFFF;

pub const CMD_PING: u8 = 0x01;
pub const CMD_MSG: u8 = 0x03;
pub const CMD_INIT: u8 = 0x06;
pub const CMD_WINK: u8 = 0x08;
pub const CMD_CBOR: u8 = 0x10;
pub const CMD_CANCEL: u8 = 0x11;
pub const CMD_KEEPALIVE: u8 = 0x3B;
pub const CMD_ERROR: u8 = 0x3F;

pub const ERR_INVALID_CMD: u8 = 0x01;
pub const ERR_INVALID_PAR: u8 = 0x02;
pub const ERR_INVALID_LEN: u8 = 0x03;
pub const ERR_INVALID_SEQ: u8 = 0x04;
pub const ERR_MSG_TIMEOUT: u8 = 0x05;
pub const ERR_CHANNEL_BUSY: u8 = 0x06;
pub const ERR_INVALID_CHANNEL: u8 = 0x0B;

/// KEEPALIVE status: the authenticator is working on the request.
pub const STATUS_PROCESSING: u8 = 1;
/// KEEPALIVE status: waiting for user presence (here: a tap on the phone).
pub const STATUS_UPNEEDED: u8 = 2;

/// CAPABILITY_WINK | CAPABILITY_CBOR | CAPABILITY_NMSG (no CTAP1/U2F).
const CAPABILITIES: u8 = 0x01 | 0x04 | 0x08;
/// A continuation must arrive within this long of the previous packet.
const ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(3);
/// Keepalives go out at least this often while a request is in progress.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(100);

pub type Report = [u8; REPORT_LEN];

/// A complete CTAPHID request for the driver to carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub cid: u32,
    pub cmd: u8,
    pub payload: Vec<u8>,
}

/// What one input produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub reports: Vec<Report>,
    /// Hand this to the card (a `CBOR` message).
    pub start: Option<Request>,
    /// The in-flight request was cancelled (`CANCEL`, or `INIT` resync on
    /// its channel): stop the card work; its result will be dropped or, for
    /// CANCEL, answered with `CTAP2_ERR_KEEPALIVE_CANCEL` by the driver.
    pub cancel: bool,
    /// `INIT` resynced this existing channel (busy or not): per-channel
    /// state above the engine (the PIN proxy's cached token) is dropped.
    pub resync: Option<u32>,
}

struct Assembly {
    cid: u32,
    cmd: u8,
    total: usize,
    data: Vec<u8>,
    next_seq: u8,
    last: Instant,
}

struct Busy {
    cid: u32,
    cmd: u8,
    status: u8,
    last_keepalive: Instant,
}

#[derive(Default)]
pub struct Engine {
    next_cid: u32,
    assembling: Option<Assembly>,
    busy: Option<Busy>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a request is with the card right now.
    pub fn busy_cid(&self) -> Option<u32> {
        self.busy.as_ref().map(|busy| busy.cid)
    }

    /// Changes the status the keepalives report for the in-flight request.
    pub fn set_status(&mut self, status: u8) {
        if let Some(busy) = self.busy.as_mut() {
            busy.status = status;
        }
    }

    /// One output report from the host (browser / OS FIDO stack).
    pub fn on_report(&mut self, report: &[u8], now: Instant) -> Output {
        let mut out = Output::default();
        if report.len() < 5 {
            return out;
        }
        let cid = u32::from_be_bytes([report[0], report[1], report[2], report[3]]);
        let kind = report[4];
        if kind & 0x80 != 0 {
            self.on_init_packet(cid, kind & 0x7F, report, now, &mut out);
        } else {
            self.on_continuation(cid, kind, report, now, &mut out);
        }
        out
    }

    fn on_init_packet(&mut self, cid: u32, cmd: u8, report: &[u8], now: Instant, out: &mut Output) {
        if report.len() < 7 {
            return;
        }
        let total = usize::from(u16::from_be_bytes([report[5], report[6]]));
        let data = &report[7..report.len().min(7 + INIT_DATA)];

        if cmd == CMD_INIT {
            self.on_init(cid, total, data, out);
            return;
        }
        if cid == 0 || cid == BROADCAST_CID {
            out.reports.extend(error(cid, ERR_INVALID_CHANNEL));
            return;
        }
        if cmd == CMD_CANCEL {
            // No reply of its own; only meaningful for the busy channel.
            if self.busy_cid() == Some(cid) {
                out.cancel = true;
            }
            return;
        }
        if self.busy.is_some() || self.assembling.as_ref().is_some_and(|a| a.cid != cid) {
            out.reports.extend(error(cid, ERR_CHANNEL_BUSY));
            return;
        }
        if self.assembling.is_some() {
            // Same channel started a new message mid-assembly (§11.2.4).
            self.assembling = None;
            out.reports.extend(error(cid, ERR_INVALID_SEQ));
            return;
        }
        if total > MAX_MESSAGE {
            out.reports.extend(error(cid, ERR_INVALID_LEN));
            return;
        }
        let take = total.min(data.len());
        let assembly = Assembly {
            cid,
            cmd,
            total,
            data: data[..take].to_vec(),
            next_seq: 0,
            last: now,
        };
        if assembly.data.len() == total {
            self.dispatch(assembly, now, out);
        } else {
            self.assembling = Some(assembly);
        }
    }

    fn on_init(&mut self, cid: u32, total: usize, data: &[u8], out: &mut Output) {
        if total != 8 || data.len() < 8 {
            out.reports.extend(error(cid, ERR_INVALID_LEN));
            return;
        }
        let assigned = if cid == BROADCAST_CID {
            self.allocate_cid()
        } else {
            // Resync on an existing channel: abort whatever it was doing.
            out.resync = Some(cid);
            if self.assembling.as_ref().is_some_and(|a| a.cid == cid) {
                self.assembling = None;
            }
            if self.busy_cid() == Some(cid) {
                self.busy = None;
                out.cancel = true;
            }
            cid
        };
        let mut reply = data[..8].to_vec();
        reply.extend_from_slice(&assigned.to_be_bytes());
        // CTAPHID protocol 2, device version 0.1.0, capabilities.
        reply.extend_from_slice(&[2, 0, 1, 0, CAPABILITIES]);
        out.reports.extend(fragment(cid, CMD_INIT, &reply));
    }

    fn allocate_cid(&mut self) -> u32 {
        loop {
            self.next_cid = self.next_cid.wrapping_add(1);
            if self.next_cid != 0 && self.next_cid != BROADCAST_CID {
                return self.next_cid;
            }
        }
    }

    fn on_continuation(
        &mut self,
        cid: u32,
        seq: u8,
        report: &[u8],
        now: Instant,
        out: &mut Output,
    ) {
        let Some(assembly) = self.assembling.as_mut() else {
            // Stray continuation (e.g. after an error): ignored (§11.2.4).
            return;
        };
        if assembly.cid != cid {
            out.reports.extend(error(cid, ERR_CHANNEL_BUSY));
            return;
        }
        if seq != assembly.next_seq {
            self.assembling = None;
            out.reports.extend(error(cid, ERR_INVALID_SEQ));
            return;
        }
        let data = &report[5..report.len().min(5 + CONT_DATA)];
        let take = (assembly.total - assembly.data.len()).min(data.len());
        assembly.data.extend_from_slice(&data[..take]);
        assembly.next_seq += 1;
        assembly.last = now;
        if assembly.data.len() == assembly.total {
            let assembly = self.assembling.take().expect("checked above");
            self.dispatch(assembly, now, out);
        }
    }

    fn dispatch(&mut self, message: Assembly, now: Instant, out: &mut Output) {
        let Assembly { cid, cmd, data, .. } = message;
        match cmd {
            CMD_PING => out.reports.extend(fragment(cid, CMD_PING, &data)),
            CMD_WINK => out.reports.extend(fragment(cid, CMD_WINK, &[])),
            CMD_CBOR if data.is_empty() => out.reports.extend(error(cid, ERR_INVALID_LEN)),
            CMD_CBOR => {
                self.busy = Some(Busy {
                    cid,
                    cmd,
                    status: STATUS_PROCESSING,
                    last_keepalive: now,
                });
                out.start = Some(Request {
                    cid,
                    cmd,
                    payload: data,
                });
            }
            _ => out.reports.extend(error(cid, ERR_INVALID_CMD)),
        }
    }

    /// Periodic: keepalives for the in-flight request, assembly timeouts.
    pub fn on_tick(&mut self, now: Instant) -> Vec<Report> {
        let mut reports = Vec::new();
        if let Some(assembly) = &self.assembling
            && now.duration_since(assembly.last) >= ASSEMBLY_TIMEOUT
        {
            reports.extend(error(assembly.cid, ERR_MSG_TIMEOUT));
            self.assembling = None;
        }
        if let Some(busy) = self.busy.as_mut()
            && now.duration_since(busy.last_keepalive) >= KEEPALIVE_INTERVAL
        {
            busy.last_keepalive = now;
            reports.extend(fragment(busy.cid, CMD_KEEPALIVE, &[busy.status]));
        }
        reports
    }

    /// The card answered request `cid` (CTAP status byte || CBOR). Dropped
    /// when the channel was resynced or a different request is in flight.
    pub fn on_response(&mut self, cid: u32, response: &[u8]) -> Vec<Report> {
        match self.busy.take() {
            Some(busy) if busy.cid == cid => fragment(cid, busy.cmd, response),
            other => {
                self.busy = other;
                Vec::new()
            }
        }
    }
}

/// Splits `payload` into an init packet plus continuations, zero-padded.
pub fn fragment(cid: u32, cmd: u8, payload: &[u8]) -> Vec<Report> {
    let mut reports = Vec::new();
    let mut first = [0u8; REPORT_LEN];
    first[..4].copy_from_slice(&cid.to_be_bytes());
    first[4] = 0x80 | cmd;
    first[5..7].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    let head = payload.len().min(INIT_DATA);
    first[7..7 + head].copy_from_slice(&payload[..head]);
    reports.push(first);
    for (seq, chunk) in payload[head..].chunks(CONT_DATA).enumerate() {
        let mut report = [0u8; REPORT_LEN];
        report[..4].copy_from_slice(&cid.to_be_bytes());
        report[4] = seq as u8;
        report[5..5 + chunk.len()].copy_from_slice(chunk);
        reports.push(report);
    }
    reports
}

fn error(cid: u32, code: u8) -> Vec<Report> {
    fragment(cid, CMD_ERROR, &[code])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn padded(s: &str) -> Vec<u8> {
        let mut v = hex(s);
        v.resize(REPORT_LEN, 0);
        v
    }

    /// Opens a channel the way Safari did in the spike capture.
    fn open(engine: &mut Engine, now: Instant) -> u32 {
        let out = engine.on_report(&padded("ffffffff8600082171c269d6d8928d"), now);
        let reply = &out.reports[0];
        u32::from_be_bytes([reply[15], reply[16], reply[17], reply[18]])
    }

    #[test]
    fn init_on_broadcast_echoes_nonce_and_assigns_a_channel() {
        let mut engine = Engine::new();
        let out = engine.on_report(&padded("ffffffff8600082171c269d6d8928d"), Instant::now());
        assert_eq!(out.reports.len(), 1);
        let r = &out.reports[0];
        assert_eq!(&r[..7], &hex("ffffffff860011")[..]);
        assert_eq!(&r[7..15], &hex("2171c269d6d8928d")[..]);
        let cid = u32::from_be_bytes([r[15], r[16], r[17], r[18]]);
        assert!(cid != 0 && cid != BROADCAST_CID);
        assert_eq!(&r[19..24], &[2, 0, 1, 0, CAPABILITIES]);
        assert_eq!(out.start, None);
    }

    #[test]
    fn each_init_gets_a_fresh_channel() {
        let mut engine = Engine::new();
        let now = Instant::now();
        assert_ne!(open(&mut engine, now), open(&mut engine, now));
    }

    #[test]
    fn single_packet_cbor_starts_a_request() {
        // GetInfo from the capture: 01000001 90 0001 04
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut report = padded("00000000900001 04");
        report[..4].copy_from_slice(&cid.to_be_bytes());
        let out = engine.on_report(&report, now);
        assert!(out.reports.is_empty());
        assert_eq!(
            out.start,
            Some(Request {
                cid,
                cmd: CMD_CBOR,
                payload: vec![0x04]
            })
        );
        assert_eq!(engine.busy_cid(), Some(cid));
    }

    #[test]
    fn multi_packet_make_credential_from_safari_reassembles() {
        // The spike's captured MakeCredential: 0x95 = 149 bytes over 3 packets.
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let packets = [
            "0100000590009501a4015820d730f3b1ffd4a96a3d49645f733909093ce5b3f6f9ad88938e668565aa676e3402a26269646b776562617574686e2e696f646e61",
            "01000005006d656b776562617574686e2e696f03a362696451776562617574686e696f2d6a686a686a68646e616d65666a686a686a686b646973706c61794e61",
            "01000005016d65666a686a686a680481a263616c672664747970656a7075626c69632d6b65790000000000000000000000000000000000000000000000000000",
        ];
        let mut start = None;
        for packet in packets {
            let mut report = hex(packet);
            report[..4].copy_from_slice(&cid.to_be_bytes());
            let out = engine.on_report(&report, now);
            assert!(
                out.reports.is_empty(),
                "no error expected: {:02x?}",
                out.reports
            );
            if out.start.is_some() {
                start = out.start;
            }
        }
        let request = start.expect("message completes on the third packet");
        assert_eq!(request.payload.len(), 0x95);
        assert_eq!(request.payload[0], 0x01, "authenticatorMakeCredential");
        assert!(
            request.payload.windows(11).any(|w| w == b"webauthn.io"),
            "rp id survives reassembly"
        );
        assert!(request.payload.ends_with(b"public-key"));
    }

    #[test]
    fn response_is_fragmented_with_the_request_command() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut report = padded("00000000900001 04");
        report[..4].copy_from_slice(&cid.to_be_bytes());
        engine.on_report(&report, now);

        let response: Vec<u8> = (0..130u8).collect();
        let reports = engine.on_response(cid, &response);
        assert_eq!(reports.len(), 3, "57 + 59 + 14");
        assert_eq!(reports[0][4], 0x80 | CMD_CBOR);
        assert_eq!(&reports[0][5..7], &[0, 130]);
        assert_eq!(reports[1][4], 0);
        assert_eq!(reports[2][4], 1);
        let mut joined = reports[0][7..].to_vec();
        joined.extend_from_slice(&reports[1][5..]);
        joined.extend_from_slice(&reports[2][5..5 + 14]);
        assert_eq!(joined, response);
        assert_eq!(engine.busy_cid(), None);
    }

    #[test]
    fn other_channels_are_busy_while_a_request_is_in_flight() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let a = open(&mut engine, now);
        let b = open(&mut engine, now);
        let mut report = padded("00000000900001 04");
        report[..4].copy_from_slice(&a.to_be_bytes());
        assert!(engine.on_report(&report, now).start.is_some());
        report[..4].copy_from_slice(&b.to_be_bytes());
        let out = engine.on_report(&report, now);
        assert_eq!(out.start, None);
        assert_eq!(out.reports[0][4], 0x80 | CMD_ERROR);
        assert_eq!(out.reports[0][7], ERR_CHANNEL_BUSY);
        // INIT still works for anyone.
        assert_ne!(open(&mut engine, now), 0);
    }

    #[test]
    fn keepalives_flow_while_busy_and_report_the_status() {
        let mut engine = Engine::new();
        let t0 = Instant::now();
        let cid = open(&mut engine, t0);
        assert!(
            engine.on_tick(t0 + Duration::from_secs(1)).is_empty(),
            "idle: none"
        );
        let mut report = padded("00000000900001 04");
        report[..4].copy_from_slice(&cid.to_be_bytes());
        engine.on_report(&report, t0);
        assert!(engine.on_tick(t0 + Duration::from_millis(50)).is_empty());
        let ka = engine.on_tick(t0 + Duration::from_millis(100));
        assert_eq!(ka.len(), 1);
        assert_eq!(
            &ka[0][4..8],
            &[0x80 | CMD_KEEPALIVE, 0, 1, STATUS_PROCESSING]
        );
        engine.set_status(STATUS_UPNEEDED);
        let ka = engine.on_tick(t0 + Duration::from_millis(200));
        assert_eq!(ka[0][7], STATUS_UPNEEDED);
    }

    #[test]
    fn cancel_on_the_busy_channel_signals_without_replying() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut report = padded("00000000900001 04");
        report[..4].copy_from_slice(&cid.to_be_bytes());
        engine.on_report(&report, now);
        let mut cancel = padded("00000000910000");
        cancel[..4].copy_from_slice(&cid.to_be_bytes());
        let out = engine.on_report(&cancel, now);
        assert!(out.cancel);
        assert!(out.reports.is_empty());
        // The driver answers the request with KEEPALIVE_CANCEL.
        assert_eq!(engine.on_response(cid, &[0x2D])[0][7], 0x2D);
    }

    #[test]
    fn init_resync_on_the_busy_channel_drops_the_late_response() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut report = padded("00000000900001 04");
        report[..4].copy_from_slice(&cid.to_be_bytes());
        engine.on_report(&report, now);
        let mut init = padded("000000008600080102030405060708");
        init[..4].copy_from_slice(&cid.to_be_bytes());
        let out = engine.on_report(&init, now);
        assert!(out.cancel);
        let r = &out.reports[0];
        assert_eq!(
            u32::from_be_bytes([r[15], r[16], r[17], r[18]]),
            cid,
            "same channel"
        );
        assert!(engine.on_response(cid, &[0x00]).is_empty());
    }

    #[test]
    fn init_on_an_existing_channel_reports_a_resync_even_when_idle() {
        // The PIN proxy forgets that channel's cached PIN state.
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut init = padded("000000008600080102030405060708");
        init[..4].copy_from_slice(&cid.to_be_bytes());
        let out = engine.on_report(&init, now);
        assert_eq!(out.resync, Some(cid));
        assert!(!out.cancel, "nothing was in flight");
        let out = engine.on_report(&padded("ffffffff8600080102030405060708"), now);
        assert_eq!(out.resync, None, "a broadcast INIT opens a new channel");
    }

    #[test]
    fn ping_and_wink_answer_immediately() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut ping = padded("00000000810003aabbcc");
        ping[..4].copy_from_slice(&cid.to_be_bytes());
        let out = engine.on_report(&ping, now);
        assert_eq!(&out.reports[0][4..10], &hex("810003aabbcc")[..]);
        let mut wink = padded("00000000880000");
        wink[..4].copy_from_slice(&cid.to_be_bytes());
        assert_eq!(engine.on_report(&wink, now).reports[0][4], 0x88);
    }

    #[test]
    fn u2f_msg_is_refused_as_advertised_by_nmsg() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut msg = padded("00000000830001 00");
        msg[..4].copy_from_slice(&cid.to_be_bytes());
        let out = engine.on_report(&msg, now);
        assert_eq!(out.reports[0][7], ERR_INVALID_CMD);
    }

    #[test]
    fn non_init_commands_on_broadcast_are_rejected() {
        let mut engine = Engine::new();
        let out = engine.on_report(&padded("ffffffff900001 04"), Instant::now());
        assert_eq!(out.reports[0][7], ERR_INVALID_CHANNEL);
    }

    #[test]
    fn wrong_sequence_aborts_the_message() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut first = padded("00000000900064 01");
        first[..4].copy_from_slice(&cid.to_be_bytes());
        engine.on_report(&first, now);
        let mut cont = padded("0000000001");
        cont[..4].copy_from_slice(&cid.to_be_bytes());
        let out = engine.on_report(&cont, now);
        assert_eq!(out.reports[0][7], ERR_INVALID_SEQ);
        assert_eq!(out.start, None);
    }

    #[test]
    fn stalled_assembly_times_out() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut first = padded("00000000900064 01");
        first[..4].copy_from_slice(&cid.to_be_bytes());
        engine.on_report(&first, now);
        let out = engine.on_tick(now + Duration::from_secs(3));
        assert_eq!(out[0][7], ERR_MSG_TIMEOUT);
        // Channel is free again.
        let mut ping = padded("00000000810000");
        ping[..4].copy_from_slice(&cid.to_be_bytes());
        assert_eq!(engine.on_report(&ping, now).reports[0][4], 0x81);
    }

    #[test]
    fn oversized_messages_are_rejected() {
        let mut engine = Engine::new();
        let now = Instant::now();
        let cid = open(&mut engine, now);
        let mut first = padded("0000000090ffff");
        first[..4].copy_from_slice(&cid.to_be_bytes());
        assert_eq!(engine.on_report(&first, now).reports[0][7], ERR_INVALID_LEN);
    }
}
