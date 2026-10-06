//! CTAP2 over ISO-7816 (NFC) framing: the card-APDU relay codec for the
//! hybrid authenticator.
//!
//! CTAP 2.2 §11.3: the FIDO2 applet is selected with AID `A0000006.472F0002`
//! (`00 A4 04 00 <AID> Le`), replies with a version string (`FIDO_2_0` for
//! CTAP2-only, `U2F_V2` if it also speaks CTAP1). CTAP messages ride in
//! `NFCCTAP_MSG` (INS `0x10`) APDUs: `80 10 00 <P1> 00 <CTAP-byte||CBOR> Le`
//! with P1 bit 0x80 marking NFCCTAP_GETRESPONSE support; the card answers
//! `9000` (data = CTAP status || response), `9100` (status update, fetch
//! with `NFCCTAP_GETRESPONSE` INS `0x11`), or an ISO error word.
//! `NFCCTAP_CONTROL` (`80 12 01 00`) ends the CTAP transaction.
//!
//! Short APDU chaining uses CLA 0x90 for continuation fragments. Extended
//! length APDUs hold up to 65535 bytes of command data.
//!
//! The phone relays tunnel CTAP messages to the card through this framing,
//! byte-for-byte; it never interprets CTAP content beyond the framing layer.

/// CLA for NFCCTAP commands.
pub const CLA: u8 = 0x80;
/// INS: NFCCTAP_MSG — carry a CTAP message.
pub const INS_MSG: u8 = 0x10;
/// INS: NFCCTAP_GETRESPONSE — fetch status updates / a pending reply.
pub const INS_GETRESPONSE: u8 = 0x11;
/// INS: NFCCTAP_CONTROL — end the CTAP transaction (deselect).
pub const INS_CONTROL: u8 = 0x12;
/// P1 for NFCCTAP_CONTROL: end CTAP_MSG.
pub const P1_CONTROL_END: u8 = 0x01;
/// P1 bit for NFCCTAP_MSG: client supports NFCCTAP_GETRESPONSE.
pub const P1_GETRESPONSE_SUPPORT: u8 = 0x80;

/// CLA used for short-APDU chaining fragments (§11.3.6).
pub const CHAINING_CLA: u8 = 0x90;

/// The FIDO2 applet AID for CTAP2 over NFC (RID A000000647, PIX 2F0002).
pub const FIDO2_NFC_AID: [u8; 8] = [0xA0, 0x00, 0x00, 0x06, 0x47, 0x2F, 0x00, 0x02];

/// Version string the card returns on SELECT for a CTAP2-only authenticator.
pub const VERSION_FIDO_2_0: &[u8] = b"FIDO_2_0";
/// Version string for CTAP1/U2F (also used by dual-protocol cards).
pub const VERSION_U2F_V2: &[u8] = b"U2F_V2";

/// Maximum command data bytes in an extended-length APDU (§11.3.6).
pub const MAX_EXTENDED_DATA: usize = 65535;
/// Maximum data bytes in a short APDU.
pub const MAX_SHORT_DATA: usize = 255;

/// Status word: success.
pub const SW_SUCCESS: [u8; 2] = [0x90, 0x00];
/// Status word: status update (fetch with NFCCTAP_GETRESPONSE).
pub const SW_STATUS_UPDATE: [u8; 2] = [0x91, 0x00];

/// Errors from the relay codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtapError {
    /// CTAP payload too large for extended-length APDU framing.
    TooLarge,
    /// P1 had RFU bits set (must be 0 or 0x80).
    BadP1,
    /// P2 non-zero (RFU).
    BadP2,
    /// Response shorter than a status word.
    ShortResponse,
}

impl std::fmt::Display for CtapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::TooLarge => "CTAP payload exceeds extended APDU capacity",
            Self::BadP1 => "NFCCTAP_MSG P1 must be 0x00 or 0x80",
            Self::BadP2 => "NFCCTAP_MSG P2 must be 0x00",
            Self::ShortResponse => "card response shorter than two bytes",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for CtapError {}

/// Build the SELECT APDU for the FIDO2 NFC applet (`00 A4 04 00 AID Le`).
pub fn select_aid_apdu() -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + FIDO2_NFC_AID.len() + 1);
    out.extend_from_slice(&[0x00, 0xA4, 0x04, 0x00]);
    out.push(FIDO2_NFC_AID.len() as u8);
    out.extend_from_slice(&FIDO2_NFC_AID);
    out.push(0x00); // Le
    out
}

/// Build the NFCCTAP_CONTROL "end CTAP_MSG" APDU (`80 12 01 00`).
pub fn control_end_apdu() -> [u8; 4] {
    [CLA, INS_CONTROL, P1_CONTROL_END, 0x00]
}

/// Build an NFCCTAP_MSG APDU (extended length) carrying a CTAP2 message
/// (command byte || CBOR, already assembled).
///
/// `getresponse_support` sets P1 bit 0x80 so the card may reply `9100` for
/// status updates instead of blocking.
pub fn msg_apdu(ctap_message: &[u8], getresponse_support: bool) -> Result<Vec<u8>, CtapError> {
    if ctap_message.len() > MAX_EXTENDED_DATA {
        return Err(CtapError::TooLarge);
    }
    let p1 = if getresponse_support {
        P1_GETRESPONSE_SUPPORT
    } else {
        0x00
    };
    let mut out = Vec::with_capacity(7 + ctap_message.len() + 3);
    out.extend_from_slice(&[CLA, INS_MSG, p1, 0x00]);
    // Extended length: Lc = 0x00 0xHH 0xLL (three bytes, first zero)
    out.push(0x00);
    out.extend_from_slice(&(ctap_message.len() as u16).to_be_bytes());
    out.extend_from_slice(ctap_message);
    // Le extended: 0x00 0xHH 0xLL for up to 65536; use 65536 -> 0x00 0x00 0x00
    out.extend_from_slice(&[0x00, 0x00, 0x00]);
    Ok(out)
}

/// Split a CTAP2 message into short-APDU chaining fragments (CLA 0x90).
/// First fragment uses `80 10 00 <P1>`; continuations use `90 10 00 00`.
/// Each fragment is <= 255 bytes of data. Returns an error if the message
/// needs extended length (only used when the card lacks extended support —
/// the relay prefers [`msg_apdu`]).
pub fn msg_apdu_short_chained(
    ctap_message: &[u8],
    getresponse_support: bool,
    max_fragment: usize,
) -> Result<Vec<Vec<u8>>, CtapError> {
    if ctap_message.is_empty() {
        return Err(CtapError::TooLarge);
    }
    if !(1..=MAX_SHORT_DATA).contains(&max_fragment) {
        return Err(CtapError::TooLarge);
    }
    let p1 = if getresponse_support {
        P1_GETRESPONSE_SUPPORT
    } else {
        0x00
    };
    let mut fragments = Vec::new();
    let mut rest = ctap_message;
    let mut first = true;
    while !rest.is_empty() {
        let (chunk, tail) = rest.split_at(rest.len().min(max_fragment));
        let mut apdu = Vec::with_capacity(5 + chunk.len());
        let cla = if first { CLA } else { CHAINING_CLA };
        let frag_p1 = if first { p1 } else { 0x00 };
        apdu.extend_from_slice(&[cla, INS_MSG, frag_p1, 0x00]);
        apdu.push(chunk.len() as u8);
        apdu.extend_from_slice(chunk);
        apdu.push(0x00); // Le
        fragments.push(apdu);
        rest = tail;
        first = false;
    }
    Ok(fragments)
}

/// Build the NFCCTAP_GETRESPONSE APDU (`80 11 00 00 00`).
pub fn getresponse_apdu() -> Vec<u8> {
    vec![CLA, INS_GETRESPONSE, 0x00, 0x00, 0x00]
}

/// A parsed card response: data plus the two-byte status word.
#[derive(Clone, PartialEq, Eq)]
pub struct CardResponse {
    /// Response data before the status word (CTAP status byte || response
    /// CBOR on 9000; status update payload on 9100).
    pub data: Vec<u8>,
    /// SW1/SW2.
    pub status_word: [u8; 2],
}

impl CardResponse {
    /// Parse raw card bytes (data || SW1 || SW2). Strict: at least 2 bytes.
    pub fn parse(raw: &[u8]) -> Result<Self, CtapError> {
        if raw.len() < 2 {
            return Err(CtapError::ShortResponse);
        }
        Ok(Self {
            data: raw[..raw.len() - 2].to_vec(),
            status_word: [raw[raw.len() - 2], raw[raw.len() - 1]],
        })
    }

    /// True when the response is `9000` (success).
    pub fn is_success(&self) -> bool {
        self.status_word == SW_SUCCESS
    }

    /// True when the response is `9100`: a status update; the transport
    /// layer must issue NFCCTAP_GETRESPONSE (unless cancelled).
    pub fn is_status_update(&self) -> bool {
        self.status_word == SW_STATUS_UPDATE
    }

    /// True when the card is waiting for more chaining fragments (SW1 0x61
    /// or 0x6C per ISO 7816-4 semantics used by some cards).
    pub fn is_more_time_or_chaining(&self) -> bool {
        matches!(self.status_word[0], 0x61 | 0x6C)
    }

    /// The CTAP2 layer's view: on 9000, data is `CTAP status byte || CBOR`.
    /// Returns (ctap_status_byte, response_bytes) or None on non-success.
    pub fn ctap_reply(&self) -> Option<(u8, &[u8])> {
        if !self.is_success() || self.data.is_empty() {
            return None;
        }
        Some((self.data[0], &self.data[1..]))
    }
}

impl std::fmt::Debug for CardResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Response bytes can contain user-identifying CTAP data (username,
        // display name, credential ids). Redact data; status word is safe.
        f.debug_struct("CardResponse")
            .field("data", &"REDACTED")
            .field(
                "status_word",
                &format_args!("{:#04x}", u16::from_be_bytes(self.status_word)),
            )
            .finish()
    }
}

/// Validate an incoming NFCCTAP_MSG APDU (strict decode before relay).
pub fn validate_msg_apdu(apdu: &[u8]) -> Result<(), CtapError> {
    if apdu.len() < 4 {
        return Err(CtapError::ShortResponse);
    }
    if apdu[0] != CLA && apdu[0] != CHAINING_CLA {
        return Err(CtapError::BadP1);
    }
    if apdu[1] != INS_MSG {
        return Err(CtapError::BadP1);
    }
    let p1 = apdu[2] & 0x7F; // RFU bits must be zero
    if p1 != 0 {
        return Err(CtapError::BadP1);
    }
    if apdu[3] != 0 {
        return Err(CtapError::BadP2);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_aid_layout() {
        let apdu = select_aid_apdu();
        assert_eq!(
            apdu,
            vec![
                0x00, 0xA4, 0x04, 0x00, 0x08, 0xA0, 0x00, 0x00, 0x06, 0x47, 0x2F, 0x00, 0x02, 0x00
            ]
        );
    }

    #[test]
    fn control_end_layout() {
        assert_eq!(control_end_apdu(), [0x80, 0x12, 0x01, 0x00]);
    }

    #[test]
    fn msg_apdu_extended_layout() {
        // authenticatorGetInfo is just the command byte 0x04
        let apdu = msg_apdu(&[0x04], true).unwrap();
        assert_eq!(
            apdu,
            vec![
                0x80, 0x10, 0x80, 0x00, // CLA INS P1 P2
                0x00, 0x00, 0x01, // extended Lc = 1
                0x04, // CTAP command byte
                0x00, 0x00, 0x00 // extended Le = 65536
            ]
        );
        let apdu = msg_apdu(&[0x02, 0xA3], false).unwrap();
        assert_eq!(apdu[2], 0x00, "P1 without getresponse support");
        assert_eq!(&apdu[4..7], &[0x00, 0x00, 0x02], "extended Lc");
        assert_eq!(&apdu[7..9], &[0x02, 0xA3]);
    }

    #[test]
    fn msg_apdu_size_bound() {
        assert!(msg_apdu(&[0u8; 65535], true).is_ok());
        assert_eq!(
            msg_apdu(&[0u8; 65536], true).unwrap_err(),
            CtapError::TooLarge
        );
    }

    #[test]
    fn msg_apdu_short_chaining_layout() {
        let msg = vec![0xABu8; 300];
        let fragments = msg_apdu_short_chained(&msg, true, 255).unwrap();
        assert_eq!(fragments.len(), 2);
        // first fragment: 80 10 80 00 FF <255 bytes> 00
        assert_eq!(fragments[0][..4], [0x80, 0x10, 0x80, 0x00]);
        assert_eq!(fragments[0][4], 0xFF);
        assert_eq!(fragments[0].len(), 5 + 255 + 1);
        // second: 90 10 00 00 2D <45 bytes> 00
        assert_eq!(fragments[1][..4], [0x90, 0x10, 0x00, 0x00]);
        assert_eq!(fragments[1][4], 45);
        let mut reassembled = Vec::new();
        for f in &fragments {
            let len = f[4] as usize;
            reassembled.extend_from_slice(&f[5..5 + len]);
        }
        assert_eq!(reassembled, msg);
    }

    #[test]
    fn msg_apdu_short_chaining_bounds() {
        assert!(msg_apdu_short_chained(&[1], true, 0).is_err());
        assert!(msg_apdu_short_chained(&[1], true, 256).is_err());
        assert!(msg_apdu_short_chained(&[], true, 255).is_err());
        assert_eq!(msg_apdu_short_chained(&[1], true, 255).unwrap().len(), 1);
    }

    #[test]
    fn getresponse_layout() {
        assert_eq!(getresponse_apdu(), vec![0x80, 0x11, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn card_response_parse() {
        // success with data: CTAP status 0x00 (ok) + CBOR
        let raw = [0x00, 0xA1, 0x01, 0x02, 0x90, 0x00];
        let r = CardResponse::parse(&raw).unwrap();
        assert!(r.is_success());
        assert!(!r.is_status_update());
        assert_eq!(r.ctap_reply(), Some((0x00, &raw[1..4])));
        // status update 9100
        let raw = [0x08, 0x91, 0x00];
        let r = CardResponse::parse(&raw).unwrap();
        assert!(r.is_status_update());
        assert!(!r.is_success());
        assert_eq!(r.ctap_reply(), None);
        // ISO error
        let raw = [0x6A, 0x82];
        let r = CardResponse::parse(&raw).unwrap();
        assert!(!r.is_success());
        assert_eq!(r.status_word, [0x6A, 0x82]);
    }

    #[test]
    fn card_response_strict() {
        assert_eq!(
            CardResponse::parse(&[0x90]).unwrap_err(),
            CtapError::ShortResponse
        );
        assert_eq!(
            CardResponse::parse(&[]).unwrap_err(),
            CtapError::ShortResponse
        );
    }

    #[test]
    fn card_response_debug_redacts() {
        let r = CardResponse::parse(&[0xDE, 0xAD, 0x90, 0x00]).unwrap();
        let s = format!("{r:?}");
        assert!(s.contains("REDACTED"));
        assert!(!s.contains("dead"));
        assert!(s.contains("0x9000"));
    }

    #[test]
    fn validate_msg_apdu_strict() {
        assert!(validate_msg_apdu(&[0x80, 0x10, 0x00, 0x00, 0x00]).is_ok());
        assert!(validate_msg_apdu(&[0x80, 0x10, 0x80, 0x00, 0x00]).is_ok());
        assert!(validate_msg_apdu(&[0x90, 0x10, 0x00, 0x00, 0x00]).is_ok());
        // RFU bits in P1
        assert_eq!(
            validate_msg_apdu(&[0x80, 0x10, 0x7F, 0x00, 0x00]).unwrap_err(),
            CtapError::BadP1
        );
        // non-zero P2
        assert_eq!(
            validate_msg_apdu(&[0x80, 0x10, 0x00, 0x01, 0x00]).unwrap_err(),
            CtapError::BadP2
        );
        // wrong INS
        assert!(validate_msg_apdu(&[0x80, 0x11, 0x00, 0x00, 0x00]).is_err());
        // wrong CLA
        assert!(validate_msg_apdu(&[0x00, 0x10, 0x00, 0x00, 0x00]).is_err());
    }
}
