//! Black-box integration tests for reader lifetime/regression behavior,
//! written against the STANDARD USB/IP wire format (big-endian, 48-byte
//! headers). No real card, no real authentication.

use std::io;
use std::time::Duration;
use tapbridge_core::wire::Frame;
use tapbridge_host::{ccid, serve};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

const DEVID: u32 = 0x0001_0001;

struct Harness {
    addr: std::net::SocketAddr,
    phone_tx: mpsc::Sender<Frame>,
    phone_rx: mpsc::Receiver<Frame>,
    shutdown: watch::Sender<bool>,
    serve_jh: Option<tokio::task::JoinHandle<io::Result<()>>>,
}

async fn harness() -> Harness {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (phone_tx, phone_rx) = mpsc::channel::<Frame>(16);
    let (host_tx, host_rx) = mpsc::channel::<Frame>(16);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let serve_jh =
        tokio::spawn(async move { serve(listener, host_rx, phone_tx, shutdown_rx).await });
    Harness {
        addr,
        phone_tx: host_tx,
        phone_rx,
        shutdown,
        serve_jh: Some(serve_jh),
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(jh) = self.serve_jh.take() {
            jh.abort();
        }
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

    async fn import(&mut self) {
        let mut req = vec![0x01u8, 0x11, 0x80, 0x03, 0, 0, 0, 0];
        let mut busid = [0u8; 32];
        busid[..3].copy_from_slice(b"1-1");
        req.extend_from_slice(&busid);
        self.send(&req).await;
        let head = self.read(320).await;
        let status = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
        assert_eq!(status, 0, "import must succeed");
    }

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

    async fn bulk_send(&mut self, seqnum: u32, data: &[u8]) {
        let mut msg = Self::submit_hdr(seqnum, 0, 1, data.len() as u32).to_vec();
        msg.extend_from_slice(data);
        self.send(&msg).await;
    }

    /// Read one RET_SUBMIT; returns (head, payload).
    async fn ret_submit(&mut self) -> (Vec<u8>, Vec<u8>) {
        let head = self.read(48).await;
        let actual = u32::from_be_bytes([head[24], head[25], head[26], head[27]]) as usize;
        let seq = u32::from_be_bytes(head[4..8].try_into().unwrap());
        let data = if actual > 0 && !self.out_sequences.remove(&seq) {
            self.read(actual).await
        } else {
            Vec::new()
        };
        (head, data)
    }

    async fn bulk_in(&mut self, seqnum: u32, len: u32) -> (Vec<u8>, Vec<u8>) {
        self.send(&Self::submit_hdr(seqnum, 1, 2, len)).await;
        self.ret_submit().await
    }

    async fn intr_submit(&mut self, seqnum: u32) {
        self.send(&Self::submit_hdr(seqnum, 1, 3, 8)).await;
    }
}

fn bulk_out_msg(msg_type: u8, seq: u8, data: &[u8]) -> Vec<u8> {
    let mut m = vec![msg_type];
    m.extend_from_slice(&(data.len() as u32).to_le_bytes());
    m.push(0);
    m.push(seq);
    m.extend_from_slice(&[0u8; 3]);
    m.extend_from_slice(data);
    m
}

fn get_slot_status(seq: u8) -> Vec<u8> {
    bulk_out_msg(ccid::GET_SLOT_STATUS, seq, &[])
}

const ATR: &[u8] = &[0x3B, 0x00];

async fn card(h: &Harness, generation: u64, atr: &[u8]) {
    h.phone_tx
        .send(Frame::Card {
            generation,
            atr: atr.to_vec(),
            max_apdu: 261,
            uid: vec![],
        })
        .await
        .unwrap();
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

// 1. Card present BEFORE import must be visible after import, and a
//    replacement generation fires a notify even without a presence edge.
#[tokio::test]
async fn card_present_before_import_seen_after_import() {
    let h = harness().await;
    card(&h, 1, ATR).await;
    settle().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    settle().await;
    c.bulk_send(10, &get_slot_status(1)).await;
    let _ = c.ret_submit().await;
    let (_head, data) = c.bulk_in(11, 256).await;
    assert_eq!(
        data[7] & 0x03,
        0x00,
        "card present before import must be seen"
    );
    // Replacement: new generation, same presence -> notify on parked IN URB.
    c.intr_submit(12).await;
    card(&h, 2, ATR).await;
    let head = tokio::time::timeout(Duration::from_secs(3), c.read(48))
        .await
        .expect("timeout waiting for notify head");
    let actual = u32::from_be_bytes([head[24], head[25], head[26], head[27]]);
    assert_eq!(actual, 2, "generation replacement must notify");
    let notify = c.read(2).await;
    assert_eq!(notify, vec![0x50, 0b11]);
}

// 2. APDU ids are per-serve monotonic: a new import never reuses ids.
#[tokio::test]
async fn apdu_ids_not_reused_across_imports() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    card(&h, 1, ATR).await;
    settle().await;
    let xfr = bulk_out_msg(ccid::XFR_BLOCK, 1, &[0x00, 0xA4, 0x04, 0x00]);
    c.bulk_send(20, &xfr).await;
    let _ = c.ret_submit().await;
    let f1 = tokio::time::timeout(Duration::from_secs(3), h.phone_rx.recv())
        .await
        .expect("timeout waiting for Apdu f1")
        .unwrap();
    let id1 = match f1 {
        Frame::Apdu { id, .. } => id,
        other => panic!("expected Apdu, got {:?}", other),
    };
    // Session ends without a response; let the server reap the dead session.
    drop(c);
    settle().await;
    let mut c2 = Client::connect(h.addr).await;
    c2.import().await;
    let xfr2 = bulk_out_msg(ccid::XFR_BLOCK, 2, &[0x00, 0xA4, 0x04, 0x00]);
    c2.bulk_send(21, &xfr2).await;
    let _ = c2.ret_submit().await;
    let f2 = h.phone_rx.recv().await.unwrap();
    let id2 = match f2 {
        Frame::Apdu { id, .. } => id,
        other => panic!("expected Apdu, got {:?}", other),
    };
    assert_ne!(id1, id2, "apdu ids must be monotonic across imports");
}

// 3. import slot is freed even when the imported client dies mid-op.
#[tokio::test]
async fn import_slot_freed_after_client_death() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    card(&h, 1, ATR).await;
    settle().await;
    let xfr = bulk_out_msg(ccid::XFR_BLOCK, 1, &[0x00, 0xA4, 0x04, 0x00]);
    c.bulk_send(30, &xfr).await;
    let _ = c.ret_submit().await;
    let _ = h.phone_rx.recv().await.unwrap();
    drop(c);
    settle().await;
    let mut c2 = Client::connect(h.addr).await;
    c2.import().await; // must not be rejected
}

// 4. Shutdown joins promptly with an active client and phone traffic.
#[tokio::test]
async fn shutdown_returns_promptly_with_active_client_and_phone_traffic() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    for i in 0..64u64 {
        let _ = h
            .phone_tx
            .send(Frame::Card {
                generation: 1_000_000 + i,
                atr: ATR.to_vec(),
                max_apdu: 261,
                uid: vec![],
            })
            .await;
    }
    h.shutdown.send(true).unwrap();
    let jh = h.serve_jh.take().unwrap();
    let joined = tokio::time::timeout(Duration::from_secs(3), jh).await;
    assert!(joined.is_ok(), "serve must return promptly on shutdown");
    let mut buf = [0u8; 16];
    let res = tokio::time::timeout(Duration::from_secs(3), c.stream.read(&mut buf)).await;
    match res {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(_)) => panic!("unexpected data after shutdown"),
        Err(_) => panic!("server did not close client on shutdown"),
    }
}

// 5. A bulk-IN reply carries the full message including the SW tail, with
//    actual_length matching the wire bytes.
#[tokio::test]
async fn bulk_in_reply_preserves_full_tail() {
    let mut h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    card(&h, 1, ATR).await;
    settle().await;
    let apdu = [0x00, 0xA4, 0x04, 0x00];
    let xfr = bulk_out_msg(ccid::XFR_BLOCK, 5, &apdu);
    c.bulk_send(40, &xfr).await;
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
    let (head, data) = c.bulk_in(41, 65536).await;
    assert_eq!(data[0], ccid::RDR_DATA_BLOCK);
    let actual = u32::from_be_bytes([head[24], head[25], head[26], head[27]]);
    assert_eq!(actual, 12, "full CCID message incl. SW tail");
    assert_eq!(
        actual as usize,
        data.len(),
        "actual_length matches wire bytes"
    );
    let dwlen = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
    assert_eq!(dwlen, 2, "CCID dwLength = 2-byte abData (full SW)");
    assert_eq!(&data[10..12], &[0x61, 0x05], "SW must not be truncated");
}

// 6. Malformed CCID on bulk-OUT still gets an OUT ack, and a failed reply.
#[tokio::test]
async fn malformed_bulk_out_rejected() {
    let h = harness().await;
    let mut c = Client::connect(h.addr).await;
    c.import().await;
    let mut bad = vec![0x6f];
    bad.extend_from_slice(&u32::MAX.to_le_bytes());
    bad.extend_from_slice(&[0, 1, 0, 0, 0]);
    c.bulk_send(81, &bad).await;
    let (ack, bytes) = c.ret_submit().await;
    assert_eq!(&ack[24..28], &10u32.to_be_bytes());
    assert!(bytes.is_empty());
    let mut byte = [0];
    let read = tokio::time::timeout(Duration::from_secs(2), c.stream.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)));
}
