//! macOS IFD shim protocol: the wire format spoken between the PC/SC
//! IFDHandler shim (`ifd_shim.c`, loaded by com.apple.ifdreader) and the
//! tapbridge daemon over a Unix-domain socket.
//!
//! Framing: every message in both directions is `u32 BE length | body`, the
//! same shape as the phone wire (versioned there, plain here: the socket is
//! local and both ends ship together).
//!
//! Request body: `[cmd:u8][payload...]`
//! Response body: `[status:u8][data...]`
//!
//! Pure codec + error type: no I/O, fully unit-testable. The daemon side
//! (desktop `macos.rs`) maps these onto the session service exactly like the
//! Windows usbip reader maps CCID.

/// Card status / ATR poll (pcscd polls presence ~1 Hz).
pub const CMD_PRESENCE: u8 = 1;
pub const CMD_ATR: u8 = 2;
/// Power on: returns the ATR when a card is present; the daemon also fires
/// the rate-limited phone wake when no live session exists.
pub const CMD_POWER_ON: u8 = 3;
pub const CMD_POWER_OFF: u8 = 4;
/// APDU exchange: payload is the raw APDU; response data is the R-APDU.
pub const CMD_TRANSMIT: u8 = 5;

/// Statuses returned by the daemon.
pub const OK: u8 = 0;
pub const E_NO_CARD: u8 = 1;
pub const E_NO_SESSION: u8 = 2;
pub const E_BUSY: u8 = 3;
pub const E_COMM: u8 = 4;
pub const E_BAD: u8 = 5;

/// Response body sizes are bounded by the phone wire contract.
pub const MAX_BODY: usize = 4 + 65536;

/// One decoded shim request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Presence,
    Atr,
    PowerOn,
    PowerOff,
    Transmit(Vec<u8>),
}

/// Why a transmit failed; the daemon maps this onto the protocol status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransmitError {
    NoCard,
    NoSession,
    Busy,
    InvalidApdu,
    Other,
}

impl TransmitError {
    pub fn status(self) -> u8 {
        match self {
            Self::NoCard => E_NO_CARD,
            Self::NoSession => E_NO_SESSION,
            Self::Busy => E_BUSY,
            Self::InvalidApdu | Self::Other => E_COMM,
        }
    }
}

/// Encodes one request with its length prefix.
pub fn encode_request(request: &Request) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    match request {
        Request::Presence => body.push(CMD_PRESENCE),
        Request::Atr => body.push(CMD_ATR),
        Request::PowerOn => body.push(CMD_POWER_ON),
        Request::PowerOff => body.push(CMD_POWER_OFF),
        Request::Transmit(apdu) => {
            body.push(CMD_TRANSMIT);
            body.extend_from_slice(apdu);
        }
    }
    framed(&mut body)
}

/// Decodes a request body (no length prefix) of exactly `len` bytes.
pub fn decode_request(body: &[u8]) -> Option<Request> {
    let cmd = *body.first()?;
    match cmd {
        CMD_PRESENCE if body.len() == 1 => Some(Request::Presence),
        CMD_ATR if body.len() == 1 => Some(Request::Atr),
        CMD_POWER_ON if body.len() == 1 => Some(Request::PowerOn),
        CMD_POWER_OFF if body.len() == 1 => Some(Request::PowerOff),
        CMD_TRANSMIT => Some(Request::Transmit(body[1..].to_vec())),
        _ => None,
    }
}

/// Encodes one response with its length prefix.
pub fn encode_response(status: u8, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(data.len() + 5);
    body.push(status);
    body.extend_from_slice(data);
    framed(&mut body)
}

/// Decodes a response body (no length prefix).
pub fn decode_response(body: &[u8]) -> Option<(u8, Vec<u8>)> {
    let status = *body.first()?;
    Some((status, body[1..].to_vec()))
}

fn framed(body: &mut Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.append(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        for request in [
            Request::Presence,
            Request::Atr,
            Request::PowerOn,
            Request::PowerOff,
            Request::Transmit(vec![0x00, 0xA4, 0x00, 0x00, 0x02, 0x3F, 0x00]),
        ] {
            let encoded = encode_request(&request);
            assert_eq!(
                u32::from_be_bytes(encoded[..4].try_into().unwrap()) as usize,
                encoded.len() - 4
            );
            assert_eq!(decode_request(&encoded[4..]), Some(request));
        }
    }

    #[test]
    fn transmit_apdu_travels_unmodified() {
        let apdu = vec![0x00; 65536];
        let encoded = encode_request(&Request::Transmit(apdu.clone()));
        assert_eq!(decode_request(&encoded[4..]), Some(Request::Transmit(apdu)));
    }

    #[test]
    fn response_round_trips_with_status_and_data() {
        let encoded = encode_response(E_NO_CARD, b"");
        assert_eq!(decode_response(&encoded[4..]), Some((E_NO_CARD, vec![])));
        let rapdu = vec![0x90, 0x00];
        let encoded = encode_response(OK, &rapdu);
        assert_eq!(decode_response(&encoded[4..]), Some((OK, rapdu)));
    }

    #[test]
    fn unknown_or_malformed_commands_are_rejected() {
        assert_eq!(decode_request(&[]), None);
        assert_eq!(decode_request(&[0x99]), None);
        // Extra payload on a payload-less command is malformed.
        assert_eq!(decode_request(&[CMD_PRESENCE, 1]), None);
        assert_eq!(decode_response(&[]), None);
    }

    #[test]
    fn transmit_error_statuses_match_the_c_shim() {
        assert_eq!(TransmitError::NoCard.status(), E_NO_CARD);
        assert_eq!(TransmitError::NoSession.status(), E_NO_SESSION);
        assert_eq!(TransmitError::Busy.status(), E_BUSY);
        assert_eq!(TransmitError::InvalidApdu.status(), E_COMM);
    }
}
