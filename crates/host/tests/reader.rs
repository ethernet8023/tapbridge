//! Black-box protocol tests against the public `serve`, using raw STANDARD
//! USB/IP bytes (https://docs.kernel.org/usb/usbip_protocol.html) with no
//! shared helper that could encode the same wrong assumption. Big-endian.
//! Each test binds a real TCP listener, runs serve(), and drives it with a
//! minimal USB/IP protocol client. No real card, no real authentication.

use std::time::Duration;
use tapbridge_core::wire::Frame;
use tapbridge_host::{ccid, serve, usb, usbip};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

const DEVID: u32 = 0x0001_0001; // (busnum 1 << 16) | devnum 1

struct Harness {
    addr: std::net::SocketAddr,
    phone_tx: mpsc::Sender<Frame>,
    phone_rx: mpsc::Receiver<Frame>,
    shutdown: watch::Sender<bool>,
}

async fn harness() -> Harness {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (phone_tx, phone_rx) = mpsc::channel::<Frame>(16);
    let (host_tx, host_rx) = mpsc::channel::<Frame>(16);
    let (shutdown, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        let _ = serve(listener, host_rx, phone_tx, shutdown_rx).await;
    });
    Harness {
        addr,
        phone_tx: host_tx,
        phone_rx,
        shutdown,
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

struct Client {
    stream: TcpStream,
    out_sequences: std::collections::HashSet<u32>,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Client {
        Client {
            stream: TcpStream::connect(addr).await.unwrap(),
            out_sequences: std::collections::HashSet::new(),
        }
    }

    async fn send(&mut self, bytes: &[u8]) {
        if bytes.len() >= 48 && bytes[..4] == 1u32.to_be_bytes() && bytes[12..16] == [0; 4] {
            self.out_sequences
                .insert(u32::from_be_bytes(bytes[4..8].try_into().unwrap()));
        }
        self.stream.write_all(bytes).await.unwrap();
    }

    async fn read(&mut self, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        tokio::time::timeout(Duration::from_secs(5), self.stream.read_exact(&mut out))
            .await
            .expect("timeout waiting for usbip reply")
            .unwrap();
        out
    }

    /// OP_REQ_DEVLIST with raw standard bytes; returns the whole reply.
    async fn devlist(&mut self) -> Vec<u8> {
        let req: [u8; 8] = [0x01, 0x11, 0x80, 0x05, 0, 0, 0, 0];
        self.send(&req).await;
        // 12-byte reply head + one 312-byte device + 4-byte interface.
        self.read(12 + 312 + 4).await
    }

    /// OP_REQ_IMPORT for busid 1-1; asserts success and returns the 320-byte
    /// reply.
    async fn import(&mut self) -> Vec<u8> {
        let mut req = vec![0x01u8, 0x11, 0x80, 0x03, 0, 0, 0, 0];
        let mut busid = [0u8; 32];
        busid[..3].copy_from_slice(b"1-1");
        req.extend_from_slice(&busid);
        self.send(&req).await;
        let m = self.read(320).await;
        assert_eq!(&m[2..4], &[0x00, 0x03], "OP_REP_IMPORT");
        let status = u32::from_be_bytes([m[4], m[5], m[6], m[7]]);
        assert_eq!(status, 0, "import must succeed");
        assert_eq!(m.len(), 320, "import reply = common(8) + usb_device(312)");
        m
    }

    /// Raw CMD_SUBMIT header: cmd at 0, seq 4, devid 8, dir 12, ep 16,
    /// flags 20, buffer_length 24. `payload` is appended for OUT.
    fn submit_hdr(seqnum: u32, dir_in: u32, ep: u32, buffer_length: u32) -> [u8; 48] {
        let mut hdr = [0u8; 48];
        hdr[0..4].copy_from_slice(&1u32.to_be_bytes());
        hdr[4..8].copy_from_slice(&seqnum.to_be_bytes());
        hdr[8..12].copy_from_slice(&DEVID.to_be_bytes());
        hdr[12..16].copy_from_slice(&dir_in.to_be_bytes());
        hdr[16..20].copy_from_slice(&ep.to_be_bytes());
        hdr[24..28].copy_from_slice(&buffer_length.to_be_bytes());
        hdr
    }

    /// Fire a bulk-OUT submit without waiting for its reply.
    async fn bulk_send(&mut self, seqnum: u32, data: &[u8]) {
        let mut msg = Self::submit_hdr(seqnum, 0, 1, data.len() as u32).to_vec();
        msg.extend_from_slice(data); // OUT payload follows the header
        self.send(&msg).await;
    }

    /// Read one RET_SUBMIT (48-byte head + `actual` payload bytes, payload
    /// present only when actual > 0).
    async fn ret_submit(&mut self) -> (Vec<u8>, Vec<u8>) {
        let head = self.read(48).await;
        let cmd = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        assert_eq!(cmd, 3, "RET_SUBMIT command");
        let actual = u32::from_be_bytes([head[24], head[25], head[26], head[27]]) as usize;
        let seq = u32::from_be_bytes(head[4..8].try_into().unwrap());
        let data = if actual > 0 && !self.out_sequences.remove(&seq) {
            self.read(actual).await
        } else {
            Vec::new()
        };
        (head, data)
    }

    /// EP0 control IN: setup rides at header offset 40.
    async fn control_in(&mut self, seqnum: u32, setup: [u8; 8]) -> (Vec<u8>, Vec<u8>) {
        let mut hdr = Self::submit_hdr(seqnum, 1, 0, 255);
        hdr[40..48].copy_from_slice(&setup);
        self.send(&hdr).await;
        self.ret_submit().await
    }

    /// Park an interrupt-IN submit (no reply expected yet).
    async fn intr_submit(&mut self, seqnum: u32) {
        self.send(&Self::submit_hdr(seqnum, 1, 3, 8)).await;
    }

    /// CMD_UNLINK: command 2 at 0, own seq at 4, target at 20. Returns the
    /// status from RET_UNLINK (command 4, status at 20).
    async fn unlink(&mut self, seqnum: u32, target: u32) -> i32 {
        let mut hdr = [0u8; 48];
        hdr[0..4].copy_from_slice(&2u32.to_be_bytes());
        hdr[4..8].copy_from_slice(&seqnum.to_be_bytes());
        hdr[20..24].copy_from_slice(&target.to_be_bytes());
        self.send(&hdr).await;
        let head = self.read(48).await;
        assert_eq!(&head[0..4], &4u32.to_be_bytes(), "RET_UNLINK command");
        i32::from_be_bytes([head[20], head[21], head[22], head[23]])
    }
}

fn bulk_out_msg(msg_type: u8, seq: u8, data: &[u8]) -> Vec<u8> {
    // 10-byte CCID bulk-OUT header: type, dwLength(LE), slot, seq, 3 bytes
    // msg-specific, then abData.
    let mut m = vec![msg_type];
    m.extend_from_slice(&(data.len() as u32).to_le_bytes());
    m.push(0); // slot
    m.push(seq);
    m.extend_from_slice(&[0u8; 3]);
    m.extend_from_slice(data);
    m
}

fn get_slot_status(seq: u8) -> Vec<u8> {
    bulk_out_msg(ccid::GET_SLOT_STATUS, seq, &[])
}

/// T=1 ATR: TS, T0=0x80 (TD1 only), TD1=0x80 (TD2), TD2=0x01 (T=1), TCK=1.
const ATR: &[u8] = &[0x3B, 0x80, 0x80, 0x01, 0x01];

async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn devlist_is_328_bytes_standard_layout() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    let m = c.devlist().await;
    assert_eq!(m.len(), 12 + 312 + 4, "head + device + 1 interface");
    assert_eq!(&m[2..4], &0x0005u16.to_be_bytes(), "OP_REP_DEVLIST");
    assert_eq!(u32::from_be_bytes([m[8], m[9], m[10], m[11]]), 1);
    let busid = &m[12 + 256..12 + 256 + 32];
    assert_eq!(&busid[..3], b"1-1");
    let vid = u16::from_be_bytes([m[12 + 288 + 12], m[12 + 288 + 13]]);
    assert_eq!(vid, 0x1209);
    assert_eq!(m[12 + 312], 0x0B, "smart-card interface class");
}

#[tokio::test]
async fn import_reply_is_320_bytes_no_descriptor() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    let m = c.import().await;
    // The USB descriptors are NOT on the wire; they come over EP0 later.
    assert_ne!(m[8], 0x12, "no device descriptor inlined");
    assert_eq!(&m[8 + 256..8 + 259], b"1-1", "busid inside usb_device");
}

#[tokio::test]
async fn ep0_control_get_config_descriptor() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    // GET_DESCRIPTOR CONFIG, setup in the submit header.
    let setup: [u8; 8] = [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 0xFF, 0x00];
    let (head, data) = c.control_in(1, setup).await;
    let status = i32::from_be_bytes([head[20], head[21], head[22], head[23]]);
    assert_eq!(status, 0);
    assert_eq!(data[1], 0x02, "config descriptor type");
    assert_eq!(
        data.len(),
        93,
        "config total length (54-byte CCID class desc)"
    );
    assert_eq!(usb::id_vendor(), 0x1209);
}

#[tokio::test]
async fn ep0_control_out_acknowledged_with_no_data() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    // SET_CONFIGURATION (a standard OUT request): zero-length ack.
    let setup: [u8; 8] = [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
    let mut hdr = Client::submit_hdr(2, 0, 0, 0);
    hdr[40..48].copy_from_slice(&setup);
    c.send(&hdr).await;
    let (head, data) = c.ret_submit().await;
    assert_eq!(
        i32::from_be_bytes([head[20], head[21], head[22], head[23]]),
        0
    );
    assert_eq!(
        u32::from_be_bytes([head[24], head[25], head[26], head[27]]),
        0
    );
    assert!(data.is_empty(), "EP0 OUT ack carries no payload");
}

#[tokio::test]
async fn second_import_rejected() {
    let h = harness().await;
    let mut c1 = Client::connect(h.addr).await;
    c1.import().await;
    let mut c2 = Client::connect(h.addr).await;
    let mut req = vec![0x01u8, 0x11, 0x80, 0x03, 0, 0, 0, 0];
    let mut busid = [0u8; 32];
    busid[..3].copy_from_slice(b"1-1");
    req.extend_from_slice(&busid);
    c2.send(&req).await;
    let head = c2.read(8).await;
    let status = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
    assert_ne!(status, 0, "second import must be rejected");
}

#[tokio::test]
async fn bulk_out_acked_immediately_then_in_reply_independent() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    h.phone_tx
        .send(Frame::Card {
            generation: 1,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    settle().await;
    // 1. GetSlotStatus OUT: acked immediately with actual=0, NO payload.
    c.bulk_send(10, &get_slot_status(1)).await;
    let (head, data) = c.ret_submit().await;
    assert_eq!(
        u32::from_be_bytes([head[24], head[25], head[26], head[27]]),
        10,
        "bulk OUT reports transferred bytes without payload"
    );
    assert_eq!(
        i32::from_be_bytes([head[20], head[21], head[22], head[23]]),
        0
    );
    assert!(data.is_empty());
    // 2. The CCID response arrives on an independent bulk-IN submit.
    c.send(&Client::submit_hdr(11, 1, 2, 256)).await;
    let (head, data) = c.ret_submit().await;
    assert_eq!(data[0], ccid::RDR_SLOT_STATUS);
    assert_eq!(data[6], 1, "bSeq preserved");
    assert_eq!(data[7] & 0x03, 0x00, "present");
    let _ = head;
}

#[tokio::test]
async fn apdu_roundtrip_out_ack_then_in_data_block() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    h.phone_tx
        .send(Frame::Card {
            generation: 1,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    settle().await;
    let apdu = [
        0x00u8, 0xA4, 0x04, 0x00, 0x07, 0xA0, 0x00, 0x00, 0x05, 0x27, 0x47, 0x11,
    ];
    let xfr = bulk_out_msg(ccid::XFR_BLOCK, 7, &apdu);
    c.bulk_send(20, &xfr).await;
    // OUT acked with byte count, no payload.
    let (ack, ack_data) = c.ret_submit().await;
    assert_eq!(
        u32::from_be_bytes([ack[24], ack[25], ack[26], ack[27]]),
        xfr.len() as u32,
        "OUT reports transferred bytes without payload"
    );
    assert!(ack_data.is_empty());
    // Phone receives the exact APDU with our generation.
    let frame = h.phone_rx.recv().await.expect("phone should get Apdu");
    let (id, generation, data) = match frame {
        Frame::Apdu {
            id,
            generation,
            data,
        } => (id, generation, data),
        other => panic!("expected Apdu, got {:?}", other),
    };
    assert_eq!(generation, 1);
    assert_eq!(data, apdu.to_vec());
    h.phone_tx
        .send(Frame::Response {
            id,
            generation,
            data: vec![0x61, 0x05],
        })
        .await
        .unwrap();
    // Reply arrives on a bulk-IN submit, capped to requested length.
    c.send(&Client::submit_hdr(21, 1, 2, 256)).await;
    let (_head, out) = c.ret_submit().await;
    assert_eq!(out[0], ccid::RDR_DATA_BLOCK);
    assert_eq!(out[6], 7, "bSeq preserved");
    assert_eq!(&out[10..12], &[0x61, 0x05], "exact bytes incl SW");
}

#[tokio::test]
async fn card_present_before_import_seen_after_import() {
    let h = harness().await;
    h.phone_tx
        .send(Frame::Card {
            generation: 1,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    settle().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    settle().await;
    c.bulk_send(10, &get_slot_status(1)).await;
    let _ = c.ret_submit().await;
    c.send(&Client::submit_hdr(11, 1, 2, 256)).await;
    let (_head, data) = c.ret_submit().await;
    assert_eq!(
        data[7] & 0x03,
        0x00,
        "card present before import must be seen"
    );
}

#[tokio::test]
async fn late_generation_response_is_ignored() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    h.phone_tx
        .send(Frame::Card {
            generation: 5,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    settle().await;
    let xfr = bulk_out_msg(ccid::XFR_BLOCK, 3, &[0x00, 0xA4, 0x04, 0x00]);
    c.bulk_send(40, &xfr).await;
    let _ = c.ret_submit().await; // OUT ack
    let frame = h.phone_rx.recv().await.unwrap();
    let (id, generation) = match frame {
        Frame::Apdu { id, generation, .. } => (id, generation),
        other => panic!("expected Apdu, got {:?}", other),
    };
    assert_eq!(generation, 5);
    // Stale response from an earlier generation must NOT satisfy the APDU.
    h.phone_tx
        .send(Frame::Response {
            id,
            generation: 4,
            data: vec![0x90, 0x00],
        })
        .await
        .unwrap();
    // Real response with the right generation.
    h.phone_tx
        .send(Frame::Response {
            id,
            generation: 5,
            data: vec![0x6A, 0x82],
        })
        .await
        .unwrap();
    c.send(&Client::submit_hdr(41, 1, 2, 256)).await;
    let (_head, data) = c.ret_submit().await;
    assert_eq!(
        &data[10..12],
        &[0x6A, 0x82],
        "stale reply must not satisfy the XfrBlock"
    );
}

#[tokio::test]
async fn ccid_abort_two_phase() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    h.phone_tx
        .send(Frame::Card {
            generation: 1,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    settle().await;
    // Phase 1: EP0 class ABORT (slot 0, seq 5). wValue low=slot, high=seq.
    let setup: [u8; 8] = [0x21, 0x01, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00];
    let (head, _) = c.control_in(50, setup).await;
    assert_eq!(
        i32::from_be_bytes([head[20], head[21], head[22], head[23]]),
        0
    );
    // Phase 2: bulk PC_to_RDR_Abort with the same slot/seq.
    let abort = bulk_out_msg(0x72, 5, &[]);
    c.bulk_send(51, &abort).await;
    let _ = c.ret_submit().await; // OUT ack
    c.send(&Client::submit_hdr(52, 1, 2, 256)).await;
    let (_head, data) = c.ret_submit().await;
    assert_eq!(data[0], ccid::RDR_SLOT_STATUS);
    assert_eq!(data[6], 5, "abort reply carries abort's own seq");
}

#[tokio::test]
async fn interrupt_in_and_unlink() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    // Park an interrupt-IN URB; a GetSlotStatus in parallel proves liveness.
    c.intr_submit(60).await;
    c.bulk_send(61, &get_slot_status(6)).await;
    let _ = c.ret_submit().await;
    c.send(&Client::submit_hdr(62, 1, 2, 256)).await;
    let (_head, data) = c.ret_submit().await;
    assert_eq!(data[0], ccid::RDR_SLOT_STATUS);
    // Card arrives: the parked URB completes with the 2-byte notify
    // (present bit0, changed bit1).
    h.phone_tx
        .send(Frame::Card {
            generation: 1,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    let head = tokio::time::timeout(Duration::from_secs(3), c.read(48))
        .await
        .expect("timeout waiting for notify");
    let actual = u32::from_be_bytes([head[24], head[25], head[26], head[27]]);
    assert_eq!(actual, 2);
    let notify = c.read(2).await;
    assert_eq!(notify, vec![0x50, 0b11]);

    // Park again, then unlink: RET_UNLINK with ECONNRESET and status at 20.
    c.intr_submit(63).await;
    let status = c.unlink(64, 63).await;
    assert_eq!(status, usbip::ECONNRESET);
    // Unlinking a non-pending seq returns 0.
    let status = c.unlink(65, 999).await;
    assert_eq!(status, 0);
}

#[tokio::test]
async fn busy_slot_returns_ccid_busy() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    h.phone_tx
        .send(Frame::Card {
            generation: 1,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    settle().await;
    let xfr1 = bulk_out_msg(ccid::XFR_BLOCK, 1, &[0x00, 0xA4, 0x04, 0x00]);
    c.bulk_send(70, &xfr1).await;
    let _ = c.ret_submit().await;
    let frame = h.phone_rx.recv().await.unwrap();
    let id = match frame {
        Frame::Apdu { id, .. } => id,
        other => panic!("expected Apdu, got {:?}", other),
    };
    // Second XfrBlock while busy: immediate CCID CMD_SLOT_BUSY reply.
    let xfr2 = bulk_out_msg(ccid::XFR_BLOCK, 2, &[0x00, 0xA4, 0x04, 0x00]);
    c.bulk_send(71, &xfr2).await;
    let _ = c.ret_submit().await;
    c.send(&Client::submit_hdr(72, 1, 2, 256)).await;
    let (_head, data) = c.ret_submit().await;
    assert_eq!(data[7] >> 6, 1, "failed");
    assert_eq!(data[8], ccid::ERR_CMD_SLOT_BUSY);
    // Finish the first.
    h.phone_tx
        .send(Frame::Response {
            id,
            generation: 1,
            data: vec![0x90, 0x00],
        })
        .await
        .unwrap();
    c.send(&Client::submit_hdr(73, 1, 2, 256)).await;
    let (_head, data1) = c.ret_submit().await;
    assert_eq!(&data1[10..12], &[0x90, 0x00]);
}

#[tokio::test]
async fn fragmented_ccid_across_out_urbs() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    // One GetSlotStatus split across two bulk-OUT submits.
    let msg = get_slot_status(1);
    let (first, second) = msg.split_at(4);
    c.bulk_send(80, first).await;
    let _ = c.ret_submit().await;
    c.bulk_send(81, second).await;
    let _ = c.ret_submit().await;
    // Only now can the reassembled command complete on bulk-IN.
    c.send(&Client::submit_hdr(82, 1, 2, 256)).await;
    let (_head, data) = c.ret_submit().await;
    assert_eq!(data[0], ccid::RDR_SLOT_STATUS);
    assert_eq!(data[6], 1);
}

#[tokio::test]
async fn coalesced_ccid_in_one_out_urb() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    // Two GetSlotStatus messages inside one bulk-OUT submit.
    let m1 = get_slot_status(1);
    let m2 = get_slot_status(2);
    let mut both = m1.clone();
    both.extend_from_slice(&m2);
    c.bulk_send(90, &both).await;
    let _ = c.ret_submit().await;
    // First bulk-IN gets the first message, second gets the second.
    c.send(&Client::submit_hdr(91, 1, 2, 256)).await;
    let (_h1, d1) = c.ret_submit().await;
    assert_eq!(d1[6], 1);
    c.send(&Client::submit_hdr(92, 1, 2, 256)).await;
    let (_h2, d2) = c.ret_submit().await;
    assert_eq!(d2[6], 2);
}

#[tokio::test]
async fn parked_small_in_keeps_tail_after_later_reply() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    c.send(&Client::submit_hdr(501, 1, 2, 4)).await;
    c.bulk_send(502, &get_slot_status(9)).await;
    let mut pieces = Vec::new();
    for _ in 0..2 {
        let (head, data) = c.ret_submit().await;
        if u32::from_be_bytes(head[4..8].try_into().unwrap()) == 501 {
            assert_eq!(data.len(), 4);
            pieces.extend(data);
        }
    }
    c.send(&Client::submit_hdr(503, 1, 2, 256)).await;
    pieces.extend(c.ret_submit().await.1);
    assert_eq!(pieces.len(), 10);
    assert_eq!(pieces[6], 9);
}

#[tokio::test]
async fn replacement_retires_pending_request_and_drops_late_reply() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    h.phone_tx
        .send(Frame::Card {
            generation: 1,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    settle().await;
    c.bulk_send(600, &bulk_out_msg(ccid::XFR_BLOCK, 1, &[0, 0xa4, 4, 0]))
        .await;
    c.ret_submit().await;
    let Frame::Apdu { id, .. } = h.phone_rx.recv().await.unwrap() else {
        panic!("APDU")
    };
    h.phone_tx
        .send(Frame::Card {
            generation: 2,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    h.phone_tx
        .send(Frame::Response {
            id,
            generation: 1,
            data: vec![0x90, 0],
        })
        .await
        .unwrap();
    c.send(&Client::submit_hdr(601, 1, 2, 256)).await;
    let error = c.ret_submit().await.1;
    assert_eq!(error[7] >> 6, 1);
    assert_eq!(error.len(), 10);
}

#[tokio::test]
async fn in_reply_capped_to_requested_length_with_tail_retained() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    h.phone_tx
        .send(Frame::Card {
            generation: 1,
            atr: ATR.to_vec(),
            max_apdu: 261,
        })
        .await
        .unwrap();
    settle().await;
    let xfr = bulk_out_msg(ccid::XFR_BLOCK, 5, &[0x00, 0xA4, 0x04, 0x00]);
    c.bulk_send(100, &xfr).await;
    let _ = c.ret_submit().await;
    let f = h.phone_rx.recv().await.unwrap();
    let (id, generation) = match f {
        Frame::Apdu { id, generation, .. } => (id, generation),
        other => panic!("expected Apdu, got {:?}", other),
    };
    h.phone_tx
        .send(Frame::Response {
            id,
            generation,
            data: vec![0x61, 0x05],
        })
        .await
        .unwrap();
    // Request only 6 bytes of the 12-byte DataBlock: exactly 6 arrive.
    c.send(&Client::submit_hdr(101, 1, 2, 6)).await;
    let (head, part) = c.ret_submit().await;
    assert_eq!(
        u32::from_be_bytes([head[24], head[25], head[26], head[27]]),
        6,
        "never send more than requested"
    );
    assert_eq!(part.len(), 6);
    // The tail is retained and delivered on the next IN.
    c.send(&Client::submit_hdr(102, 1, 2, 256)).await;
    let (_head, rest) = c.ret_submit().await;
    assert_eq!(rest.len(), 6, "tail retained for next IN");
    assert_eq!(&rest[4..6], &[0x61, 0x05], "SW bytes are in the tail");
}

#[tokio::test]
async fn shutdown_closes_server() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    h.shutdown.send(true).unwrap();
    let mut buf = [0u8; 16];
    let res = tokio::time::timeout(Duration::from_secs(5), c.stream.read(&mut buf)).await;
    match res {
        Ok(Ok(0)) => {}
        Ok(Err(_)) => {}
        Ok(Ok(_)) => panic!("unexpected data after shutdown"),
        Err(_) => panic!("server did not close on shutdown"),
    }
}
