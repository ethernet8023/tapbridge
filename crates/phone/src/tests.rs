use super::*;
use iroh::endpoint::presets;
use std::sync::{Arc, mpsc};

fn builder() -> iroh::endpoint::Builder {
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .alpns(vec![tapbridge_core::ALPN.to_vec()])
        .bind_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .unwrap()
}

#[test]
fn actual_phone_session_relays_frames_and_close_unblocks_reader() {
    let (address_tx, address_rx) = mpsc::channel();
    let host = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let endpoint = builder().bind().await.unwrap();
            let addr = endpoint.addr();
            address_tx.send(addr.clone()).unwrap();
            let conn = transport::accept(&endpoint).await.unwrap();
            let (mut send, mut recv, hello) = transport::host_receive_hello(&conn).await.unwrap();
            assert_eq!(
                hello,
                Frame::Hello {
                    capability: None,
                    name: "phone".into(),
                    push: None,
                }
            );
            transport::reply_accepted(&mut send, "PC".into())
                .await
                .unwrap();
            assert!(matches!(
                wire::read_frame(&mut recv).await.unwrap(),
                Frame::Card { generation: 1, uid, .. } if uid == [4, 1, 2, 3, 4, 5, 6]
            ));
            let request = Frame::Apdu {
                id: 7,
                generation: 1,
                data: vec![0, 0xa4, 4, 0],
            };
            wire::write_frame(&mut send, &request).await.unwrap();
            assert_eq!(
                wire::read_frame(&mut recv).await.unwrap(),
                Frame::Response {
                    id: 7,
                    generation: 1,
                    data: vec![0x90, 0]
                }
            );
            let setup = Frame::SetupPiv {
                uid: vec![4, 1, 2, 3, 4, 5, 6],
            };
            wire::write_frame(&mut send, &setup).await.unwrap();
            assert_eq!(
                wire::read_frame(&mut recv).await.unwrap(),
                Frame::PivSetupDone { ok: true }
            );
            tokio::time::timeout(Duration::from_secs(5), conn.closed())
                .await
                .unwrap();
            endpoint.close().await;
        });
    });
    let (session, used) = Session::connect_to(
        builder(),
        address_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        transport::ClientHello {
            capability: None,
            name: "phone".into(),
            push: None,
        },
    )
    .unwrap();
    assert_eq!(session.host_name(), "PC");
    assert!(!used.is_empty());
    let session = Arc::new(session);
    session
        .send(&Frame::Card {
            generation: 1,
            atr: vec![0x3b, 0x80, 1, 0x81],
            max_apdu: 261,
            uid: vec![4, 1, 2, 3, 4, 5, 6],
        })
        .unwrap();
    assert_eq!(
        session.read().unwrap(),
        Frame::Apdu {
            id: 7,
            generation: 1,
            data: vec![0, 0xa4, 4, 0]
        }
    );
    session
        .send(&Frame::Response {
            id: 7,
            generation: 1,
            data: vec![0x90, 0],
        })
        .unwrap();
    assert_eq!(
        session.read().unwrap(),
        Frame::SetupPiv {
            uid: vec![4, 1, 2, 3, 4, 5, 6]
        }
    );
    session.send(&Frame::PivSetupDone { ok: true }).unwrap();
    let blocked = session.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        done_tx.send(blocked.read().is_err()).unwrap();
    });
    session.close();
    assert!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap());
    reader.join().unwrap();
    drop(session);
    host.join().unwrap();
}

#[test]
fn rejects_invalid_or_mismatched_invitation_without_network() {
    assert!(Session::connect([1; 32], "bad", "", "phone", None).is_err());
    let host = SecretKey::generate().public();
    let invitation = Invitation::new(SecretKey::generate().public());
    assert!(
        Session::connect(
            [1; 32],
            &host.to_string(),
            &invitation.to_string(),
            "phone",
            None
        )
        .is_err()
    );
}
