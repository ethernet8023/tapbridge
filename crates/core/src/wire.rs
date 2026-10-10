//! Bounded, versioned messages. Never log secret capabilities or card bytes.

use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

pub const MAX_APDU: usize = 65536;
pub const MAX_ATR: usize = 33;
pub const MAX_FRAME: usize = 65600;
/// Frame format version byte. v3: `Card` carries a length-prefixed ATR and
/// the tag UID; adds `SetupPiv` and `PivSetupDone`.
pub const VERSION: u8 = 3;
/// NFC tag UID (ISO 14443: 4, 7 or 10 bytes).
pub const MAX_UID: usize = 10;
/// Display names (phone and host), UTF-8 bytes.
pub const MAX_NAME: usize = 64;
/// FCM registration token, UTF-8 bytes.
pub const MAX_PUSH: usize = 4096;

#[derive(Clone, PartialEq, Eq)]
pub enum Frame {
    Hello {
        capability: Option<[u8; 32]>,
        name: String,
        push: Option<String>,
    },
    Accepted {
        name: String,
    },
    Rejected,
    Card {
        generation: u64,
        atr: Vec<u8>,
        max_apdu: u32,
        /// The tag's NFC UID (`Tag.getId()`), 0..=[`MAX_UID`] bytes. Empty
        /// when the card is absent, has none, or presents a random one.
        uid: Vec<u8>,
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
    /// Host -> phone: where the current sign-in stands, for the phone's
    /// tap animation. One of the [`auth`] constants.
    Auth {
        state: u8,
    },
    /// Host -> phone: open the phone's smart-card (PIV) setup for the card
    /// with this UID (1..=[`MAX_UID`] bytes).
    SetupPiv {
        uid: Vec<u8>,
    },
    /// Phone -> host: the PIV setup the host asked for finished.
    PivSetupDone {
        ok: bool,
    },
}

/// [`Frame::Auth`] states.
pub mod auth {
    /// Nothing in progress.
    pub const IDLE: u8 = 0;
    /// The card is being used: keep holding it.
    pub const WORKING: u8 = 1;
    /// The sign-in (or registration) succeeded.
    pub const SUCCESS: u8 = 2;
    /// The card left mid-operation; the host retries after the next tap.
    pub const REMOVED: u8 = 3;
    /// The PC is asking the user for the card's PIN.
    pub const PIN: u8 = 4;
    /// The operation failed (wrong PIN, timeout, card error).
    pub const FAILED: u8 = 5;
    pub const MAX: u8 = FAILED;
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Hello { .. } => "Hello(REDACTED)",
            Self::Accepted { .. } => "Accepted",
            Self::Rejected => "Rejected",
            Self::Card { .. } => "Card",
            Self::Apdu { .. } => "Apdu(REDACTED)",
            Self::Response { .. } => "Response(REDACTED)",
            Self::Failed { .. } => "Failed",
            Self::Cancel { .. } => "Cancel",
            Self::ReadCard => "ReadCard",
            Self::Auth { .. } => "Auth",
            Self::SetupPiv { .. } => "SetupPiv",
            Self::PivSetupDone { .. } => "PivSetupDone",
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
        Frame::Card {
            atr, max_apdu, uid, ..
        } => {
            atr.len() <= MAX_ATR
                && uid.len() <= MAX_UID
                && if atr.is_empty() {
                    *max_apdu == 0 && uid.is_empty()
                } else {
                    atr.len() >= 2 && (4..=MAX_APDU as u32).contains(max_apdu)
                }
        }
        Frame::SetupPiv { uid } => (1..=MAX_UID).contains(&uid.len()),
        Frame::Apdu { data, .. } => (4..=MAX_APDU).contains(&data.len()),
        Frame::Response { data, .. } => (2..=MAX_APDU).contains(&data.len()),
        Frame::Hello { name, push, .. } => {
            (1..=MAX_NAME).contains(&name.len())
                && push
                    .as_ref()
                    .is_none_or(|push| (1..=MAX_PUSH).contains(&push.len()))
        }
        Frame::Accepted { name } => (1..=MAX_NAME).contains(&name.len()),
        Frame::Auth { state } => *state <= auth::MAX,
        _ => true,
    };
    if valid { Ok(()) } else { Err(invalid()) }
}

fn decode(body: &[u8]) -> io::Result<Frame> {
    if body.len() < 2 || body[0] != VERSION {
        return Err(invalid());
    }
    let data = &body[2..];
    let number = |at: usize| -> u64 { u64::from_be_bytes(data[at..at + 8].try_into().unwrap()) };
    match body[1] {
        0 => {
            let mut fields = Fields(data);
            let capability = match fields.byte()? {
                0 => None,
                1 => Some(fields.take(32)?.try_into().unwrap()),
                _ => return Err(invalid()),
            };
            let name = fields.string()?;
            let push = match fields.byte()? {
                0 => None,
                1 => Some(fields.string()?),
                _ => return Err(invalid()),
            };
            fields.end()?;
            Ok(Frame::Hello {
                capability,
                name,
                push,
            })
        }
        1 => {
            let mut fields = Fields(data);
            let name = fields.string()?;
            fields.end()?;
            Ok(Frame::Accepted { name })
        }
        2 if data.is_empty() => Ok(Frame::Rejected),
        3 if data.len() >= 12 => {
            let mut fields = Fields(&data[12..]);
            let atr = fields.bytes()?;
            let uid = fields.bytes()?;
            fields.end()?;
            Ok(Frame::Card {
                generation: number(0),
                max_apdu: u32::from_be_bytes(data[8..12].try_into().unwrap()),
                atr,
                uid,
            })
        }
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
        9 if data.len() == 1 => Ok(Frame::Auth { state: data[0] }),
        10 => {
            let mut fields = Fields(data);
            let uid = fields.bytes()?;
            fields.end()?;
            Ok(Frame::SetupPiv { uid })
        }
        11 if data.len() == 1 && data[0] <= 1 => Ok(Frame::PivSetupDone { ok: data[0] == 1 }),
        _ => Err(invalid()),
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    use tokio::io::AsyncWriteExt;
    validate(frame)?;
    let mut body = vec![VERSION];
    match frame {
        Frame::Hello {
            capability,
            name,
            push,
        } => {
            body.extend_from_slice(&[0, u8::from(capability.is_some())]);
            if let Some(capability) = capability {
                body.extend_from_slice(capability);
            }
            put_string(&mut body, name);
            body.push(u8::from(push.is_some()));
            if let Some(push) = push {
                put_string(&mut body, push);
            }
        }
        Frame::Accepted { name } => {
            body.push(1);
            put_string(&mut body, name);
        }
        Frame::Rejected => body.push(2),
        Frame::Card {
            generation,
            atr,
            max_apdu,
            uid,
        } => {
            body.push(3);
            body.extend_from_slice(&generation.to_be_bytes());
            body.extend_from_slice(&max_apdu.to_be_bytes());
            put_bytes(&mut body, atr);
            put_bytes(&mut body, uid);
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
        Frame::Auth { state } => body.extend_from_slice(&[9, *state]),
        Frame::SetupPiv { uid } => {
            body.push(10);
            put_bytes(&mut body, uid);
        }
        Frame::PivSetupDone { ok } => body.extend_from_slice(&[11, u8::from(*ok)]),
    }
    writer.write_u32(body.len() as u32).await?;
    writer.write_all(&body).await
}

/// Length-prefixed (u16 big-endian) UTF-8 field.
fn put_string(body: &mut Vec<u8>, value: &str) {
    body.extend_from_slice(&(value.len() as u16).to_be_bytes());
    body.extend_from_slice(value.as_bytes());
}

/// Short (u8-length-prefixed) byte field: ATR and UID. Callers validate the
/// length bound first, so it always fits.
fn put_bytes(body: &mut Vec<u8>, value: &[u8]) {
    body.push(value.len() as u8);
    body.extend_from_slice(value);
}

/// Strict cursor over variable-length frame fields.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < count {
            return Err(invalid());
        }
        let (head, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(head)
    }

    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let length = self.byte()? as usize;
        Ok(self.take(length)?.to_vec())
    }

    fn string(&mut self) -> io::Result<String> {
        let length = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| invalid())
    }

    fn end(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}
