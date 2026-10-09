use tapbridge_core::wire::{Frame, MAX_NAME, MAX_PUSH, read_frame, write_frame};

fn hello(name: &str, push: Option<&str>) -> Frame {
    Frame::Hello {
        capability: None,
        name: name.into(),
        push: push.map(Into::into),
    }
}

#[tokio::test]
async fn carries_pairing_presence_and_cancellation_in_order() {
    let frames = [
        Frame::Hello {
            capability: Some([17; 32]),
            name: "Pixel 9".into(),
            push: Some("fcm-token".into()),
        },
        hello("p", None),
        Frame::Accepted {
            name: "Desk PC \u{1f5a5}".into(),
        },
        Frame::Rejected,
        Frame::Card {
            generation: 4,
            atr: vec![0x3b, 0x00],
            max_apdu: 261,
        },
        Frame::Card {
            generation: 5,
            atr: vec![],
            max_apdu: 0,
        },
        Frame::Apdu {
            id: 10,
            generation: 4,
            data: vec![0, 0xa4, 4, 0],
        },
        Frame::Failed {
            id: 10,
            generation: 4,
        },
        Frame::Cancel { id: 10 },
        Frame::ReadCard,
        Frame::Auth { state: 2 },
    ];
    let mut bytes = Vec::new();
    for frame in &frames {
        write_frame(&mut bytes, frame).await.unwrap();
    }
    let mut reader = bytes.as_slice();
    for frame in frames {
        assert_eq!(read_frame(&mut reader).await.unwrap(), frame);
    }
    assert!(reader.is_empty());
}

#[tokio::test]
async fn rejects_oversized_card_and_apdu_before_writing() {
    let invalid = [
        Frame::Card {
            generation: 1,
            atr: vec![0; 34],
            max_apdu: 261,
        },
        Frame::Card {
            generation: 1,
            atr: vec![],
            max_apdu: 261,
        },
        Frame::Card {
            generation: 1,
            atr: vec![0x3b, 0],
            max_apdu: 0,
        },
        Frame::Apdu {
            id: 1,
            generation: 1,
            data: vec![0; 65537],
        },
        Frame::Response {
            id: 1,
            generation: 1,
            data: vec![0x90],
        },
        hello("", None),
        hello(&"n".repeat(MAX_NAME + 1), None),
        hello("p", Some("")),
        hello("p", Some(&"t".repeat(MAX_PUSH + 1))),
        Frame::Accepted {
            name: String::new(),
        },
        Frame::Accepted {
            name: "n".repeat(MAX_NAME + 1),
        },
    ];
    for frame in invalid {
        let mut out = Vec::new();
        assert!(
            write_frame(&mut out, &frame).await.is_err(),
            "accepted {frame:?}"
        );
        assert!(out.is_empty());
    }
}

#[tokio::test]
async fn rejects_oversized_incoming_apdu() {
    let mut body = vec![2, 4];
    body.extend_from_slice(&[0; 16]);
    body.extend_from_slice(&vec![0; 65537]);
    let mut wire = (body.len() as u32).to_be_bytes().to_vec();
    wire.extend(body);
    assert!(read_frame(&mut wire.as_slice()).await.is_err());
}

#[tokio::test]
async fn rejects_malformed_headers_and_trailing_control_bytes() {
    for body in [
        // v1 frames are refused outright.
        vec![1, 2],
        vec![1, 0, 0],
        vec![2],
        vec![3, 2],
        vec![2, 255],
        vec![2, 2, 0],
        vec![2, 0, 2],
        vec![2, 7, 0],
        // Accepted without a name, with a truncated name, with trailing bytes.
        vec![2, 1],
        vec![2, 1, 0, 3, b'a'],
        vec![2, 1, 0, 1, b'a', 0],
        // Hello: invalid UTF-8 name, empty name, bad push flag, trailing byte.
        vec![2, 0, 0, 0, 1, 0xff, 0],
        vec![2, 0, 0, 0, 0, 0],
        vec![2, 0, 0, 0, 1, b'p', 2],
        vec![2, 0, 0, 0, 1, b'p', 0, 0],
        // Hello with a truncated capability.
        vec![2, 0, 1, 0, 0],
    ] {
        let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
        bytes.extend(body);
        assert_eq!(
            read_frame(&mut bytes.as_slice()).await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
    for length in [0_u32, 1, 65601, u32::MAX] {
        assert_eq!(
            read_frame(&mut length.to_be_bytes().as_slice())
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}

#[tokio::test]
async fn reads_fragmented_frames_and_accepts_exact_apdu_limit() {
    use tokio::io::AsyncWriteExt;
    let expected = Frame::Response {
        id: 123,
        generation: 7,
        data: vec![0x55; 65536],
    };
    let mut encoded = Vec::new();
    write_frame(&mut encoded, &expected).await.unwrap();
    let (mut send, mut recv) = tokio::io::duplex(7);
    let writer = tokio::spawn(async move {
        for chunk in encoded.chunks(3) {
            send.write_all(chunk).await.unwrap();
        }
    });
    assert_eq!(read_frame(&mut recv).await.unwrap(), expected);
    writer.await.unwrap();
}

#[tokio::test]
async fn frame_layout_is_fixed_and_secrets_are_redacted() {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &hello("p", Some("tk")))
        .await
        .unwrap();
    assert_eq!(
        bytes,
        [0, 0, 0, 11, 2, 0, 0, 0, 1, b'p', 1, 0, 2, b't', b'k']
    );
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &Frame::Accepted { name: "PC".into() })
        .await
        .unwrap();
    assert_eq!(bytes, [0, 0, 0, 6, 2, 1, 0, 2, b'P', b'C']);
    assert_eq!(
        format!(
            "{:?}",
            Frame::Hello {
                capability: Some([88; 32]),
                name: "secret-name".into(),
                push: Some("secret-token".into()),
            }
        ),
        "Hello(REDACTED)"
    );
    assert_eq!(
        format!(
            "{:?}",
            Frame::Apdu {
                id: 1,
                generation: 1,
                data: vec![0, 0xa4, 4, 0]
            }
        ),
        "Apdu(REDACTED)"
    );
}

#[tokio::test]
async fn transports_card_bytes_without_changing_status_words() {
    let frame = Frame::Response {
        id: 42,
        generation: 7,
        data: vec![0x00, 0xff, 0x61, 0x00, 0x90, 0x00],
    };
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &frame).await.unwrap();
    assert_eq!(read_frame(&mut bytes.as_slice()).await.unwrap(), frame);
}

#[tokio::test]
async fn auth_states_outside_the_known_range_are_rejected() {
    let mut bytes = Vec::new();
    assert!(
        write_frame(
            &mut bytes,
            &Frame::Auth {
                state: tapbridge_core::wire::auth::MAX + 1
            }
        )
        .await
        .is_err()
    );
    // Hand-built frame with an unknown state: length 3, version, kind 9, state 0x7f.
    let raw = [0, 0, 0, 3, tapbridge_core::wire::VERSION, 9, 0x7f];
    assert!(read_frame(&mut raw.as_slice()).await.is_err());
}
