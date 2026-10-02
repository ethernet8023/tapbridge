//! Tests for the encrypted iroh transport helpers.
//!
//! Every test binds real iroh endpoints on loopback port 0 with isolated
//! `SecretKey` identities and connects over genuine QUIC. No mocks.

use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, SecretKey, endpoint::presets};
use tapbridge_core::{ALPN, transport, wire::Frame};

const TIMEOUT: Duration = Duration::from_secs(20);

/// A bound, isolated endpoint on 127.0.0.1:0 with no relays or discovery.
async fn test_endpoint(secret: &SecretKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(secret.clone())
        .alpns(vec![ALPN.to_vec()])
        .bind_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .unwrap()
        .bind()
        .await
        .expect("bind")
}

/// The endpoint address of a bound endpoint (loopback direct addrs only here).
fn addr_of(ep: &Endpoint) -> EndpointAddr {
    ep.addr()
}

async fn accept_conn(ep: &Endpoint) -> iroh::endpoint::Connection {
    let incoming = tokio::time::timeout(TIMEOUT, ep.accept())
        .await
        .expect("timed out waiting for incoming connection")
        .expect("endpoint closed");
    tokio::time::timeout(TIMEOUT, incoming)
        .await
        .expect("timed out completing handshake")
        .expect("handshake failed")
}

async fn open_bi(
    conn: &iroh::endpoint::Connection,
) -> (iroh::endpoint::SendStream, iroh::endpoint::RecvStream) {
    tokio::time::timeout(TIMEOUT, conn.open_bi())
        .await
        .expect("timed out opening stream")
        .expect("open_bi failed")
}

/// Full session task for the host side of a pairing-less session: accept,
/// read Hello, and reply per the closure's decision.
async fn run_host(
    ep: Endpoint,
    pinned: Option<iroh::EndpointId>,
) -> (iroh::EndpointId, transport::HostHelloOutcome) {
    let conn = accept_conn(&ep).await;
    let peer = conn.remote_id();
    let outcome = transport::host_handle_hello(&conn, |hello| match &hello {
        Frame::Hello { .. } => {
            let known = pinned.is_none_or(|p| p == peer);
            if known {
                transport::HostDecision::Accept("Desk".into())
            } else {
                transport::HostDecision::Reject
            }
        }
        _ => transport::HostDecision::Reject,
    })
    .await
    .expect("host handshake failed");
    if matches!(outcome, transport::HostHelloOutcome::Accepted { .. }) {
        // Keep the connection alive so the Accepted frame is delivered.
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
    }
    (peer, outcome)
}

#[tokio::test]
async fn hello_accepted_roundtrip_over_real_endpoint() {
    let host_key = SecretKey::generate();
    let phone_key = SecretKey::generate();
    let host = test_endpoint(&host_key).await;
    let phone = test_endpoint(&phone_key).await;
    let host_addr = addr_of(&host);

    let host_task = tokio::spawn(run_host(host, None));

    let conn = tokio::time::timeout(TIMEOUT, phone.connect(host_addr, ALPN))
        .await
        .expect("timed out connecting")
        .expect("connect failed");
    assert_eq!(conn.remote_id(), host_key.public());

    let (mut send, mut recv) = open_bi(&conn).await;
    let reply = transport::client_hello(&mut send, &mut recv, hello())
        .await
        .expect("client hello failed");
    assert!(matches!(reply, transport::HelloReply::Accepted { .. }));

    let (_, outcome) = host_task.await.unwrap();
    match outcome {
        transport::HostHelloOutcome::Accepted { .. } => {}
        other => panic!("expected Accepted, got {other:?}"),
    }
    transport::close(&conn);
}

#[tokio::test]
async fn unauthorized_peer_is_rejected_and_disconnected() {
    let host_key = SecretKey::generate();
    let phone_key = SecretKey::generate();
    let stranger_key = SecretKey::generate();
    let host = test_endpoint(&host_key).await;
    let stranger = test_endpoint(&stranger_key).await;
    let host_addr = addr_of(&host);

    // Host only ever accepts the pinned phone identity.
    let host_task = tokio::spawn(run_host(host, Some(phone_key.public())));

    // A stranger dials the host: connection succeeds (iroh auth) but the
    // application handshake must reject it and close.
    let conn = tokio::time::timeout(TIMEOUT, stranger.connect(host_addr, ALPN))
        .await
        .expect("timed out connecting")
        .expect("connect failed");
    assert_eq!(conn.remote_id(), host_key.public());
    let (mut send, mut recv) = open_bi(&conn).await;
    let reply = transport::client_hello(&mut send, &mut recv, hello())
        .await
        .expect("client hello failed");
    assert!(matches!(reply, transport::HelloReply::Rejected));

    // Host saw the unauthorized peer and rejected.
    let (peer, outcome) = host_task.await.unwrap();
    assert_eq!(peer, stranger_key.public());
    assert!(matches!(
        outcome,
        transport::HostHelloOutcome::Rejected { .. }
    ));

    // The host closed the connection after Rejected; the client observes the
    // reply stream ending.
    let eof = tokio::time::timeout(
        TIMEOUT,
        tokio::io::AsyncReadExt::read_to_end(&mut recv, &mut Vec::new()),
    )
    .await;
    assert!(eof.is_ok(), "reply stream should end after rejection");
}

#[tokio::test]
async fn capability_hello_roundtrip_and_debug_redaction() {
    let host_key = SecretKey::generate();
    let phone_key = SecretKey::generate();
    let host = test_endpoint(&host_key).await;
    let phone = test_endpoint(&phone_key).await;
    let host_addr = addr_of(&host);

    let capability: [u8; 32] = rand::random();
    let host_task = tokio::spawn(async move {
        let conn = accept_conn(&host).await;
        let (mut send, _recv, hello) = transport::host_receive_hello(&conn)
            .await
            .expect("host receive hello failed");
        assert_eq!(
            hello,
            Frame::Hello {
                capability: Some(capability),
                name: "phone".into(),
                push: Some("push-token".into()),
            }
        );
        transport::reply_accepted(&mut send, "Desk".into())
            .await
            .unwrap();
        send.finish().map_err(std::io::Error::other).unwrap();
        // Keep the connection alive until the phone has read the reply.
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
        conn
    });

    let conn = tokio::time::timeout(TIMEOUT, phone.connect(host_addr, ALPN))
        .await
        .expect("timed out connecting")
        .expect("connect failed");
    let (mut send, mut recv) = open_bi(&conn).await;
    let reply = transport::client_hello(
        &mut send,
        &mut recv,
        transport::ClientHello {
            capability: Some(capability),
            name: "phone".into(),
            push: Some("push-token".into()),
        },
    )
    .await
    .expect("client hello failed");
    assert_eq!(
        reply,
        transport::HelloReply::Accepted {
            host_name: "Desk".into()
        }
    );
    let _conn = host_task.await.unwrap();

    // Hello frames must never leak the capability through Debug.
    let frame = Frame::Hello {
        capability: Some(capability),
        name: "phone".into(),
        push: Some("push-token".into()),
    };
    let debug = format!("{frame:?}");
    assert!(!debug.contains(&hex_debug(&capability)));
    assert!(!debug.contains("push-token"));
}

fn hex_debug(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x?}")).collect()
}

#[tokio::test]
async fn overlength_frame_is_rejected() {
    let host_key = SecretKey::generate();
    let phone_key = SecretKey::generate();
    let host = test_endpoint(&host_key).await;
    let phone = test_endpoint(&phone_key).await;
    let host_addr = addr_of(&host);

    let host_task = tokio::spawn(async move {
        let conn = accept_conn(&host).await;
        let result = transport::host_receive_hello(&conn).await;
        assert!(result.is_err(), "overlength frame must be rejected");
    });

    let conn = tokio::time::timeout(TIMEOUT, phone.connect(host_addr, ALPN))
        .await
        .expect("timed out connecting")
        .expect("connect failed");
    let (mut send, _recv) = open_bi(&conn).await;
    // Write a size prefix far beyond the frame limit, then the bytes.
    send.write_all(&70000u32.to_be_bytes()).await.unwrap();
    send.write_all(&vec![0u8; 70000]).await.unwrap();
    host_task.await.unwrap();
    transport::close(&conn);
}

#[tokio::test]
async fn accepted_handshake_keeps_both_stream_directions_open() {
    let host = test_endpoint(&SecretKey::generate()).await;
    let phone = test_endpoint(&SecretKey::generate()).await;
    let address = host.addr();
    let host_task = tokio::spawn(async move {
        let conn = accept_conn(&host).await;
        let outcome =
            transport::host_handle_hello(&conn, |_| transport::HostDecision::Accept("Desk".into()))
                .await
                .unwrap();
        let _keep_outcome_alive = outcome;
        tokio::time::timeout(TIMEOUT, conn.closed()).await.unwrap();
        host.close().await;
    });
    let conn = phone.connect(address, ALPN).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    assert_eq!(
        transport::client_hello(&mut send, &mut recv, hello())
            .await
            .unwrap(),
        transport::HelloReply::Accepted {
            host_name: "Desk".into()
        }
    );
    // Neither EOF nor STOP_SENDING is allowed after Accepted.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), recv.read(&mut [0; 1]))
            .await
            .is_err()
    );
    tapbridge_core::wire::write_frame(&mut send, &Frame::ReadCard)
        .await
        .unwrap();
    transport::close(&conn);
    host_task.await.unwrap();
    phone.close().await;
}

#[tokio::test]
async fn non_hello_frame_rejected_at_host() {
    let host_key = SecretKey::generate();
    let phone_key = SecretKey::generate();
    let host = test_endpoint(&host_key).await;
    let phone = test_endpoint(&phone_key).await;
    let host_addr = addr_of(&host);

    let host_task = tokio::spawn(async move {
        let conn = accept_conn(&host).await;
        let result = transport::host_receive_hello(&conn).await;
        assert!(result.is_err(), "non-hello frame must be rejected");
    });

    let conn = tokio::time::timeout(TIMEOUT, phone.connect(host_addr, ALPN))
        .await
        .expect("timed out connecting")
        .expect("connect failed");
    let (mut send, _recv) = open_bi(&conn).await;

    tapbridge_core::wire::write_frame(&mut send, &Frame::ReadCard)
        .await
        .unwrap();
    host_task.await.unwrap();
    transport::close(&conn);
}

fn hello() -> transport::ClientHello {
    transport::ClientHello {
        capability: None,
        name: "phone".into(),
        push: None,
    }
}
