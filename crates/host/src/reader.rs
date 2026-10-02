//! USB/IP session owner: accepts loopback usbip clients, answers
//! OP_REQ_DEVLIST / OP_REQ_IMPORT per the standard Linux USB/IP protocol
//! (https://docs.kernel.org/usb/usbip_protocol.html), drives CCID over the
//! bulk endpoints and relays APDUs to the phone over wire frames.
//!
//! Wire facts implemented here (all big-endian):
//! - every URB packet is a 48-byte header; CMD_SUBMIT command 1 at 0,
//!   seqnum 4, devid 8, direction 12 (0=OUT, 1=IN), ep 16, transfer_flags 20,
//!   buffer_length 24, setup 40..48. OUT payload (including EP0 data stage)
//!   follows the header; IN submits carry no payload.
//! - RET_SUBMIT command 3 at 0, seqnum 4, status 20, actual_length 24,
//!   payload from 48 (IN only; OUT completes with actual = transferred, no bytes).
//! - CMD_UNLINK command 2, target seqnum at 20; RET_UNLINK command 4,
//!   status at 20, always 48 bytes.
//! - OP_REP_IMPORT is common(8) + usb_device(312) = 320 bytes; the USB
//!   descriptors are fetched later over EP0, never inlined.
//!
//! CCID model: every bulk-OUT URB is acknowledged immediately with
//! actual_length = byte count and NO payload. The bytes are reassembled into
//! CCID messages (fragmented across and coalesced within URBs are both
//! supported); each parsed command queues a response for an independent
//! bulk-IN URB. Bulk-IN replies are capped to the requested length; any tail
//! is retained for the next IN. Interrupt-IN notifications are coalesced.
//! UNLINK cancels a pending USB transfer, never a CCID command.
//!
//! Invariants:
//! - one imported client; a second import is rejected (status != 0)
//! - one APDU in flight; a second XfrBlock gets CCID CMD_SLOT_BUSY
//! - phone Card with EMPTY ATR = card absent
//! - latest phone Card is retained by serve and replayed into a new import
//! - stale response ids/generations never satisfy a pending APDU
//! - per-serve monotonic u64 APDU ids, shared across imports
//! - CCID time extension (bmCommandStatus 10b) on deadline, hard fail after a bound
//! - two-phase CCID abort (EP0 class request + bulk 0x72) distinct from USB UNLINK
//! - all queues bounded; all I/O bounded; ids are per-serve, not global

use crate::ccid::{self, BulkOutKind};
use crate::usb::{self, ControlReply};
use crate::usbip::{self, OpHeader};
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tapbridge_core::wire::Frame;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

const APDU_DEADLINE: Duration = Duration::from_secs(2);
const MAX_EXTENSIONS: u32 = 10;
/// Bound on the bulk-IN response queue (FIFO).
const MAX_BULK_QUEUE: usize = 16;
/// Bound on parked interrupt-IN URBs.
const MAX_INTR_WAIT: usize = 4;
/// Hard cap on one bulk-OUT payload we will read (bounded I/O).
const MAX_OUT_PAYLOAD: usize = 8192;
/// Bound on concurrently accepted connections.
const MAX_CONNECTIONS: usize = 8;
const BUSID: &str = "1-1";
/// devid exported at import: (busnum 1 << 16) | devnum 1.
const EXPORTED_DEVID: u32 = 0x0001_0001;

type SharedFlag = Arc<AtomicBool>;
struct AbortReader(tokio::task::JoinHandle<()>);
impl Drop for AbortReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct ImportGuard(SharedFlag);
impl Drop for ImportGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
struct CardState {
    generation: u64,
    atr: Vec<u8>,
}

impl CardState {
    fn present(&self) -> bool {
        !self.atr.is_empty()
    }
}

/// The one in-flight APDU (pending queue of size 1).
struct InFlight {
    ccid_seq: u8,
    apdu_id: u64,
    generation: u64,
    extensions: u32,
    deadline: tokio::time::Instant,
}

/// Session state for one imported client: single event-state owner.
struct Session {
    card: Option<CardState>,
    present: bool,
    /// CCID slot power state: active after PowerOn, inactive after PowerOff
    /// (and initially inactive). Drives busy/absent failure replies.
    slot_active: bool,
    in_flight: Option<InFlight>,
    /// Completed CCID messages waiting for a bulk-IN URB (bounded FIFO).
    bulk_queue: VecDeque<Vec<u8>>,
    /// Coalesced pending interrupt-IN notification (latest wins).
    intr_notify: Option<Vec<u8>>,
    overflowed: bool,
    /// Parked bulk-IN submit seqnums waiting for data (bounded FIFO).
    bulk_wait: VecDeque<(u32, usize)>,
    /// Parked interrupt-IN submit seqnums (bounded FIFO).
    intr_wait: VecDeque<(u32, usize)>,
    /// Partial CCID message reassembly buffer from fragmented bulk-OUTs.
    ccid_buf: Vec<u8>,
    /// Two-phase abort armed by the EP0 class request: (slot, seq).
    abort_armed: Option<(u8, u8)>,
}

impl Session {
    fn new(initial_card: Option<CardState>) -> Self {
        let present = initial_card.as_ref().map(|c| c.present()).unwrap_or(false);
        Session {
            card: initial_card.clone(),
            present,
            slot_active: false,
            in_flight: None,
            bulk_queue: VecDeque::new(),
            intr_notify: initial_card
                .as_ref()
                .filter(|c| c.present())
                .map(|_| ccid::notify_slot_change(true, true)),
            overflowed: false,
            bulk_wait: VecDeque::new(),
            intr_wait: VecDeque::new(),
            ccid_buf: Vec::new(),
            abort_armed: None,
        }
    }

    fn on_card(&mut self, generation: u64, atr: Vec<u8>) -> bool {
        let was_present = self.present;
        let generation_changed = self
            .card
            .as_ref()
            .is_none_or(|c| c.generation != generation);
        self.card = Some(CardState { generation, atr });
        self.present = self.card.as_ref().unwrap().present();
        self.present != was_present || generation_changed
    }
}

/// Shared per-serve state: the active session's frame sink + latest card.
#[derive(Clone, Default)]
struct Shared {
    active: Arc<Mutex<Option<mpsc::Sender<Frame>>>>,
    latest_card: Arc<Mutex<Option<CardState>>>,
}

/// Per-serve monotonic APDU ids (never reused while served).
#[derive(Clone, Default)]
struct ApduIds(Arc<Mutex<u64>>);

impl ApduIds {
    fn next(&self) -> u64 {
        let mut g = self.0.lock().unwrap();
        let id = *g;
        *g = g.checked_add(1).expect("APDU id exhausted");
        id
    }
}

pub async fn serve(
    listener: TcpListener,
    mut from_phone: mpsc::Receiver<Frame>,
    to_phone: mpsc::Sender<Frame>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let import_active: SharedFlag = Arc::new(AtomicBool::new(false));
    let shared = Shared::default();
    let apdu_ids = ApduIds::default();
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(io::Error::other("USB/IP must bind loopback"));
    }
    let mut connections: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    loop {
        if *shutdown.borrow_and_update() {
            return shutdown_now(&shared, connections).await;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return shutdown_now(&shared, connections).await;
                }
            }
            phone = from_phone.recv() => {
                let frame = match phone {
                    Some(f) => f,
                    None => return shutdown_now(&shared, connections).await,
                };
                if let Frame::Card { generation, atr, .. } = &frame {
                    *shared.latest_card.lock().unwrap() =
                        Some(CardState { generation: *generation, atr: atr.clone() });
                }
                let sender = shared.active.lock().unwrap().clone();
                if let Some(tx) = sender {
                    // try_send: phone backpressure must never stall accept/shutdown.
                    tokio::select! {
                        _ = shutdown.changed() => return shutdown_now(&shared, connections).await,
                        result = tokio::time::timeout(Duration::from_secs(2), tx.send(frame)) => {
                            if result.is_err() { return shutdown_now(&shared, connections).await; }
                        }
                    }
                }
            }
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                if !stream.peer_addr().map(|a| a.ip().is_loopback()).unwrap_or(false) {
                    continue;
                }
                while connections.try_join_next().is_some() {}
                if connections.len() >= MAX_CONNECTIONS {
                    continue;
                }
                let to_phone = to_phone.clone();
                let import_active = import_active.clone();
                let shared = shared.clone();
                let apdu_ids = apdu_ids.clone();
                let mut shutdown_rx = shutdown.clone();
                connections.spawn(async move {
                    let _ = tokio::select! {
                        r = run_connection(stream, to_phone, import_active, shared, apdu_ids) => r,
                        _ = shutdown_rx.changed() => Ok::<(), io::Error>(()),
                    };
                });
            }
        }
    }
}

async fn shutdown_now(
    shared: &Shared,
    mut connections: tokio::task::JoinSet<()>,
) -> io::Result<()> {
    *shared.active.lock().unwrap() = None;
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

/// Handle one TCP connection: OP phase, then the URB event loop.
async fn run_connection(
    mut stream: tokio::net::TcpStream,
    to_phone: mpsc::Sender<Frame>,
    import_active: SharedFlag,
    shared: Shared,
    apdu_ids: ApduIds,
) -> io::Result<()> {
    {
        let op = read_op_request(&mut stream).await?;
        match op {
            OpHeader::ReqDevlist => {
                stream.write_all(&usbip::devlist()).await?;
                Ok(())
            }
            OpHeader::ReqImport { ref busid } if busid == BUSID => {
                if import_active.swap(true, Ordering::SeqCst) {
                    stream
                        .write_all(&usbip::encode_op_rep_import_error())
                        .await?;
                    return Ok(());
                }
                let _import_guard = ImportGuard(import_active.clone());
                stream
                    .write_all(&usbip::encode_op_rep_import(EXPORTED_DEVID))
                    .await?;
                let result = imported_session(stream, to_phone, shared, apdu_ids).await;
                import_active.store(false, Ordering::SeqCst);
                result
            }
            _ => {
                stream
                    .write_all(&usbip::encode_op_rep_import_error())
                    .await?;
                Ok(())
            }
        }
    }
}

async fn imported_session(
    stream: tokio::net::TcpStream,
    to_phone: mpsc::Sender<Frame>,
    shared: Shared,
    apdu_ids: ApduIds,
) -> io::Result<()> {
    let (session_tx, mut session_rx) = mpsc::channel::<Frame>(16);
    {
        let mut slot = shared.active.lock().unwrap();
        // Replace any stale registration: only one session at a time exists
        // (import_active), so a leftover entry is dead.
        *slot = Some(session_tx.clone());
    }
    let latest_card = shared.latest_card.lock().unwrap().clone();
    let result = urb_loop(stream, to_phone, &mut session_rx, apdu_ids, latest_card).await;
    let mut slot = shared.active.lock().unwrap();
    if slot
        .as_ref()
        .map(|tx| tx.same_channel(&session_tx))
        .unwrap_or(false)
    {
        *slot = None;
    }
    result
}

/// Parsed command forwarded from the dedicated reader task.
enum CmdEvent {
    Submit {
        seqnum: u32,
        dir_in: bool,
        ep: u32,
        buffer_length: u32,
        setup: Option<[u8; 8]>,
        payload: Vec<u8>,
    },
    Unlink {
        seqnum: u32,
        target: u32,
    },
}

async fn urb_loop(
    stream: tokio::net::TcpStream,
    to_phone: mpsc::Sender<Frame>,
    session_rx: &mut mpsc::Receiver<Frame>,
    apdu_ids: ApduIds,
    initial_card: Option<CardState>,
) -> io::Result<()> {
    let mut session = Session::new(initial_card);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<CmdEvent>(MAX_BULK_QUEUE);
    let (mut stream_read, mut stream_write) = stream.into_split();

    // Dedicated packet reader: reads whole standard packets only, forwards
    // events, dies on any error (which ends the session). Never restarts a
    // partial read.
    let reader = tokio::spawn(async move {
        let cmd_tx = cmd_tx;
        loop {
            let mut hdr = [0u8; usbip::CMD_SUBMIT_SIZE];
            if stream_read.read_exact(&mut hdr).await.is_err() {
                return;
            }
            match u32::from_be_bytes(hdr[0..4].try_into().unwrap()) {
                usbip::CMD_SUBMIT => {
                    let submit = match usbip::parse_cmd_submit(&hdr) {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    // After import, all URBs carry our exported devid.
                    if submit.devid != EXPORTED_DEVID {
                        return;
                    }
                    let setup = if submit.ep == 0 {
                        let mut s = [0u8; 8];
                        s.copy_from_slice(&hdr[40..48]);
                        Some(s)
                    } else {
                        None
                    };
                    let mut payload = Vec::new();
                    if !submit.dir_in {
                        let len = submit.buffer_length as usize;
                        if len > MAX_OUT_PAYLOAD {
                            return;
                        }
                        payload = vec![0u8; len];
                        if stream_read.read_exact(&mut payload).await.is_err() {
                            return;
                        }
                    }
                    if cmd_tx
                        .send(CmdEvent::Submit {
                            seqnum: submit.seqnum,
                            dir_in: submit.dir_in,
                            ep: submit.ep,
                            buffer_length: submit.buffer_length,
                            setup,
                            payload,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                usbip::CMD_UNLINK => {
                    let u = match usbip::parse_cmd_unlink(&hdr) {
                        Ok(u) => u,
                        Err(_) => return,
                    };
                    if cmd_tx
                        .send(CmdEvent::Unlink {
                            seqnum: u.seqnum,
                            target: u.unlink_seqnum,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                _ => return,
            }
        }
    });

    let reader = AbortReader(reader);
    let mut deadline_tick = tokio::time::interval(Duration::from_millis(250));
    deadline_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    deadline_tick.reset();

    let result = loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(event) = cmd else { break Ok(()) };
                match event {
                    CmdEvent::Unlink { seqnum, target } => {
                        let was_parked = session.bulk_wait.iter().position(|&(s, _)| s == target)
                            .map(|p| { session.bulk_wait.remove(p); true })
                            .unwrap_or(false)
                            || session.intr_wait.iter().position(|&(s, _)| s == target)
                                .map(|p| { session.intr_wait.remove(p); true })
                                .unwrap_or(false);
                        let status = if was_parked { usbip::ECONNRESET } else { 0 };
                        stream_write
                            .write_all(&usbip::encode_ret_unlink(seqnum, status))
                            .await?;
                    }
                    CmdEvent::Submit { seqnum, dir_in, ep, buffer_length, setup, payload, .. } => {
                        handle_submit(
                            &mut session, seqnum, dir_in, ep, buffer_length, setup,
                            payload, &mut stream_write, &to_phone, &apdu_ids,
                        ).await?;
                    }
                }
            }
            phone = session_rx.recv() => {
                let Some(frame) = phone else { break Ok(()) };
                match frame {
                    Frame::Card { generation, atr, .. } => {
                        if session.on_card(generation, atr) {
                            // Coalesced notification: latest state wins, the
                            // changed bit is always set.
                            session.intr_notify = Some(ccid::notify_slot_change(session.present, true));
                        }
                        // Absence/replacement retires any pending APDU with a
                        // CCID error block (never carried to the next tag).
                        if session.in_flight.as_ref().is_some_and(|inf| !session.present || inf.generation != generation)
                            && let Some(inf) = session.in_flight.take()
                        {
                            queue_bulk(&mut session, error_block(inf.ccid_seq, ccid::ERR_ICC_MUTE));
                        }
                    }
                    Frame::Response { id, generation, data } => {
                        let matches = session
                            .in_flight.as_ref()
                            .is_some_and(|inf| inf.apdu_id == id && inf.generation == generation);
                        if matches && session.present && session.card.as_ref().is_some_and(|c| c.generation == generation)
                            && let Some(inf) = session.in_flight.take()
                        {
                            queue_bulk(&mut session, ccid::data_block(0, inf.ccid_seq, data, false));
                        }
                        // Stale id/generation or absent card: ignored.
                    }
                    Frame::Failed { id, generation } => {
                        let matches = session
                            .in_flight.as_ref()
                            .is_some_and(|inf| inf.apdu_id == id && inf.generation == generation);
                        if matches
                            && let Some(inf) = session.in_flight.take()
                        {
                            queue_bulk(&mut session, error_block(inf.ccid_seq, ccid::ERR_HW_ERROR));
                        }
                    }
                    _ => {}
                }
            }
            _ = deadline_tick.tick() => {
                let expired = session.in_flight.as_ref()
                    .map(|i| tokio::time::Instant::now() >= i.deadline)
                    .unwrap_or(false);
                if expired {
                    let inf = session.in_flight.as_mut().unwrap();
                    inf.extensions += 1;
                    if inf.extensions > MAX_EXTENSIONS {
                        let inf = session.in_flight.take().unwrap();
                        queue_bulk(&mut session, error_block(inf.ccid_seq, ccid::ERR_ICC_MUTE));
                    } else {
                        // CCID time extension: a separate bulk-IN message for
                        // this reply, bStatus time-extension, bError multiplier.
                        let mut p = ccid::data_block(0, inf.ccid_seq, Vec::new(), false);
                        p[7] = 0x80;
                        p[8] = 1;
                        inf.deadline = tokio::time::Instant::now() + APDU_DEADLINE;
                        queue_bulk(&mut session, p);
                    }
                }
            }
        }
        if session.overflowed {
            break Err(io::Error::other("CCID reply queue exceeded limit"));
        }
        // Drain anything now deliverable to parked IN URBs.
        flush_parked(&mut session, &mut stream_write).await?;
    };
    reader.0.abort();
    result
}

/// Queue one bulk-IN response (bounded FIFO; oldest dropped on overflow).
fn queue_bulk(session: &mut Session, msg: Vec<u8>) {
    if msg.is_empty() {
        return;
    }
    if session.bulk_queue.len() >= MAX_BULK_QUEUE {
        session.overflowed = true;
        return;
    }
    session.bulk_queue.push_back(msg);
}

/// Deliver queued data to parked IN URBs after any event.
async fn flush_parked<W: tokio::io::AsyncWrite + Unpin>(
    session: &mut Session,
    w: &mut W,
) -> io::Result<()> {
    while let Some((seq, requested)) = session.bulk_wait.front().copied() {
        let Some(mut msg) = session.bulk_queue.pop_front() else {
            break;
        };
        if msg.len() > requested {
            let tail = msg.split_off(requested);
            session.bulk_queue.push_front(tail);
        }
        session.bulk_wait.pop_front();
        // Cap to the requested length is enforced at submit time via
        // requested_len; parked URBs store only seqnum, so the split happens
        // against the request recorded alongside. See handle_submit.
        let _ = seq;
        write_ret_submit(w, seq, 0, &msg).await?;
    }
    while let Some((seq, requested)) = session.intr_wait.front().copied() {
        let Some(payload) = session.intr_notify.take() else {
            break;
        };
        session.intr_wait.pop_front();
        if requested < payload.len() {
            session.intr_notify = Some(payload);
            write_ret_submit(w, seq, -75, &[]).await?;
        } else {
            write_ret_submit(w, seq, 0, &payload).await?;
        }
    }
    Ok(())
}

async fn write_ret_submit<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    seqnum: u32,
    status: i32,
    payload: &[u8],
) -> io::Result<()> {
    let msg = usbip::encode_ret_submit(seqnum, 0, payload.len() as u32, payload);
    let mut msg = msg;
    if status != 0 {
        msg[20..24].copy_from_slice(&status.to_be_bytes());
    }
    w.write_all(&msg).await
}

#[allow(clippy::too_many_arguments)]
async fn handle_submit<W: tokio::io::AsyncWrite + Unpin>(
    session: &mut Session,
    seqnum: u32,
    dir_in: bool,
    ep: u32,
    buffer_length: u32,
    setup: Option<[u8; 8]>,
    payload: Vec<u8>,
    w: &mut W,
    to_phone: &mpsc::Sender<Frame>,
    apdu_ids: &ApduIds,
) -> io::Result<()> {
    match (ep, dir_in) {
        (0, _) => {
            // EP0 control. Setup rides in the submit header; a data-stage OUT
            // (never used by this device's requests) is acked with no data.
            let Some(setup) = setup else {
                return write_ret_submit(w, seqnum, 0, &[]).await;
            };
            match usb::handle_control(&setup) {
                ControlReply::Data(d) => {
                    // Truncate to the requested buffer_length.
                    let n = (d.len() as u32).min(buffer_length) as usize;
                    write_ret_submit(w, seqnum, 0, &d[..n]).await
                }
                ControlReply::Ok => write_ret_submit(w, seqnum, 0, &[]).await,
                ControlReply::Abort { slot, seq } => {
                    // Two-phase CCID abort, phase 1: arm the bulk phase.
                    session.abort_armed = Some((slot, seq));
                    write_ret_submit(w, seqnum, 0, &[]).await
                }
                ControlReply::Stall => write_ret_submit(w, seqnum, -32, &[]).await, // EPIPE
            }
        }
        (1, false) => {
            // CCID bulk-OUT: acknowledge immediately with the byte count and
            // NO payload, then process the bytes.
            w.write_all(&usbip::encode_ret_submit(
                seqnum,
                0,
                payload.len() as u32,
                &[],
            ))
            .await?;
            // Ack first (write above), so the caller sees it before any IN data.
            session.ccid_buf.extend_from_slice(&payload);
            if session.ccid_buf.len() > ccid::MAX_MESSAGE_LENGTH * 2 {
                session.ccid_buf.clear(); // bounded: malformed flood, reset
                return Ok(());
            }
            let to_phone = to_phone.clone();
            let apdu_ids = apdu_ids.clone();
            let mut drained: Vec<Vec<u8>> = Vec::new();
            loop {
                if session.ccid_buf.len() < 10 {
                    break;
                }
                let dw = u32::from_le_bytes(session.ccid_buf[1..5].try_into().unwrap()) as usize;
                if dw > ccid::MAX_MESSAGE_LENGTH - 10 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "CCID message length exceeds limit",
                    ));
                }
                let total = 10 + dw;
                if session.ccid_buf.len() < total {
                    break; // fragmented: wait for more OUT bytes
                }
                let msg: Vec<u8> = session.ccid_buf.drain(..total).collect();
                match ccid::parse_bulk_out(&msg) {
                    Ok(cmd) => {
                        let reply = handle_ccid(&mut *session, &cmd, &to_phone, &apdu_ids).await;
                        drained.push(reply);
                    }
                    Err(_) => {
                        let seq = msg.get(6).copied().unwrap_or(0);
                        let mut p = ccid::slot_status_msg(0, seq, session.present, None);
                        p[7] = 0x40; // failed
                        p[8] = ccid::ERR_CMD_NOT_SUPPORTED;
                        drained.push(p);
                    }
                }
            }
            for reply in drained {
                queue_bulk(&mut *session, reply);
            }
            flush_parked(&mut *session, w).await
        }
        (2, true) => {
            // Bulk-IN submit: serve from the queue, cap to buffer_length,
            // retain any tail for the next IN; otherwise park the URB.
            let requested = buffer_length as usize;
            if let Some(mut msg) = session.bulk_queue.pop_front() {
                if msg.len() > requested {
                    let tail = msg.split_off(requested);
                    session.bulk_queue.push_front(tail);
                }
                if !msg.is_empty() {
                    write_ret_submit(w, seqnum, 0, &msg).await?;
                } else {
                    // A retained empty tail can only exist if the original
                    // message was empty; reply with actual=0.
                    write_ret_submit(w, seqnum, 0, &[]).await?;
                }
                Ok(())
            } else if session.bulk_wait.len() < MAX_INTR_WAIT * 4 {
                session.bulk_wait.push_back((seqnum, requested));
                Ok(())
            } else {
                write_ret_submit(w, seqnum, -32, &[]).await
            }
        }
        (3, true) => {
            // Interrupt-IN: coalesced notify or park (bounded).
            if let Some(payload) = session.intr_notify.take() {
                write_ret_submit(w, seqnum, 0, &payload).await
            } else if session.intr_wait.len() < MAX_INTR_WAIT {
                session
                    .intr_wait
                    .push_back((seqnum, buffer_length as usize));
                Ok(())
            } else {
                write_ret_submit(w, seqnum, 0, &[]).await
            }
        }
        _ => write_ret_submit(w, seqnum, -32, &[]).await,
    }
}

/// Handle one parsed CCID command; returns the CCID response message to
/// queue for bulk-IN. XfrBlock defers (no immediate reply payload).
async fn handle_ccid(
    session: &mut Session,
    cmd: &ccid::BulkOutCommand,
    to_phone: &mpsc::Sender<Frame>,
    apdu_ids: &ApduIds,
) -> Vec<u8> {
    match cmd.kind {
        BulkOutKind::GetSlotStatus => ccid::slot_status_msg(0, cmd.seq, session.present, None),
        BulkOutKind::IccPowerOn { .. } => {
            // PowerOn activates the slot; the ATR (if any) is the reply data.
            session.slot_active = true;
            match session.card.as_ref().filter(|c| c.present()) {
                Some(card) if ccid::validate_atr(&card.atr).is_ok() => {
                    ccid::data_block(0, cmd.seq, card.atr.clone(), false)
                }
                _ => error_block(cmd.seq, ccid::ERR_ICC_MUTE),
            }
        }
        BulkOutKind::IccPowerOff => {
            // PowerOff deactivates the slot; reply is a SlotStatus.
            session.slot_active = false;
            ccid::slot_status_msg(0, cmd.seq, session.present, None)
        }
        BulkOutKind::GetParameters | BulkOutKind::ResetParameters => {
            if !session.present {
                return error_block(cmd.seq, ccid::ERR_ICC_MUTE);
            }
            ccid::parameters(0, cmd.seq, true, &ccid::default_t1_parameters())
        }
        BulkOutKind::SetParameters { .. } => {
            ccid::parameters(0, cmd.seq, session.present, &ccid::default_t1_parameters())
        }
        BulkOutKind::XfrBlock { .. } => {
            // One APDU in flight; busy otherwise. Absent card fails
            // immediately; the slot must also be powered (active).
            if session.in_flight.is_some() {
                let mut p = ccid::data_block(0, cmd.seq, Vec::new(), true);
                p[7] = 0x40; // present + failed
                p[8] = ccid::ERR_CMD_SLOT_BUSY;
                return p;
            }
            if !session.present {
                let mut p = ccid::data_block(0, cmd.seq, Vec::new(), true);
                p[7] = 0x42; // absent + failed
                p[8] = ccid::ERR_ICC_MUTE;
                return p;
            }
            let generation = session.card.as_ref().map(|c| c.generation).unwrap_or(0);
            let apdu_id = apdu_ids.next();
            session.in_flight = Some(InFlight {
                ccid_seq: cmd.seq,
                apdu_id,
                generation,
                extensions: 0,
                deadline: tokio::time::Instant::now() + APDU_DEADLINE,
            });
            let sent = to_phone.try_send(Frame::Apdu {
                id: apdu_id,
                generation,
                data: cmd.ab_data.clone(),
            });
            if sent.is_err() {
                session.in_flight = None;
                return error_block(cmd.seq, ccid::ERR_ICC_MUTE);
            }
            // The response arrives later as an independent bulk-IN message.
            Vec::new()
        }
        BulkOutKind::Abort => {
            // Phase 2: valid only when the EP0 class request armed the same
            // slot/seq.
            let armed = session.abort_armed.take();
            let ok = matches!(armed, Some((s, q)) if s == cmd.slot && q == cmd.seq);
            if ok {
                if session
                    .in_flight
                    .as_ref()
                    .is_some_and(|inf| inf.ccid_seq == cmd.seq)
                    && let Some(inf) = session.in_flight.take()
                {
                    let _ = to_phone.try_send(Frame::Cancel { id: inf.apdu_id });
                    let mut p = ccid::data_block(0, inf.ccid_seq, Vec::new(), true);
                    p[7] = 0x40;
                    p[8] = ccid::ERR_CMD_ABORTED;
                    queue_bulk(session, p);
                }
                ccid::slot_status_msg(0, cmd.seq, session.present, None)
            } else {
                let mut p = ccid::slot_status_msg(0, cmd.seq, session.present, None);
                p[7] = 0x40; // CMD_NOT_ABORTED
                p
            }
        }
        _ => {
            let mut p = ccid::slot_status_msg(0, cmd.seq, session.present, None);
            p[7] = 0x40;
            p[8] = ccid::ERR_CMD_NOT_SUPPORTED;
            p
        }
    }
}

fn error_block(ccid_seq: u8, err: u8) -> Vec<u8> {
    let mut p = ccid::data_block(0, ccid_seq, Vec::new(), true);
    p[7] = 0x42; // absent (0b10) + failed (0b01 << 6)
    p[8] = err;
    p
}

/// Read one OP request: 8-byte common header; for REQ_IMPORT, 32 more.
async fn read_op_request(stream: &mut tokio::net::TcpStream) -> io::Result<OpHeader> {
    let mut head = [0u8; 8];
    stream.read_exact(&mut head).await?;
    if head[..2] != usbip::USBIP_VERSION.to_be_bytes() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad usbip version",
        ));
    }
    let cmd = u16::from_be_bytes([head[2], head[3]]);
    match cmd {
        usbip::OP_REQ_DEVLIST => Ok(OpHeader::ReqDevlist),
        usbip::OP_REQ_IMPORT => {
            let mut busid = [0u8; 32];
            stream.read_exact(&mut busid).await?;
            let end = busid.iter().position(|&b| b == 0).unwrap_or(32);
            if end == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "empty busid"));
            }
            Ok(OpHeader::ReqImport {
                busid: String::from_utf8_lossy(&busid[..end]).into_owned(),
            })
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unknown OP")),
    }
}
