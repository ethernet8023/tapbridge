use tapbridge_core::cards::{CardInfo, caps};
use tapbridge_core::wire::{
    Frame, MAX_NAME, MAX_PUSH, MAX_UID, VERSION, action, platform, read_frame, write_frame,
};

fn card() -> CardInfo {
    CardInfo::new(
        vec![0x04, 0x2C, 0x26, 0x82, 0xF2, 0x6F, 0x80],
        "hand \u{270b}".into(),
        true,
        caps::FIDO | caps::PIV | caps::CERTS,
        1_791_670_000_000,
    )
}

#[tokio::test]
async fn a_bad_flag_byte_in_card_info_is_rejected() {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &Frame::CardInfo(card()))
        .await
        .unwrap();
    // The wake flag sits right after uid, label and label_at.
    let wake = 4 + 2 + 1 + 7 + 2 + "hand \u{270b}".len() + 8;
    assert_eq!(bytes[wake], 1);
    bytes[wake] = 2;
    assert_eq!(
        read_frame(&mut bytes.as_slice()).await.unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}

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
            platform: platform::MACOS,
        },
        Frame::Rejected,
        Frame::Card {
            generation: 4,
            atr: vec![0x3b, 0x00],
            max_apdu: 261,
            uid: vec![],
        },
        Frame::Card {
            generation: 6,
            atr: vec![0x3b, 0x00],
            max_apdu: 261,
            uid: vec![0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        },
        Frame::Card {
            generation: 5,
            atr: vec![],
            max_apdu: 0,
            uid: vec![],
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
        Frame::CardInfo(card()),
        Frame::CardInfo(CardInfo {
            deleted_at: 99,
            mac_login: true,
            mac_login_at: 7,
            ..card()
        }),
        Frame::CardAction {
            uid: vec![0x04, 1, 2, 3, 4, 5, 6],
            action: action::SETUP_CERTS,
        },
        Frame::CardActionDone {
            uid: vec![0x04, 1, 2, 3],
            action: action::MAC_LOGIN,
            ok: true,
        },
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
            uid: vec![],
        },
        Frame::Card {
            generation: 1,
            atr: vec![],
            max_apdu: 261,
            uid: vec![],
        },
        Frame::Card {
            generation: 1,
            atr: vec![0x3b, 0],
            max_apdu: 0,
            uid: vec![],
        },
        // UID longer than any ISO 14443 UID.
        Frame::Card {
            generation: 1,
            atr: vec![0x3b, 0],
            max_apdu: 261,
            uid: vec![1; MAX_UID + 1],
        },
        // An absent card has no UID.
        Frame::Card {
            generation: 1,
            atr: vec![],
            max_apdu: 0,
            uid: vec![4, 1, 2, 3],
        },
        Frame::CardAction {
            uid: vec![],
            action: action::SETUP_CERTS,
        },
        Frame::CardAction {
            uid: vec![1; MAX_UID + 1],
            action: action::SETUP_CERTS,
        },
        Frame::CardAction {
            uid: vec![4, 1],
            action: 0,
        },
        Frame::CardActionDone {
            uid: vec![4, 1],
            action: action::MAX + 1,
            ok: true,
        },
        Frame::CardInfo(CardInfo {
            label: String::new(),
            ..card()
        }),
        Frame::CardInfo(CardInfo {
            uid: vec![],
            ..card()
        }),
        Frame::CardInfo(CardInfo {
            caps: 0x80,
            ..card()
        }),
        Frame::Accepted {
            name: "PC".into(),
            platform: platform::MAX + 1,
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
            platform: platform::MACOS,
        },
        Frame::Accepted {
            name: "n".repeat(MAX_NAME + 1),
            platform: platform::MACOS,
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
    let mut body = vec![VERSION, 4];
    body.extend_from_slice(&[0; 16]);
    body.extend_from_slice(&vec![0; 65537]);
    let mut wire = (body.len() as u32).to_be_bytes().to_vec();
    wire.extend(body);
    assert!(read_frame(&mut wire.as_slice()).await.is_err());
}

#[tokio::test]
async fn rejects_malformed_headers_and_trailing_control_bytes() {
    let v = VERSION;
    for body in [
        // Older versions are refused outright.
        vec![1, 2],
        vec![1, 0, 0],
        vec![2, 2],
        vec![2, 8],
        vec![v],
        vec![v + 1, 2],
        vec![v, 255],
        vec![v, 2, 0],
        vec![v, 0, 2],
        vec![v, 7, 0],
        // Accepted without a name, with a truncated name, without a
        // platform, with trailing bytes.
        vec![v, 1],
        vec![v, 1, 0, 3, b'a'],
        vec![v, 1, 0, 1, b'a'],
        vec![v, 1, 0, 1, b'a', 1, 0],
        // Hello: invalid UTF-8 name, empty name, bad push flag, trailing byte.
        vec![v, 0, 0, 0, 1, 0xff, 0],
        vec![v, 0, 0, 0, 0, 0],
        vec![v, 0, 0, 0, 1, b'p', 2],
        vec![v, 0, 0, 0, 1, b'p', 0, 0],
        // Hello with a truncated capability.
        vec![v, 0, 1, 0, 0],
        // CardAction: missing action, truncated UID, trailing byte.
        vec![v, 11, 1, 4],
        vec![v, 11, 4, 1, 2, 3],
        vec![v, 11, 1, 4, 1, 0],
        // CardActionDone: missing flag, flag out of range, trailing byte.
        vec![v, 12, 1, 4, 1],
        vec![v, 12, 1, 4, 1, 2],
        vec![v, 12, 1, 4, 1, 1, 0],
        // CardInfo: truncated.
        vec![v, 10],
        vec![v, 10, 1, 4, 0, 1, b'a'],
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
        [0, 0, 0, 11, VERSION, 0, 0, 0, 1, b'p', 1, 0, 2, b't', b'k']
    );
    let mut bytes = Vec::new();
    write_frame(
        &mut bytes,
        &Frame::Accepted {
            name: "PC".into(),
            platform: platform::MACOS,
        },
    )
    .await
    .unwrap();
    assert_eq!(bytes, [0, 0, 0, 7, VERSION, 1, 0, 2, b'P', b'C', 1]);
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

/// Card layout: generation, max APDU, then length-prefixed ATR and UID.
#[tokio::test]
async fn card_frame_layout_carries_length_prefixed_atr_and_uid() {
    let mut bytes = Vec::new();
    write_frame(
        &mut bytes,
        &Frame::Card {
            generation: 1,
            atr: vec![0x3b, 0x00],
            max_apdu: 261,
            uid: vec![0x04, 0xaa, 0xbb, 0xcc],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        bytes,
        [
            0, 0, 0, 22, VERSION, 3, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 5, 2, 0x3b, 0x00, 4, 0x04,
            0xaa, 0xbb, 0xcc
        ]
    );
}

#[tokio::test]
async fn card_frames_with_bad_lengths_are_rejected_on_read() {
    let card = |tail: &[u8]| {
        let mut body = vec![VERSION, 3];
        body.extend_from_slice(&1u64.to_be_bytes());
        body.extend_from_slice(&261u32.to_be_bytes());
        body.extend_from_slice(tail);
        let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
        bytes.extend(body);
        bytes
    };
    // Sanity: a well-formed one reads back.
    let good = card(&[2, 0x3b, 0, 1, 7]);
    assert_eq!(
        read_frame(&mut good.as_slice()).await.unwrap(),
        Frame::Card {
            generation: 1,
            atr: vec![0x3b, 0],
            max_apdu: 261,
            uid: vec![7],
        }
    );
    for tail in [
        // No UID length.
        vec![2, 0x3b, 0],
        // ATR length past the end.
        vec![5, 0x3b, 0, 0],
        // UID length past the end.
        vec![2, 0x3b, 0, 3, 1, 2],
        // UID too long.
        [vec![2, 0x3b, 0, 11], vec![1; 11]].concat(),
        // ATR too long.
        [vec![34], vec![0; 34], vec![0]].concat(),
        // Trailing byte.
        vec![2, 0x3b, 0, 0, 0],
        // The old layout (ATR to the end).
        vec![0x3b, 0],
    ] {
        let bytes = card(&tail);
        assert!(
            read_frame(&mut bytes.as_slice()).await.is_err(),
            "accepted {tail:?}"
        );
    }
}
