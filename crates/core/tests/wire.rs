use tapbridge_core::wire::{Frame, read_frame, write_frame};

#[tokio::test]
async fn carries_pairing_presence_and_cancellation_in_order() {
    let frames = [
        Frame::Hello {
            capability: Some([17; 32]),
        },
        Frame::Hello { capability: None },
        Frame::Accepted,
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
    let mut body = vec![1, 4];
    body.extend_from_slice(&[0; 16]);
    body.extend_from_slice(&vec![0; 65537]);
    let mut wire = (body.len() as u32).to_be_bytes().to_vec();
    wire.extend(body);
    assert!(read_frame(&mut wire.as_slice()).await.is_err());
}

#[tokio::test]
async fn rejects_malformed_headers_and_trailing_control_bytes() {
    for body in [
        vec![1],
        vec![2, 1],
        vec![1, 255],
        vec![1, 1, 0],
        vec![1, 0, 2],
        vec![1, 7, 0],
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
    write_frame(&mut bytes, &Frame::Hello { capability: None })
        .await
        .unwrap();
    assert_eq!(bytes, [0, 0, 0, 3, 1, 0, 0]);
    assert_eq!(
        format!(
            "{:?}",
            Frame::Hello {
                capability: Some([88; 32])
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
