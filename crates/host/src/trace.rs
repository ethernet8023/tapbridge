//! Opt-in, payload-free summaries of the APDU traffic between PC and phone.
//!
//! Prints command class, instruction, CTAP command name and status words.
//! Never prints data bytes, credential ids, PIN material or rp hashes.

use std::hash::{DefaultHasher, Hash, Hasher};
use tapbridge_core::wire::Frame;

#[derive(Default)]
pub struct Tracer {
    /// CTAP2 command byte of the request currently awaiting a response.
    pending_ctap: Option<u8>,
    /// Command byte of an unfinished chained request (CLA 0x90 chunks sent so far).
    chain_cmd: Option<u8>,
    /// Previous request was a non-final chained chunk (CLA 0x90).
    chaining: bool,
    /// (id, in-process hash) of recent CTAP2 requests, only to spot exact repeats.
    recent: Vec<(u64, u64)>,
}

impl Tracer {
    /// Describe a frame crossing the PC/phone link; `None` for frames not worth a line.
    pub fn observe(&mut self, frame: &Frame) -> Option<String> {
        match frame {
            Frame::Apdu { id, data, .. } => {
                Some(format!("pc -> card  #{id} {}", self.request(*id, data)))
            }
            Frame::Response { id, data, .. } => {
                Some(format!("card -> pc  #{id} {}", self.response(data)))
            }
            Frame::Failed { id, .. } => {
                self.pending_ctap = None;
                Some(format!(
                    "card -> pc  #{id} transceive FAILED (tag lost or I/O error)"
                ))
            }
            Frame::Card { atr, .. } if atr.is_empty() => Some("card removed".into()),
            Frame::Card { atr, .. } => Some(format!("card present (ATR {} bytes)", atr.len())),
            _ => None,
        }
    }

    fn request(&mut self, id: u64, apdu: &[u8]) -> String {
        let [cla, ins, p1, p2, rest @ ..] = apdu else {
            self.pending_ctap = None;
            return format!("malformed APDU, {} bytes", apdu.len());
        };
        let lc = rest.first().copied().unwrap_or(0);
        let body = if rest.len() > 1 { &rest[1..] } else { &[][..] };
        let was_chaining = std::mem::replace(&mut self.chaining, false);
        self.pending_ctap = None;
        let what = match (*cla, *ins) {
            (_, 0xA4) if body.starts_with(&[0xA0, 0x00, 0x00, 0x06, 0x47, 0x2F, 0x00, 0x01]) => {
                "SELECT FIDO applet".to_string()
            }
            (_, 0xA4) => "SELECT other applet".to_string(),
            (0x80 | 0x90, 0x10) => {
                self.chaining = *cla == 0x90;
                let cmd = if was_chaining {
                    self.chain_cmd
                } else {
                    body.first().copied()
                };
                self.chain_cmd = if self.chaining { cmd } else { None };
                self.pending_ctap = cmd;
                let repeat = self.note_repeat(id, apdu);
                let name = cmd.map_or("?".into(), |c| ctap_command(c, body, was_chaining));
                let mut label = if was_chaining {
                    format!("CTAP2 {name} (continued chunk)")
                } else {
                    format!("CTAP2 {name}")
                };
                if let Some(earlier) = repeat {
                    label.push_str(&format!(" [exact repeat of #{earlier}]"));
                }
                label
            }
            (0x00, 0xC0) => "GET RESPONSE".to_string(),
            (0x00, 0x01) => "U2F register".to_string(),
            (0x00, 0x02) => format!("U2F authenticate ({})", u2f_control(*p1)),
            (0x00, 0x03) => "U2F version".to_string(),
            _ => format!("other CLA={cla:02X} INS={ins:02X}"),
        };
        format!(
            "{what}  [CLA={cla:02X} INS={ins:02X} P1={p1:02X} P2={p2:02X} Lc={lc} bytes={}]",
            apdu.len()
        )
    }

    /// Remember this request's hash; return the id of an identical earlier request.
    fn note_repeat(&mut self, id: u64, apdu: &[u8]) -> Option<u64> {
        let mut hasher = DefaultHasher::new();
        apdu.hash(&mut hasher);
        let hash = hasher.finish();
        let earlier = self
            .recent
            .iter()
            .find(|(_, h)| *h == hash)
            .map(|(i, _)| *i);
        if self.recent.len() == 16 {
            self.recent.remove(0);
        }
        self.recent.push((id, hash));
        earlier
    }

    fn response(&mut self, data: &[u8]) -> String {
        let [.., sw1, sw2] = data else {
            return format!("malformed response, {} bytes", data.len());
        };
        let payload = &data[..data.len() - 2];
        let sw = u16::from_be_bytes([*sw1, *sw2]);
        let ctap = self.pending_ctap.take();
        let mut text = format!(
            "SW={sw:04X} ({}) payload={} bytes",
            sw_name(sw),
            payload.len()
        );
        if payload.starts_with(b"U2F_V2") || payload.starts_with(b"FIDO_2_0") {
            let name = if payload.starts_with(b"U2F_V2") {
                "U2F_V2"
            } else {
                "FIDO_2_0"
            };
            text.push_str(&format!("  applet speaks {name}"));
        } else if let (Some(_), Some(status), true) =
            (ctap, payload.first(), sw == 0x9000 || sw & 0xFF00 == 0x6100)
        {
            text.push_str(&format!("  CTAP status {}", ctap_status(*status)));
        }
        text
    }
}

/// clientPIN subCommand (map key 2) when the leading map entries are small unsigned ints.
fn client_pin_sub_command(body: &[u8]) -> Option<u8> {
    let entries = (*body.get(1)?).checked_sub(0xA0).filter(|n| *n <= 6)?;
    let mut at = 2;
    for _ in 0..entries {
        let (key, value) = (*body.get(at)?, *body.get(at + 1)?);
        if key > 0x17 || value > 0x17 {
            return None;
        }
        if key == 2 {
            return Some(value);
        }
        at += 2;
    }
    None
}

fn client_pin_name(sub: u8) -> &'static str {
    match sub {
        0x01 => "getPINRetries",
        0x02 => "getKeyAgreement",
        0x03 => "setPIN",
        0x04 => "changePIN",
        0x05 => "getPinToken",
        0x06 => "getPinUvAuthTokenUsingUvWithPermissions",
        0x07 => "getUVRetries",
        0x09 => "getPinUvAuthTokenUsingPinWithPermissions",
        _ => "unknown subCommand",
    }
}

fn ctap_command(byte: u8, body: &[u8], continued: bool) -> String {
    match byte {
        0x01 => "makeCredential".into(),
        0x02 => "getAssertion".into(),
        0x04 => "getInfo".into(),
        0x06 if !continued => match client_pin_sub_command(body) {
            Some(sub) => format!("clientPIN/{}", client_pin_name(sub)),
            None => "clientPIN".into(),
        },
        0x06 => "clientPIN".into(),
        0x07 => "reset".into(),
        0x08 => "getNextAssertion".into(),
        other => format!("command 0x{other:02X}"),
    }
}

fn ctap_status(byte: u8) -> String {
    let name = match byte {
        0x00 => "OK",
        0x01 => "INVALID_COMMAND",
        0x11 => "CBOR_UNEXPECTED_TYPE",
        0x12 => "INVALID_CBOR",
        0x14 => "MISSING_PARAMETER",
        0x15 => "LIMIT_EXCEEDED",
        0x19 => "CREDENTIAL_EXCLUDED",
        0x27 => "OPERATION_DENIED",
        0x2B => "UNSUPPORTED_OPTION",
        0x2C => "INVALID_OPTION",
        0x30 => "NOT_ALLOWED",
        0x2E => "NO_CREDENTIALS (card holds none of the credentials the site listed)",
        0x31 => "PIN_INVALID",
        0x32 => "PIN_BLOCKED",
        0x33 => "PIN_AUTH_INVALID",
        0x34 => "PIN_AUTH_BLOCKED",
        0x35 => "PIN_NOT_SET",
        0x36 => "PIN_REQUIRED",
        0x38 => "PIN_TOKEN_EXPIRED",
        0x39 => "REQUEST_TOO_LARGE",
        0x3A => "ACTION_TIMEOUT",
        0x3B => "UP_REQUIRED",
        0x40 => "UNAUTHORIZED_PERMISSION",
        _ => return format!("0x{byte:02X}"),
    };
    format!("0x{byte:02X} {name}")
}

fn u2f_control(p1: u8) -> &'static str {
    match p1 {
        0x03 => "check-only",
        0x07 => "enforce user presence",
        0x08 => "no user presence",
        _ => "unknown control",
    }
}

fn sw_name(sw: u16) -> &'static str {
    match sw {
        0x9000 => "ok",
        0x6985 => "conditions not satisfied / touch needed",
        0x6A80 => "wrong data / key handle not from this card",
        0x6A82 => "file not found",
        0x6700 => "wrong length",
        0x6D00 => "INS not supported",
        0x6E00 => "CLA not supported",
        s if s & 0xFF00 == 0x6100 => "more data, GET RESPONSE",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apdu(data: &[u8]) -> Frame {
        Frame::Apdu {
            id: 7,
            generation: 1,
            data: data.to_vec(),
        }
    }
    fn response(data: &[u8]) -> Frame {
        Frame::Response {
            id: 7,
            generation: 1,
            data: data.to_vec(),
        }
    }

    #[test]
    fn names_ctap2_get_assertion_and_no_credentials_status() {
        let mut t = Tracer::default();
        let request = t
            .observe(&apdu(&[0x80, 0x10, 0, 0, 3, 0x02, 0xAA, 0xBB, 0]))
            .unwrap();
        assert!(request.contains("CTAP2 getAssertion"), "{request}");
        let reply = t.observe(&response(&[0x2E, 0x90, 0x00])).unwrap();
        assert!(reply.contains("NO_CREDENTIALS"), "{reply}");
        assert!(reply.contains("SW=9000"), "{reply}");
    }

    #[test]
    fn names_u2f_wrong_key_handle() {
        let mut t = Tracer::default();
        let request = t.observe(&apdu(&[0, 0x02, 0x03, 0, 2, 9, 9])).unwrap();
        assert!(
            request.contains("U2F authenticate (check-only)"),
            "{request}"
        );
        let reply = t.observe(&response(&[0x6A, 0x80])).unwrap();
        assert!(reply.contains("key handle not from this card"), "{reply}");
    }

    #[test]
    fn select_reports_applet_version_without_data_dump() {
        let mut t = Tracer::default();
        let select = [0, 0xA4, 4, 0, 8, 0xA0, 0, 0, 6, 0x47, 0x2F, 0, 1, 0];
        assert!(
            t.observe(&apdu(&select))
                .unwrap()
                .contains("SELECT FIDO applet")
        );
        let mut reply = b"U2F_V2".to_vec();
        reply.extend([0x90, 0]);
        assert!(
            t.observe(&response(&reply))
                .unwrap()
                .contains("applet speaks U2F_V2")
        );
    }

    #[test]
    fn chained_continuation_does_not_misread_a_command_byte() {
        let mut t = Tracer::default();
        assert!(
            t.observe(&apdu(&[0x90, 0x10, 0, 0, 2, 0x01, 0x55]))
                .unwrap()
                .contains("makeCredential")
        );
        assert!(
            t.observe(&apdu(&[0x80, 0x10, 0, 0, 2, 0x04, 0x66]))
                .unwrap()
                .contains("continued")
        );
    }

    #[test]
    fn final_chained_chunk_response_shows_ctap_status() {
        let mut t = Tracer::default();
        t.observe(&apdu(&[0x90, 0x10, 0, 0, 2, 0x01, 0x55]));
        t.observe(&response(&[0x90, 0x00]));
        let last = t
            .observe(&apdu(&[0x80, 0x10, 0, 0, 2, 0x66, 0x77]))
            .unwrap();
        assert!(last.contains("makeCredential (continued chunk)"), "{last}");
        let reply = t.observe(&response(&[0x36, 0x90, 0x00])).unwrap();
        assert!(reply.contains("0x36 PIN_REQUIRED"), "{reply}");
    }

    #[test]
    fn names_client_pin_sub_commands() {
        let mut t = Tracer::default();
        let key_agreement = [0x80, 0x10, 0, 0, 6, 0x06, 0xA2, 1, 2, 2, 2, 0];
        assert!(
            t.observe(&apdu(&key_agreement))
                .unwrap()
                .contains("clientPIN/getKeyAgreement")
        );
        let retries = [0x80, 0x10, 0, 0, 4, 0x06, 0xA1, 2, 1, 0];
        assert!(
            t.observe(&apdu(&retries))
                .unwrap()
                .contains("clientPIN/getPINRetries")
        );
        let with_permissions = [0x80, 0x10, 0, 0, 8, 0x06, 0xA2, 1, 2, 2, 9, 3, 0xA5, 0];
        let line = t.observe(&apdu(&with_permissions)).unwrap();
        assert!(
            line.contains("getPinUvAuthTokenUsingPinWithPermissions"),
            "{line}"
        );
    }

    #[test]
    fn flags_exact_repeat_of_a_ctap_request_and_status_on_chained_reply() {
        let mut t = Tracer::default();
        let request = [0x80, 0x10, 0, 0, 3, 0x02, 0xAA, 0xBB, 0];
        t.observe(&Frame::Apdu {
            id: 31,
            generation: 1,
            data: request.to_vec(),
        });
        let first = t.observe(&response(&[0x00, 1, 2, 0x61, 0x28])).unwrap();
        assert!(first.contains("CTAP status 0x00 OK"), "{first}");
        let again = t
            .observe(&Frame::Apdu {
                id: 33,
                generation: 1,
                data: request.to_vec(),
            })
            .unwrap();
        assert!(again.contains("exact repeat of #31"), "{again}");
    }

    #[test]
    fn output_never_contains_payload_bytes() {
        let mut t = Tracer::default();
        let secret = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut data = vec![0x80, 0x10, 0, 0, 5, 0x02];
        data.extend(secret);
        let line = t.observe(&apdu(&data)).unwrap().to_ascii_uppercase();
        assert!(
            !line.contains("DEADBEEF") && !line.contains("DE AD"),
            "{line}"
        );
    }

    #[test]
    fn malformed_input_does_not_panic() {
        let mut t = Tracer::default();
        assert!(t.observe(&apdu(&[1, 2])).unwrap().contains("malformed"));
        assert!(t.observe(&response(&[0x90])).unwrap().contains("malformed"));
    }
}
