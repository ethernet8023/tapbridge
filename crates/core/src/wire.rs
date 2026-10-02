//! Bounded, versioned messages. Never log secret capabilities or card bytes.

use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

pub const MAX_APDU: usize = 65536;
pub const MAX_ATR: usize = 33;
pub const MAX_FRAME: usize = 65600;

#[derive(Clone, PartialEq, Eq)]
pub enum Frame {
    Hello {
        capability: Option<[u8; 32]>,
    },
    Accepted,
    Rejected,
    Card {
        generation: u64,
        atr: Vec<u8>,
        max_apdu: u32,
    },
    Apdu {
        id: u64,
        generation: u64,
        data: Vec<u8>,
    },
    Response {
        id: u64,
        generation: u64,
        data: Vec<u8>,
    },
    Failed {
        id: u64,
        generation: u64,
    },
    Cancel {
        id: u64,
    },
    ReadCard,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Hello { .. } => "Hello(REDACTED)",
            Self::Accepted => "Accepted",
            Self::Rejected => "Rejected",
            Self::Card { .. } => "Card",
            Self::Apdu { .. } => "Apdu(REDACTED)",
            Self::Response { .. } => "Response(REDACTED)",
            Self::Failed { .. } => "Failed",
            Self::Cancel { .. } => "Cancel",
            Self::ReadCard => "ReadCard",
        })
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid reader frame")
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Frame> {
    use tokio::io::AsyncReadExt;
    let size = reader.read_u32().await? as usize;
    if !(2..=MAX_FRAME).contains(&size) {
        return Err(invalid());
    }
    let mut body = vec![0; size];
    reader.read_exact(&mut body).await?;
    let frame = decode(&body)?;
    validate(&frame)?;
    Ok(frame)
}

fn validate(frame: &Frame) -> io::Result<()> {
    let valid = match frame {
        Frame::Card { atr, max_apdu, .. } => {
            atr.len() <= MAX_ATR
                && if atr.is_empty() {
                    *max_apdu == 0
                } else {
                    atr.len() >= 2 && (4..=MAX_APDU as u32).contains(max_apdu)
                }
        }
        Frame::Apdu { data, .. } => (4..=MAX_APDU).contains(&data.len()),
        Frame::Response { data, .. } => (2..=MAX_APDU).contains(&data.len()),
        _ => true,
    };
    if valid { Ok(()) } else { Err(invalid()) }
}

fn decode(body: &[u8]) -> io::Result<Frame> {
    if body.len() < 2 || body[0] != 1 {
        return Err(invalid());
    }
    let data = &body[2..];
    let number = |at: usize| -> u64 { u64::from_be_bytes(data[at..at + 8].try_into().unwrap()) };
    match body[1] {
        0 => match data {
            [0] => Ok(Frame::Hello { capability: None }),
            [1, rest @ ..] if rest.len() == 32 => Ok(Frame::Hello {
                capability: Some(rest.try_into().unwrap()),
            }),
            _ => Err(invalid()),
        },
        1 if data.is_empty() => Ok(Frame::Accepted),
        2 if data.is_empty() => Ok(Frame::Rejected),
        3 if data.len() >= 12 => Ok(Frame::Card {
            generation: number(0),
            max_apdu: u32::from_be_bytes(data[8..12].try_into().unwrap()),
            atr: data[12..].to_vec(),
        }),
        kind @ (4 | 5) if data.len() >= 16 => {
            let (id, generation, bytes) = (number(0), number(8), data[16..].to_vec());
            if kind == 4 {
                Ok(Frame::Apdu {
                    id,
                    generation,
                    data: bytes,
                })
            } else {
                Ok(Frame::Response {
                    id,
                    generation,
                    data: bytes,
                })
            }
        }
        6 if data.len() == 16 => Ok(Frame::Failed {
            id: number(0),
            generation: number(8),
        }),
        7 if data.len() == 8 => Ok(Frame::Cancel { id: number(0) }),
        8 if data.is_empty() => Ok(Frame::ReadCard),
        _ => Err(invalid()),
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    use tokio::io::AsyncWriteExt;
    validate(frame)?;
    let mut body = vec![1];
    match frame {
        Frame::Hello { capability } => {
            body.extend_from_slice(&[0, u8::from(capability.is_some())]);
            if let Some(capability) = capability {
                body.extend_from_slice(capability);
            }
        }
        Frame::Accepted => body.push(1),
        Frame::Rejected => body.push(2),
        Frame::Card {
            generation,
            atr,
            max_apdu,
        } => {
            body.push(3);
            body.extend_from_slice(&generation.to_be_bytes());
            body.extend_from_slice(&max_apdu.to_be_bytes());
            body.extend_from_slice(atr);
        }
        Frame::Apdu {
            id,
            generation,
            data,
        }
        | Frame::Response {
            id,
            generation,
            data,
        } => {
            if data.len() > MAX_APDU {
                return Err(invalid());
            }
            body.push(if matches!(frame, Frame::Apdu { .. }) {
                4
            } else {
                5
            });
            body.extend_from_slice(&id.to_be_bytes());
            body.extend_from_slice(&generation.to_be_bytes());
            body.extend_from_slice(data);
        }
        Frame::Failed { id, generation } => {
            body.push(6);
            body.extend_from_slice(&id.to_be_bytes());
            body.extend_from_slice(&generation.to_be_bytes());
        }
        Frame::Cancel { id } => {
            body.push(7);
            body.extend_from_slice(&id.to_be_bytes());
        }
        Frame::ReadCard => body.push(8),
    }
    writer.write_u32(body.len() as u32).await?;
    writer.write_all(&body).await
}
