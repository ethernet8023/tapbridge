//! USB link: the no-network transport (Android Open Accessory, AOA).
//!
//! The PC is the USB host. It switches a known phone into accessory mode
//! with the AOA control requests (51 GET_PROTOCOL, 52 SEND_STRING, 53
//! START) and then talks over the accessory's two bulk endpoints. Android
//! opens tapbridge from the accessory identity in [`aoa`] (the app's
//! `accessory_filter.xml` must match it).
//!
//! There is no TLS on a cable, so before the usual `Hello`/`Accepted`
//! frames both sides prove their identity keys (the same Ed25519 keys iroh
//! uses, so pinned ids stay the same):
//!
//! 1. host → phone `Greeting { host, host_nonce, name }`
//! 2. phone → host `PhoneProof { phone, phone_nonce, sig_phone }`
//! 3. host → phone `HostProof { sig_host, capability }`
//!
//! Each signature covers a role label plus both ids and both nonces, so
//! neither proof can be replayed into another session or reflected. After
//! step 3 the stream carries ordinary [`crate::wire`] frames exactly as over
//! iroh: the phone sends `Hello`, the host answers `Accepted`/`Rejected`.
//!
//! `capability` is the host's outstanding pairing capability, sent only when
//! the PC's pairing window is open and only after the phone's proof
//! verified. It lets a phone pair over the cable without scanning the QR;
//! the phone still asks its user first.
//!
//! What the cable does not protect: anyone who can splice the USB cable can
//! read APDUs (a network link is encrypted; a cable is not). Authority is
//! unchanged: the phone serves only paired hosts, the host only its pinned
//! phone.
//!
//! [`pump`] bridges a blocking USB endpoint pair (libusb on the PC, the
//! accessory file descriptor on Android) to a tokio byte stream so the
//! frame code is shared.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iroh::{EndpointId, PublicKey, SecretKey, Signature};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::wire::MAX_NAME;

/// AOA accessory identity. Android matches manufacturer, model and version
/// against the app's `res/xml/accessory_filter.xml`.
pub mod aoa {
    pub const MANUFACTURER: &str = "tapbridge";
    pub const MODEL: &str = "tapbridge";
    pub const DESCRIPTION: &str = "tapbridge USB link";
    pub const VERSION: &str = "1";
    /// Shown by Android when no installed app matches the accessory.
    pub const URI: &str = "https://github.com/ethernet8023/tapbridge";
    /// Accessory serial (an identity string, not the phone's USB serial).
    pub const SERIAL: &str = "tapbridge-usb-1";

    /// Google's accessory-mode product ids (accessory, +adb, +audio...).
    pub const GOOGLE_VID: u16 = 0x18d1;
    pub const ACCESSORY_PIDS: [u16; 4] = [0x2d00, 0x2d01, 0x2d04, 0x2d05];

    /// AOA vendor control requests.
    pub const GET_PROTOCOL: u8 = 51;
    pub const SEND_STRING: u8 = 52;
    pub const START: u8 = 53;

    /// The six SEND_STRING values, indexed by string id.
    pub const STRINGS: [&str; 6] = [MANUFACTURER, MODEL, DESCRIPTION, VERSION, URI, SERIAL];

    pub fn is_accessory(vid: u16, pid: u16) -> bool {
        vid == GOOGLE_VID && ACCESSORY_PIDS.contains(&pid)
    }
}

/// Largest single USB transfer either side writes, and the minimum read
/// size. Android's accessory driver discards the unread rest of a transfer,
/// so every read must offer at least this many bytes.
pub const CHUNK: usize = 16 * 1024;

/// Handshake message magic + version.
const MAGIC: &[u8; 8] = b"TBUSB\x00\x00\x01";
/// Upper bound for one handshake message body.
const MAX_MESSAGE: usize = 512;

const LABEL_PHONE: &[u8] = b"tapbridge-usb/1 phone";
const LABEL_HOST: &[u8] = b"tapbridge-usb/1 host";

/// What the phone learns about the host from a verified handshake.
#[derive(Clone, PartialEq, Eq)]
pub struct HostInfo {
    /// The host's verified identity (same as its iroh endpoint id).
    pub host: EndpointId,
    /// Host display name, 1..=64 UTF-8 bytes. Unverified wording only.
    pub name: String,
    /// Pairing capability when the PC's pairing window is open.
    pub capability: Option<[u8; 32]>,
}

impl std::fmt::Debug for HostInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostInfo")
            .field("host", &self.host)
            .field("name", &self.name)
            .field("pairing_open", &self.capability.is_some())
            .finish()
    }
}

fn invalid(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn denied(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why)
}

fn transcript(
    label: &[u8],
    host: &EndpointId,
    hn: &[u8; 32],
    phone: &EndpointId,
    pn: &[u8; 32],
) -> Vec<u8> {
    let mut m = Vec::with_capacity(label.len() + 128);
    m.extend_from_slice(label);
    m.extend_from_slice(host.as_bytes());
    m.extend_from_slice(hn);
    m.extend_from_slice(phone.as_bytes());
    m.extend_from_slice(pn);
    m
}

async fn write_message<W: AsyncWrite + Unpin>(w: &mut W, body: &[u8]) -> io::Result<()> {
    let mut out = Vec::with_capacity(4 + MAGIC.len() + body.len());
    out.extend_from_slice(&((MAGIC.len() + body.len()) as u32).to_be_bytes());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(body);
    // One write per message: one USB transfer.
    w.write_all(&out).await?;
    w.flush().await
}

async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Vec<u8>> {
    let size = r.read_u32().await? as usize;
    if !(MAGIC.len()..=MAX_MESSAGE).contains(&size) {
        return Err(invalid("bad usb handshake length"));
    }
    let mut body = vec![0; size];
    r.read_exact(&mut body).await?;
    if &body[..MAGIC.len()] != MAGIC {
        return Err(invalid("not a tapbridge usb link"));
    }
    body.drain(..MAGIC.len());
    Ok(body)
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        if self.0.len() < N {
            return Err(invalid("short usb handshake"));
        }
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        Ok(head.try_into().unwrap())
    }
    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn rest(self) -> &'a [u8] {
        self.0
    }
    fn end(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("trailing usb handshake bytes"))
        }
    }
}

fn key(bytes: [u8; 32]) -> io::Result<PublicKey> {
    PublicKey::from_bytes(&bytes).map_err(|_| invalid("bad identity key"))
}

/// Host side. Returns the phone's verified identity; the caller then reads
/// the phone's `Hello` frame and authorizes it exactly as for an iroh peer
/// (`remote_id()` = the returned id). `capability` is sent only after the
/// phone's proof verified.
pub async fn host_handshake<R, W>(
    r: &mut R,
    w: &mut W,
    secret: &SecretKey,
    name: &str,
    capability: Option<[u8; 32]>,
) -> io::Result<EndpointId>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if !(1..=MAX_NAME).contains(&name.len()) {
        return Err(invalid("host name length"));
    }
    let host = secret.public();
    let host_nonce: [u8; 32] = rand::random();
    let mut greeting = Vec::new();
    greeting.extend_from_slice(host.as_bytes());
    greeting.extend_from_slice(&host_nonce);
    greeting.extend_from_slice(name.as_bytes());
    write_message(w, &greeting).await?;

    let proof = read_message(r).await?;
    let mut c = Cursor(&proof);
    let phone = key(c.take::<32>()?)?;
    let phone_nonce = c.take::<32>()?;
    let sig = Signature::from_bytes(&c.take::<64>()?);
    c.end()?;
    phone
        .verify(
            &transcript(LABEL_PHONE, &host, &host_nonce, &phone, &phone_nonce),
            &sig,
        )
        .map_err(|_| denied("phone proof did not verify"))?;

    let sig_host = secret.sign(&transcript(
        LABEL_HOST,
        &host,
        &host_nonce,
        &phone,
        &phone_nonce,
    ));
    let mut reply = sig_host.to_bytes().to_vec();
    match capability {
        Some(cap) => {
            reply.push(1);
            reply.extend_from_slice(&cap);
        }
        None => reply.push(0),
    }
    write_message(w, &reply).await?;
    Ok(phone)
}

/// Phone side. Returns the host's verified identity, display name and (if
/// the pairing window is open) its pairing capability. The caller decides
/// whether to continue (paired host, or user-confirmed pairing) and then
/// sends `Hello` with [`crate::wire::write_frame`].
pub async fn phone_handshake<R, W>(r: &mut R, w: &mut W, secret: &SecretKey) -> io::Result<HostInfo>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let greeting = read_message(r).await?;
    let mut c = Cursor(&greeting);
    let host = key(c.take::<32>()?)?;
    let host_nonce = c.take::<32>()?;
    let name = c.rest();
    if !(1..=MAX_NAME).contains(&name.len()) {
        return Err(invalid("host name length"));
    }
    let name = String::from_utf8(name.to_vec()).map_err(|_| invalid("host name utf-8"))?;

    let phone = secret.public();
    let phone_nonce: [u8; 32] = rand::random();
    let sig = secret.sign(&transcript(
        LABEL_PHONE,
        &host,
        &host_nonce,
        &phone,
        &phone_nonce,
    ));
    let mut proof = Vec::with_capacity(128);
    proof.extend_from_slice(phone.as_bytes());
    proof.extend_from_slice(&phone_nonce);
    proof.extend_from_slice(&sig.to_bytes());
    write_message(w, &proof).await?;

    let reply = read_message(r).await?;
    let mut c = Cursor(&reply);
    let sig_host = Signature::from_bytes(&c.take::<64>()?);
    let capability = match c.byte()? {
        0 => None,
        1 => Some(c.take::<32>()?),
        _ => return Err(invalid("bad capability flag")),
    };
    c.end()?;
    host.verify(
        &transcript(LABEL_HOST, &host, &host_nonce, &phone, &phone_nonce),
        &sig_host,
    )
    .map_err(|_| denied("host proof did not verify"))?;
    Ok(HostInfo {
        host,
        name,
        capability,
    })
}

/// A blocking USB endpoint pair.
pub trait Link: Send + Sync + 'static {
    /// Reads into `buf` (always [`CHUNK`] bytes). `Ok(0)` means "nothing
    /// yet" (a timeout): the pump checks for shutdown and reads again. An
    /// error ends the link. Implementations over a plain file must map EOF
    /// to an error, not `Ok(0)`.
    fn read(&self, buf: &mut [u8]) -> io::Result<usize>;
    /// Writes one transfer of at most [`CHUNK`] bytes, completely.
    fn write_all(&self, data: &[u8]) -> io::Result<()>;
}

/// Stops the pump threads when dropped (the reader notices at its next
/// `Ok(0)` or error; the writer when the stream's write half is dropped).
pub struct PumpGuard {
    stop: Arc<AtomicBool>,
}

impl PumpGuard {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

impl Drop for PumpGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Bridges `link` to a tokio byte stream on two OS threads. Split the
/// returned stream with [`tokio::io::split`]. When the link fails, the
/// stream reports EOF/broken pipe and the guard reads as stopped.
pub fn pump(
    link: Arc<dyn Link>,
    runtime: tokio::runtime::Handle,
) -> (tokio::io::DuplexStream, PumpGuard) {
    let (ours, theirs) = tokio::io::duplex(4 * CHUNK);
    let (mut from_app, mut to_app) = tokio::io::split(theirs);
    let stop = Arc::new(AtomicBool::new(false));

    let (link_r, stop_r, rt_r) = (Arc::clone(&link), Arc::clone(&stop), runtime.clone());
    std::thread::Builder::new()
        .name("tapbridge-usb-in".into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK];
            while !stop_r.load(Ordering::SeqCst) {
                match link_r.read(&mut buf) {
                    Ok(0) => continue,
                    Ok(n) => {
                        if rt_r.block_on(to_app.write_all(&buf[..n])).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            stop_r.store(true, Ordering::SeqCst);
            let _ = rt_r.block_on(to_app.shutdown());
        })
        .expect("spawn usb reader");

    let stop_w = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("tapbridge-usb-out".into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK];
            loop {
                match runtime.block_on(from_app.read(&mut buf)) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if stop_w.load(Ordering::SeqCst) || link.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            stop_w.store(true, Ordering::SeqCst);
        })
        .expect("spawn usb writer");

    (ours, PumpGuard { stop })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{self, Frame};
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::time::Duration;

    /// One end of an in-memory "cable": transfers arrive as whole messages,
    /// like USB bulk transfers; reads time out with Ok(0).
    struct Cable {
        rx: Mutex<mpsc::Receiver<Vec<u8>>>,
        tx: mpsc::Sender<Vec<u8>>,
    }

    impl Link for Cable {
        fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
            assert!(buf.len() >= CHUNK, "reads must offer a whole chunk");
            match self
                .rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(20))
            {
                Ok(t) => {
                    assert!(t.len() <= buf.len(), "a transfer larger than a chunk");
                    buf[..t.len()].copy_from_slice(&t);
                    Ok(t.len())
                }
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(0),
                Err(_) => Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
        fn write_all(&self, data: &[u8]) -> io::Result<()> {
            assert!(data.len() <= CHUNK);
            self.tx
                .send(data.to_vec())
                .map_err(|_| io::ErrorKind::BrokenPipe.into())
        }
    }

    fn cable() -> (Arc<dyn Link>, Arc<dyn Link>) {
        let (a_tx, a_rx) = mpsc::channel();
        let (b_tx, b_rx) = mpsc::channel();
        (
            Arc::new(Cable {
                rx: Mutex::new(a_rx),
                tx: b_tx,
            }),
            Arc::new(Cable {
                rx: Mutex::new(b_rx),
                tx: a_tx,
            }),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handshake_then_frames_cross_the_pumped_cable() {
        let (host_end, phone_end) = cable();
        let rt = tokio::runtime::Handle::current();
        let (host_stream, _hg) = pump(host_end, rt.clone());
        let (phone_stream, _pg) = pump(phone_end, rt);
        let host_key = SecretKey::generate();
        let phone_key = SecretKey::generate();
        let (host_id, phone_id) = (host_key.public(), phone_key.public());
        let cap = [7u8; 32];

        let host = tokio::spawn(async move {
            let (mut r, mut w) = tokio::io::split(host_stream);
            let phone = host_handshake(&mut r, &mut w, &host_key, "Desk", Some(cap))
                .await
                .unwrap();
            let hello = wire::read_frame(&mut r).await.unwrap();
            // A large APDU spans several USB transfers.
            wire::write_frame(
                &mut w,
                &Frame::Apdu {
                    id: 1,
                    generation: 2,
                    data: vec![0xAB; 40_000],
                },
            )
            .await
            .unwrap();
            (phone, hello)
        });
        let (mut r, mut w) = tokio::io::split(phone_stream);
        let info = phone_handshake(&mut r, &mut w, &phone_key).await.unwrap();
        assert_eq!(info.host, host_id);
        assert_eq!(info.name, "Desk");
        assert_eq!(info.capability, Some(cap));
        wire::write_frame(
            &mut w,
            &Frame::Hello {
                capability: info.capability,
                name: "Pixel".into(),
                push: None,
            },
        )
        .await
        .unwrap();
        let apdu = wire::read_frame(&mut r).await.unwrap();
        assert!(matches!(apdu, Frame::Apdu { data, .. } if data.len() == 40_000));
        let (phone, hello) = host.await.unwrap();
        assert_eq!(phone, phone_id);
        assert!(matches!(hello, Frame::Hello { capability: Some(c), .. } if c == cap));
    }

    #[tokio::test]
    async fn a_forged_phone_proof_is_rejected() {
        let (host_io, phone_io) = tokio::io::duplex(4096);
        let host_key = SecretKey::generate();
        let host = tokio::spawn(async move {
            let (mut r, mut w) = tokio::io::split(host_io);
            host_handshake(&mut r, &mut w, &host_key, "Desk", Some([1; 32])).await
        });
        let (mut r, mut w) = tokio::io::split(phone_io);
        let greeting = read_message(&mut r).await.unwrap();
        // Claim someone else's id, sign with our own key.
        let victim = SecretKey::generate().public();
        let liar = SecretKey::generate();
        let hn: [u8; 32] = greeting[32..64].try_into().unwrap();
        let host_id = key(greeting[..32].try_into().unwrap()).unwrap();
        let pn = [9u8; 32];
        let sig = liar.sign(&transcript(LABEL_PHONE, &host_id, &hn, &victim, &pn));
        let mut proof = victim.as_bytes().to_vec();
        proof.extend_from_slice(&pn);
        proof.extend_from_slice(&sig.to_bytes());
        write_message(&mut w, &proof).await.unwrap();
        let err = host.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // And the capability never went out.
        drop(w);
        let mut rest = Vec::new();
        let _ = r.read_to_end(&mut rest).await;
        assert!(rest.is_empty());
    }

    #[tokio::test]
    async fn a_host_that_cannot_sign_for_its_id_is_rejected() {
        let (host_io, phone_io) = tokio::io::duplex(4096);
        let phone_key = SecretKey::generate();
        let impostor = tokio::spawn(async move {
            let (mut r, mut w) = tokio::io::split(host_io);
            let claimed = SecretKey::generate().public();
            let mut g = claimed.as_bytes().to_vec();
            g.extend_from_slice(&[3; 32]);
            g.extend_from_slice(b"Desk");
            write_message(&mut w, &g).await.unwrap();
            let _ = read_message(&mut r).await.unwrap();
            let mut reply = SecretKey::generate().sign(b"whatever").to_bytes().to_vec();
            reply.push(0);
            write_message(&mut w, &reply).await.unwrap();
        });
        let (mut r, mut w) = tokio::io::split(phone_io);
        let err = phone_handshake(&mut r, &mut w, &phone_key)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        impostor.await.unwrap();
    }

    #[tokio::test]
    async fn garbage_instead_of_a_greeting_is_invalid() {
        let mut bytes: &[u8] = b"\x00\x00\x00\x10GET / HTTP/1.1\r\n";
        let mut sink = Vec::new();
        let err = phone_handshake(&mut bytes, &mut sink, &SecretKey::generate())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn accessory_ids() {
        assert!(aoa::is_accessory(0x18d1, 0x2d01));
        assert!(!aoa::is_accessory(0x18d1, 0x4ee1));
        assert_eq!(aoa::STRINGS[5], aoa::SERIAL);
    }
}
