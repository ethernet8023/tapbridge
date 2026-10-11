//! Encrypted iroh transport: production endpoint builder and the reusable
//! Hello handshake on bounded wire frames.
//!
//! Session shape: one long-lived bidirectional QUIC stream per authenticated
//! session; the phone sends the first [`Frame::Hello`] and the host replies
//! [`Frame::Accepted`] or [`Frame::Rejected`], then closes on rejection.
//! Never log capabilities or card bytes; [`Frame::Hello`] Debug is redacted.

use std::io;
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, SecretKey, endpoint::Builder};

use crate::wire::{self, Frame};

/// Build an endpoint for production: N0 relays and DNS address discovery,
/// fixed [`crate::ALPN`], explicit identity. Bind the returned builder to
/// finish (`.bind().await`).
pub fn production_builder(secret_key: &SecretKey) -> Builder {
    Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key.clone())
        .alpns(vec![crate::ALPN.to_vec()])
}

/// Accept the next incoming connection, completing the QUIC handshake.
pub async fn accept(ep: &Endpoint) -> io::Result<Connection> {
    let incoming = ep
        .accept()
        .await
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "endpoint closed"))?;
    incoming.await.map_err(std::io::Error::other)
}

/// Host decision after inspecting the phone's Hello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDecision {
    /// Reply `Accepted` with the host display name; the session continues.
    Accept(String),
    /// Reply `Rejected` and close the connection.
    Reject,
}

/// What the host's Hello handshake concluded.
#[derive(Debug)]
pub enum HostHelloOutcome {
    /// Phone is authorized; the open stream pair is returned for the session.
    Accepted {
        /// The stream pair is owned by the session after this handshake.
        send: SendStream,
        recv: RecvStream,
    },
    /// Phone was rejected; the connection has been closed.
    Rejected {
        /// Why the host rejected, for local diagnostics only (never sent).
        reason: &'static str,
    },
}

/// Read exactly one Hello frame from an accepted bidirectional stream.
///
/// The read owns the stream for its lifetime: the framed reader must not be
/// restarted after a partial read, so on error the whole session is dead.
/// The connection's peer identity comes from `conn.remote_id()` (TLS
/// authenticated); the Hello only carries the pairing capability.
pub async fn host_receive_hello(conn: &Connection) -> io::Result<(SendStream, RecvStream, Frame)> {
    let (send, mut recv) = conn
        .accept_bi()
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let hello = read_hello(&mut recv).await?;
    Ok((send, recv, hello))
}

/// Host side of the Hello handshake: read the phone's Hello, apply `decide`,
/// reply, and close the connection on rejection.
pub async fn host_handle_hello<F>(conn: &Connection, decide: F) -> io::Result<HostHelloOutcome>
where
    F: FnOnce(Frame) -> HostDecision,
{
    let (mut send, recv, hello) = host_receive_hello(conn).await?;
    match decide(hello) {
        HostDecision::Accept(name) => {
            reply_accepted(&mut send, name).await?;
            Ok(HostHelloOutcome::Accepted { send, recv })
        }
        HostDecision::Reject => {
            reply_rejected(&mut send).await?;
            // Wait for transport acknowledgement, bounded for disconnected peers.
            send.finish().map_err(std::io::Error::other)?;
            let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
            close(conn);
            Ok(HostHelloOutcome::Rejected {
                reason: "unauthorized",
            })
        }
    }
}

/// Phone side of the Hello handshake: send Hello (optionally carrying the
/// pairing capability, always the phone display name and current push token)
/// and await the host verdict.
pub async fn client_hello(
    send: &mut SendStream,
    recv: &mut RecvStream,
    hello: ClientHello,
) -> io::Result<HelloReply> {
    write_frame(
        send,
        &Frame::Hello {
            capability: hello.capability,
            name: hello.name,
            push: hello.push,
        },
    )
    .await?;
    let reply = read_frame(recv).await?;
    match reply {
        Frame::Accepted {
            name,
            platform,
            piv_login,
        } => Ok(HelloReply::Accepted {
            host_name: name,
            platform,
            piv_login,
        }),
        Frame::Rejected => Ok(HelloReply::Rejected),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected frame in handshake",
        )),
    }
}

/// What the phone announces in its Hello.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientHello {
    /// Pairing capability from the invitation; `None` for a pinned reconnect.
    pub capability: Option<[u8; 32]>,
    /// Phone display name, 1..=64 UTF-8 bytes.
    pub name: String,
    /// Current FCM token, when push is available.
    pub push: Option<String>,
}

impl std::fmt::Debug for ClientHello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientHello(REDACTED)")
    }
}

/// The host's verdict on a Hello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloReply {
    /// Host accepted the session and told us its display name, OS
    /// ([`crate::wire::platform`]) and whether it does PIV login.
    Accepted {
        host_name: String,
        platform: u8,
        piv_login: bool,
    },
    /// Host rejected; treat the session as dead.
    Rejected,
}

/// Host reply helpers used by [`host_handle_hello`] or custom host loops.
pub async fn reply_accepted(send: &mut SendStream, name: String) -> io::Result<()> {
    write_frame(
        send,
        &Frame::Accepted {
            name,
            platform: crate::wire::platform::current(),
            piv_login: crate::wire::platform::piv_login(),
        },
    )
    .await
}

/// See [`reply_accepted`].
pub async fn reply_rejected(send: &mut SendStream) -> io::Result<()> {
    write_frame(send, &Frame::Rejected).await
}

async fn read_hello(recv: &mut RecvStream) -> io::Result<Frame> {
    let frame = read_frame(recv).await?;
    if matches!(frame, Frame::Hello { .. }) {
        Ok(frame)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected Hello as first frame",
        ))
    }
}

/// Close the connection immediately with the tapbridge application close code.
pub fn close(conn: &Connection) {
    conn.close(
        iroh::endpoint::VarInt::from_u32(CLOSE_CODE),
        b"tapbridge session closed",
    );
}

/// Application close code distinguishing deliberate tapbridge closes.
pub const CLOSE_CODE: u32 = 1;

async fn read_frame(recv: &mut RecvStream) -> io::Result<Frame> {
    wire::read_frame(recv).await
}

async fn write_frame(send: &mut SendStream, frame: &Frame) -> io::Result<()> {
    wire::write_frame(send, frame).await
}
