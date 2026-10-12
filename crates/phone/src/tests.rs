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
    let (got_tx, got_rx) = mpsc::channel();
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
            let setup = Frame::CardAction {
                uid: vec![4, 1, 2, 3, 4, 5, 6],
                action: wire::action::SETUP_CERTS,
            };
            wire::write_frame(&mut send, &setup).await.unwrap();
            assert_eq!(
                wire::read_frame(&mut recv).await.unwrap(),
                Frame::CardActionDone {
                    uid: vec![4, 1, 2, 3, 4, 5, 6],
                    action: wire::action::SETUP_CERTS,
                    ok: true,
                }
            );
            // Only now may the phone close: a close racing the last frame
            // would lose it (QUIC drops unread stream data on close).
            got_tx.send(()).unwrap();
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
    assert_eq!(session.host_platform(), wire::platform::current());
    assert_eq!(session.host_piv_login(), wire::platform::piv_login());
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
        Frame::CardAction {
            uid: vec![4, 1, 2, 3, 4, 5, 6],
            action: wire::action::SETUP_CERTS,
        }
    );
    session
        .send(&Frame::CardActionDone {
            uid: vec![4, 1, 2, 3, 4, 5, 6],
            action: wire::action::SETUP_CERTS,
            ok: true,
        })
        .unwrap();
    got_rx.recv_timeout(Duration::from_secs(5)).unwrap();
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

// ------------------------------------------------------------- USB link

mod usb_link {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use tapbridge_core::usb::host_handshake;

    const CAP: [u8; 32] = [5; 32];

    /// A fake PC on the other end of a socketpair "cable". `script` runs
    /// after the handshake with the phone's verified id.
    fn fake_host<F, Fut>(
        cable: UnixStream,
        secret: SecretKey,
        capability: Option<[u8; 32]>,
        script: F,
    ) -> std::thread::JoinHandle<()>
    where
        F: FnOnce(
                iroh::EndpointId,
                tokio::io::ReadHalf<tokio::net::UnixStream>,
                tokio::io::WriteHalf<tokio::net::UnixStream>,
            ) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = ()>,
    {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                cable.set_nonblocking(true).unwrap();
                let stream = tokio::net::UnixStream::from_std(cable).unwrap();
                let (mut r, mut w) = tokio::io::split(stream);
                let phone = host_handshake(&mut r, &mut w, &secret, "Desk PC", capability)
                    .await
                    .unwrap();
                script(phone, r, w).await;
            });
        })
    }

    #[test]
    fn usb_session_pairs_relays_apdus_and_ends_on_eof() {
        let (phone_end, host_end) = UnixStream::pair().unwrap();
        let host_key = SecretKey::generate();
        let host_id = host_key.public();
        let phone_key = SecretKey::generate();
        let phone_id = phone_key.public();
        let host = fake_host(
            host_end,
            host_key,
            Some(CAP),
            move |phone, mut r, mut w| async move {
                assert_eq!(phone, phone_id);
                assert_eq!(
                    wire::read_frame(&mut r).await.unwrap(),
                    Frame::Hello {
                        capability: Some(CAP),
                        name: "Pixel".into(),
                        push: Some("tok".into()),
                    }
                );
                transport_reply_accepted(&mut w).await;
                assert!(matches!(
                    wire::read_frame(&mut r).await.unwrap(),
                    Frame::Card { generation: 3, .. }
                ));
                // Larger than one USB chunk.
                let data = vec![0xA5; 40_000];
                wire::write_frame(
                    &mut w,
                    &Frame::Apdu {
                        id: 9,
                        generation: 3,
                        data,
                    },
                )
                .await
                .unwrap();
                assert_eq!(
                    wire::read_frame(&mut r).await.unwrap(),
                    Frame::Response {
                        id: 9,
                        generation: 3,
                        data: vec![0x90, 0]
                    }
                );
                // Unplug: the host goes away.
            },
        );

        // Rust gets a raw fd and dups it; the "Kotlin" owner keeps its own.
        let session = Session::usb_open(phone_end.as_raw_fd(), phone_key.to_bytes()).unwrap();
        drop(phone_end);
        assert!(session.is_usb());
        assert_eq!(session.host_id(), host_id.to_string());
        assert_eq!(session.host_name(), "Desk PC");
        assert!(session.pairing_open());
        assert_eq!(session.used_hint(), host_id.to_string());
        // No frames before the host accepted.
        assert!(
            session
                .send(&Frame::Failed {
                    id: 0,
                    generation: 0
                })
                .is_err()
        );

        session
            .usb_hello(true, "Pixel", Some("tok".into()))
            .unwrap();
        assert_eq!(session.host_name(), "PC on USB");
        assert_eq!(session.host_platform(), wire::platform::current());
        assert_eq!(session.host_piv_login(), wire::platform::piv_login());
        assert!(
            session.usb_hello(false, "Pixel", None).is_err(),
            "hello twice"
        );

        session
            .send(&Frame::Card {
                generation: 3,
                atr: vec![0x3b, 0x80, 1, 0x81],
                max_apdu: 65536,
                uid: vec![],
            })
            .unwrap();
        match session.read().unwrap() {
            Frame::Apdu {
                id: 9,
                generation: 3,
                data,
            } => assert_eq!(data, vec![0xA5; 40_000]),
            other => panic!("unexpected {other:?}"),
        }
        session
            .send(&Frame::Response {
                id: 9,
                generation: 3,
                data: vec![0x90, 0],
            })
            .unwrap();
        host.join().unwrap();
        // EOF on the cable ends the session.
        assert!(session.read().is_err());
    }

    async fn transport_reply_accepted<W: tokio::io::AsyncWrite + Unpin>(w: &mut W) {
        wire::write_frame(
            w,
            &Frame::Accepted {
                name: "PC on USB".into(),
                platform: wire::platform::current(),
                piv_login: wire::platform::piv_login(),
            },
        )
        .await
        .unwrap();
    }

    #[test]
    fn pairing_needs_an_open_window() {
        let (phone_end, host_end) = UnixStream::pair().unwrap();
        let host = fake_host(
            host_end,
            SecretKey::generate(),
            None,
            |_, mut r, _w| async move {
                // No Hello ever comes: the phone gives up and the cable closes.
                assert!(wire::read_frame(&mut r).await.is_err());
            },
        );
        let session =
            Session::usb_open(phone_end.as_raw_fd(), SecretKey::generate().to_bytes()).unwrap();
        drop(phone_end);
        assert!(!session.pairing_open());
        let err = session.usb_hello(true, "Pixel", None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        drop(session);
        host.join().unwrap();
    }

    #[test]
    fn paired_reconnect_sends_no_capability_and_close_unblocks_reader() {
        let (phone_end, host_end) = UnixStream::pair().unwrap();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let host = fake_host(
            host_end,
            SecretKey::generate(),
            Some(CAP),
            move |_, mut r, mut w| {
                async move {
                    // Paired: the capability on offer is not used.
                    assert!(matches!(
                        wire::read_frame(&mut r).await.unwrap(),
                        Frame::Hello {
                            capability: None,
                            ..
                        }
                    ));
                    transport_reply_accepted(&mut w).await;
                    // Stay connected and silent until the phone has closed.
                    let _ = tokio::task::spawn_blocking(move || done_rx.recv()).await;
                }
            },
        );
        let session = Arc::new(
            Session::usb_open(phone_end.as_raw_fd(), SecretKey::generate().to_bytes()).unwrap(),
        );
        session.usb_hello(false, "Pixel", None).unwrap();
        let blocked = Arc::clone(&session);
        let (read_tx, read_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || read_tx.send(blocked.read().is_err()).unwrap());
        std::thread::sleep(Duration::from_millis(100));
        session.close();
        assert!(read_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        reader.join().unwrap();
        assert!(
            session
                .send(&Frame::Failed {
                    id: 1,
                    generation: 1
                })
                .is_err()
        );
        drop(session);
        done_tx.send(()).unwrap();
        host.join().unwrap();
    }

    #[test]
    fn rejected_hello_and_bad_fd_fail() {
        assert!(Session::usb_open(-1, [1; 32]).is_err());
        let (phone_end, host_end) = UnixStream::pair().unwrap();
        let host = fake_host(
            host_end,
            SecretKey::generate(),
            None,
            |_, mut r, mut w| async move {
                let _ = wire::read_frame(&mut r).await.unwrap();
                wire::write_frame(&mut w, &Frame::Rejected).await.unwrap();
            },
        );
        let session =
            Session::usb_open(phone_end.as_raw_fd(), SecretKey::generate().to_bytes()).unwrap();
        let err = session.usb_hello(false, "Pixel", None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(session.read().is_err());
        host.join().unwrap();
    }
}
