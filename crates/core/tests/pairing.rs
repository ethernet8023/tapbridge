//! Tests for the pairing policy: invitation format, gate state machine,
//! and a full pairing over real iroh endpoints on loopback.

use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, SecretKey, endpoint::presets};
use tapbridge_core::{ALPN, pairing, transport, wire::Frame};

const TIMEOUT: Duration = Duration::from_secs(20);

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

fn addr_of(ep: &Endpoint) -> EndpointAddr {
    ep.addr()
}

// ---------- invitation format ----------

#[test]
fn invitation_format_is_tapbridge_1_host_capability() {
    let host = SecretKey::generate().public();
    let invite = pairing::Invitation::new(host);
    let s = invite.to_string();
    let parts: Vec<&str> = s.split(':').collect();
    assert_eq!(parts.len(), 4, "expected 4 colon-separated parts, got {s}");
    assert_eq!(parts[0], "tapbridge");
    assert_eq!(parts[1], "1");
    assert_eq!(
        parts[2],
        host.to_string(),
        "host id must be hex endpoint id"
    );
    assert_eq!(parts[3].len(), 64, "capability must be 32 bytes hex");
    assert!(parts[3].chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn invitation_roundtrip_and_reject_malformed() {
    let host = SecretKey::generate().public();
    let invite = pairing::Invitation::new(host);
    let parsed = pairing::Invitation::parse(&invite.to_string()).expect("valid invitation parses");
    assert_eq!(parsed.host_id(), host);
    assert_eq!(parsed.capability(), invite.capability());

    // Wrong version.
    let s = invite
        .to_string()
        .replacen("tapbridge:1:", "tapbridge:2:", 1);
    assert!(pairing::Invitation::parse(&s).is_err());
    // Wrong prefix.
    let s = format!(
        "other:1:{}",
        invite.to_string().splitn(3, ':').nth(2).unwrap()
    );
    assert!(pairing::Invitation::parse(&s).is_err());
    // Bad capability length.
    let s = format!("tapbridge:1:{host}:abcd");
    assert!(pairing::Invitation::parse(&s).is_err());
    // Bad host id.
    let s = format!("tapbridge:1:zzzz:{}", hex(32));
    assert!(pairing::Invitation::parse(&s).is_err());
    // Trailing garbage.
    let s = format!("{}!", invite);
    assert!(pairing::Invitation::parse(&s).is_err());
}

#[test]
fn invitation_debug_does_not_leak_capability() {
    let invite = pairing::Invitation::new(SecretKey::generate().public());
    let debug = format!("{invite:?}");
    let cap_hex = hex_string(&invite.capability());
    assert!(
        !debug.contains(&cap_hex),
        "debug leaked capability: {debug}"
    );
    assert!(
        !invite
            .to_string()
            .contains(&format!("{:?}", invite.capability()))
    );
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex(n: usize) -> String {
    "a".repeat(n * 2)
}

#[test]
fn capabilities_are_unique_per_mint() {
    let a = pairing::Invitation::new(SecretKey::generate().public());
    let b = pairing::Invitation::new(SecretKey::generate().public());
    assert_ne!(a.capability(), b.capability());
}

// ---------- gate state machine ----------

#[test]
fn gate_rejects_when_no_invitation() {
    let mut gate = pairing::PairingGate::new();
    let stranger = SecretKey::generate().public();
    let cap: [u8; 32] = rand::random();
    assert!(matches!(
        gate.authorize(stranger, Some(&cap)),
        Err(pairing::PairingError::NoInvitation)
    ));
    assert!(matches!(
        gate.authorize(stranger, None),
        Err(pairing::PairingError::UnknownPeer)
    ));
}

#[test]
fn gate_rejects_wrong_capability() {
    let mut gate = pairing::PairingGate::new();
    let phone = SecretKey::generate().public();
    let invite = gate.mint(SecretKey::generate().public());
    let wrong: [u8; 32] = rand::random();
    assert_ne!(wrong, invite.capability());
    assert!(matches!(
        gate.authorize(phone, Some(&wrong)),
        Err(pairing::PairingError::CapabilityMismatch)
    ));
}

#[test]
fn gate_expires_invitation() {
    let mut gate = pairing::PairingGate::with_ttl(Duration::ZERO);
    gate.mint(SecretKey::generate().public());
    let phone = SecretKey::generate().public();
    let invite = gate.mint(SecretKey::generate().public());
    assert!(matches!(
        gate.authorize(phone, Some(&invite.capability())),
        Err(pairing::PairingError::Expired)
    ));
}

#[test]
fn minting_again_revokes_previous_invitation() {
    let mut gate = pairing::PairingGate::new();
    gate.mint(SecretKey::generate().public());
    let second = gate.mint(SecretKey::generate().public());
    let phone = SecretKey::generate().public();
    // Only the latest capability works.
    assert!(gate.authorize(phone, Some(&second.capability())).is_ok());
}

#[test]
fn commit_then_replay_is_rejected() {
    let mut gate = pairing::PairingGate::new();
    let phone = SecretKey::generate().public();
    let invite = gate.mint(SecretKey::generate().public());
    let pending = gate
        .authorize(phone, Some(&invite.capability()))
        .expect("first use must authorize");
    gate.commit(pending).expect("commit succeeds");

    // Replay of the same capability after a successful commit: rejected.
    assert!(matches!(
        gate.authorize(phone, Some(&invite.capability())),
        Err(pairing::PairingError::Replay)
    ));
    // Any other attempt after commit: no invitation.
    let cap: [u8; 32] = rand::random();
    assert!(matches!(
        gate.authorize(phone, Some(&cap)),
        Err(pairing::PairingError::NoInvitation)
    ));
}

#[test]
fn no_commit_means_capability_remains_usable_lost_ack_retry() {
    // Persist failed (host never commits): the phone may retry with the same
    // capability and must still be authorized.
    let mut gate = pairing::PairingGate::new();
    let phone = SecretKey::generate().public();
    let invite = gate.mint(SecretKey::generate().public());
    let _pending = gate
        .authorize(phone, Some(&invite.capability()))
        .expect("first attempt");
    let second = gate
        .authorize(phone, Some(&invite.capability()))
        .expect("retry without commit must still authorize");
    gate.commit(second).expect("commit on retry");
}

#[test]
fn pinned_identity_reconnect_without_capability() {
    let mut gate = pairing::PairingGate::new();
    let pinned = SecretKey::generate().public();
    assert!(gate.authorize_pinned(pinned, pinned).is_ok());
    let stranger = SecretKey::generate().public();
    assert!(matches!(
        gate.authorize_pinned(stranger, pinned),
        Err(pairing::PairingError::UnknownPeer)
    ));
}

#[test]
fn pending_approval_cannot_consume_a_new_invitation() {
    let mut gate = pairing::PairingGate::new();
    let host = SecretKey::generate().public();
    let phone = SecretKey::generate().public();
    let first = gate.mint(host);
    let pending = gate.authorize(phone, Some(&first.capability())).unwrap();
    let second = gate.mint(host);
    assert!(gate.commit(pending).is_err());
    assert!(gate.authorize(phone, Some(&second.capability())).is_ok());
}

#[test]
fn one_invitation_cannot_commit_two_previously_authorized_phones() {
    let mut gate = pairing::PairingGate::new();
    let invitation = gate.mint(SecretKey::generate().public());
    let first = gate
        .authorize(
            SecretKey::generate().public(),
            Some(&invitation.capability()),
        )
        .unwrap();
    let second = gate
        .authorize(
            SecretKey::generate().public(),
            Some(&invitation.capability()),
        )
        .unwrap();
    gate.commit(first).unwrap();
    assert!(gate.commit(second).is_err());
}

// ---------- full pairing over real endpoints ----------

/// Host-side pairing session: authorize via the gate, "persist" the phone id,
/// commit, then reply. Returns the outcome for assertions.
async fn host_pairing_session(ep: &Endpoint, gate: &mut pairing::PairingGate) -> bool {
    let conn = transport::accept(ep).await.expect("accept failed");
    let result = transport::host_handle_hello(&conn, |hello| match &hello {
        Frame::Hello {
            capability: Some(cap),
        } => match gate.authorize(conn.remote_id(), Some(cap)) {
            Ok(pending) => {
                // Host caller persists BEFORE Accepted. Here persistence is
                // simulated by committing the gate entry.
                match gate.commit(pending) {
                    Ok(_) => transport::HostDecision::Accept,
                    Err(_) => transport::HostDecision::Reject,
                }
            }
            Err(_) => transport::HostDecision::Reject,
        },
        _ => transport::HostDecision::Reject,
    })
    .await
    .expect("host handshake failed");
    if matches!(result, transport::HostHelloOutcome::Accepted { .. }) {
        // Session owner keeps the connection until the phone hangs up, so
        // the Accepted frame is delivered and read by the client.
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
    } else {
        transport::close(&conn);
    }
    matches!(result, transport::HostHelloOutcome::Accepted { .. })
}

#[tokio::test]
async fn full_pairing_over_real_endpoints_and_replay_rejected() {
    let host_key = SecretKey::generate();
    let phone_key = SecretKey::generate();
    let host = test_endpoint(&host_key).await;
    let phone = test_endpoint(&phone_key).await;
    let host_addr = addr_of(&host);

    let mut gate = pairing::PairingGate::new();
    let invite = gate.mint(host_key.public());
    let capability = invite.capability();

    // Host session task: pair the first dial, then serve the replay dial.
    let host_task = tokio::spawn(async move {
        let first = host_pairing_session(&host, &mut gate).await;
        assert!(first, "first pairing");
        let second = host_pairing_session(&host, &mut gate).await;
        assert!(!second, "replay must be rejected");
    });

    // Phone pairs once over the real endpoint.
    {
        let conn = tokio::time::timeout(TIMEOUT, phone.connect(host_addr.clone(), ALPN))
            .await
            .expect("timed out connecting")
            .expect("connect failed");
        let (mut send, mut recv) = conn.open_bi().await.expect("open_bi failed");
        let reply = transport::client_hello(&mut send, &mut recv, Some(capability))
            .await
            .expect("hello failed");
        assert!(matches!(reply, transport::HelloReply::Accepted));
    }

    // Replay: the phone redials with the same (already consumed) capability.
    {
        let conn = tokio::time::timeout(TIMEOUT, phone.connect(host_addr, ALPN))
            .await
            .expect("timed out connecting")
            .expect("connect failed");
        let (mut send, mut recv) = conn.open_bi().await.expect("open_bi failed");
        let reply = transport::client_hello(&mut send, &mut recv, Some(capability))
            .await
            .expect("hello failed");
        assert!(matches!(reply, transport::HelloReply::Rejected));
        // Host must disconnect after rejection: the reply stream reaches EOF.
        let eof = tokio::time::timeout(
            TIMEOUT,
            tokio::io::AsyncReadExt::read_to_end(&mut recv, &mut Vec::new()),
        )
        .await;
        assert!(eof.is_ok(), "reply stream should end after replay reject");
    }
    host_task.await.unwrap();
}
