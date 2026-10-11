//! Bounded, versioned messages. Never log secret capabilities or card bytes.

use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

pub const MAX_APDU: usize = 65536;
pub const MAX_ATR: usize = 33;
pub const MAX_FRAME: usize = 65600;
/// Frame format version byte. v3: `Card` carries a length-prefixed ATR and
/// the tag UID; adds `SetupPiv` and `PivSetupDone`.
pub const VERSION: u8 = 4;
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
        /// The host's OS, one of [`platform`]: the phone shows macOS-only
        /// steps (Mac login) only to a Mac.
        platform: u8,
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
    /// Both ways: one synced card record (see [`crate::cards`]). Sent for
    /// every card on connect and for each local change; the receiver merges
    /// it and sends the result back if it differs.
    CardInfo(crate::cards::CardInfo),
    /// Both ways: ask the other device to do one of the [`action`]s for the
    /// card with this UID (1..=[`MAX_UID`] bytes).
    CardAction {
        uid: Vec<u8>,
        action: u8,
    },
    /// Both ways: the [`Frame::CardAction`] this device was asked for
    /// finished.
    CardActionDone {
        uid: Vec<u8>,
        action: u8,
        ok: bool,
    },
}

/// [`Frame::Accepted`] platforms.
pub mod platform {
    pub const OTHER: u8 = 0;
    pub const MACOS: u8 = 1;
    pub const WINDOWS: u8 = 2;
    pub const MAX: u8 = WINDOWS;

    /// The platform this build runs on.
    pub const fn current() -> u8 {
        if cfg!(target_os = "macos") {
            MACOS
        } else if cfg!(windows) {
            WINDOWS
        } else {
            OTHER
        }
    }
}

/// [`Frame::CardAction`] actions.
pub mod action {
    /// Host -> phone: create the card's PIV certificates (the phone asks for
    /// the PIN; existing certificates are kept).
    pub const SETUP_CERTS: u8 = 1;
    /// Phone -> host: pair the card with the PC's macOS login.
    pub const MAC_LOGIN: u8 = 2;
    pub const MAX: u8 = MAC_LOGIN;
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
            Self::CardInfo(_) => "CardInfo",
            Self::CardAction { .. } => "CardAction",
            Self::CardActionDone { .. } => "CardActionDone",
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
        Frame::CardInfo(card) => card.valid(),
        Frame::CardAction { uid, action } | Frame::CardActionDone { uid, action, .. } => {
            (1..=MAX_UID).contains(&uid.len()) && (1..=action::MAX).contains(action)
        }
        Frame::Apdu { data, .. } => (4..=MAX_APDU).contains(&data.len()),
        Frame::Response { data, .. } => (2..=MAX_APDU).contains(&data.len()),
        Frame::Hello { name, push, .. } => {
            (1..=MAX_NAME).contains(&name.len())
                && push
                    .as_ref()
                    .is_none_or(|push| (1..=MAX_PUSH).contains(&push.len()))
        }
        Frame::Accepted { name, platform } => {
            (1..=MAX_NAME).contains(&name.len()) && *platform <= platform::MAX
        }
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
            let platform = fields.byte()?;
            fields.end()?;
            Ok(Frame::Accepted { name, platform })
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
            let card = crate::cards::CardInfo {
                uid: fields.bytes()?,
                label: fields.string()?,
                label_at: fields.number()?,
                wake: fields.flag()?,
                wake_at: fields.number()?,
                caps: fields.byte()?,
                caps_at: fields.number()?,
                mac_login: fields.flag()?,
                mac_login_at: fields.number()?,
                deleted_at: fields.number()?,
            };
            fields.end()?;
            Ok(Frame::CardInfo(card))
        }
        11 => {
            let mut fields = Fields(data);
            let uid = fields.bytes()?;
            let action = fields.byte()?;
            fields.end()?;
            Ok(Frame::CardAction { uid, action })
        }
        12 => {
            let mut fields = Fields(data);
            let uid = fields.bytes()?;
            let action = fields.byte()?;
            let ok = fields.flag()?;
            fields.end()?;
            Ok(Frame::CardActionDone { uid, action, ok })
        }
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
        Frame::Accepted { name, platform } => {
            body.push(1);
            put_string(&mut body, name);
            body.push(*platform);
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
        Frame::CardInfo(card) => {
            body.push(10);
            put_bytes(&mut body, &card.uid);
            put_string(&mut body, &card.label);
            body.extend_from_slice(&card.label_at.to_be_bytes());
            body.push(u8::from(card.wake));
            body.extend_from_slice(&card.wake_at.to_be_bytes());
            body.push(card.caps);
            body.extend_from_slice(&card.caps_at.to_be_bytes());
            body.push(u8::from(card.mac_login));
            body.extend_from_slice(&card.mac_login_at.to_be_bytes());
            body.extend_from_slice(&card.deleted_at.to_be_bytes());
        }
        Frame::CardAction { uid, action } => {
            body.push(11);
            put_bytes(&mut body, uid);
            body.push(*action);
        }
        Frame::CardActionDone { uid, action, ok } => {
            body.push(12);
            put_bytes(&mut body, uid);
            body.extend_from_slice(&[*action, u8::from(*ok)]);
        }
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

    fn number(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// A 0/1 byte; anything else is malformed.
    fn flag(&mut self) -> io::Result<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid()),
        }
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
