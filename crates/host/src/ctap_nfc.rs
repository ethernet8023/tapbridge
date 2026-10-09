//! CTAP2 over ISO 7816 (NFC), host side: turns one CTAP request
//! (command byte || CBOR) into the APDUs a FIDO2 card expects and collects
//! its reply (CTAP status byte || CBOR). Pure: the APDU transport is a
//! closure, so the framing is tested without a card.
//!
//! CTAP 2.2 §11.3: SELECT `A0000006472F0001`, then `NFCCTAP_MSG`
//! (`80 10 00 00`) carrying the request. Requests over 255 bytes use short
//! APDU chaining (CLA `90` for every fragment but the last, which uses
//! `80`); replies over 256 bytes arrive as `61xx` and are fetched with
//! `GET RESPONSE`. Short APDUs only: every NFC card and phone handles them.

/// FIDO applet AID (CTAP 2.2 §11.3.1).
pub const FIDO_AID: [u8; 8] = [0xA0, 0x00, 0x00, 0x06, 0x47, 0x2F, 0x00, 0x01];

const MAX_SHORT: usize = 255;
/// Bound on `GET RESPONSE` round trips (64 KiB / 256).
const MAX_GET_RESPONSE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError<E> {
    /// The APDU transport failed (card gone, session dropped, timeout).
    Transport(E),
    /// The card has no FIDO applet, or refused a step: the status word.
    Card([u8; 2]),
    /// The request was empty.
    Empty,
}

/// SELECT the FIDO applet.
pub fn select_apdu() -> Vec<u8> {
    let mut apdu = vec![0x00, 0xA4, 0x04, 0x00, FIDO_AID.len() as u8];
    apdu.extend_from_slice(&FIDO_AID);
    apdu.push(0x00);
    apdu
}

/// The `NFCCTAP_MSG` APDUs carrying `request`, chained when over 255 bytes.
pub fn msg_apdus(request: &[u8]) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> = request.chunks(MAX_SHORT).collect();
    let last = chunks.len().saturating_sub(1);
    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let cla = if i == last { 0x80 } else { 0x90 };
            let mut apdu = vec![cla, 0x10, 0x00, 0x00, chunk.len() as u8];
            apdu.extend_from_slice(chunk);
            apdu.push(0x00);
            apdu
        })
        .collect()
}

/// Relays one CTAP request to the card. `transmit` sends one APDU and
/// returns the raw response (data || SW1 SW2).
pub fn relay<E>(
    request: &[u8],
    mut transmit: impl FnMut(Vec<u8>) -> Result<Vec<u8>, E>,
) -> Result<Vec<u8>, RelayError<E>> {
    if request.is_empty() {
        return Err(RelayError::Empty);
    }
    let (_, sw) = split(transmit(select_apdu()).map_err(RelayError::Transport)?);
    if sw != [0x90, 0x00] {
        return Err(RelayError::Card(sw));
    }

    let apdus = msg_apdus(request);
    let last = apdus.len() - 1;
    let mut reply = Vec::new();
    for (i, apdu) in apdus.into_iter().enumerate() {
        let (data, sw) = split(transmit(apdu).map_err(RelayError::Transport)?);
        if i < last {
            if sw != [0x90, 0x00] {
                return Err(RelayError::Card(sw));
            }
            continue;
        }
        reply = data;
        let mut sw = sw;
        let mut fetches = 0;
        while sw[0] == 0x61 {
            fetches += 1;
            if fetches > MAX_GET_RESPONSE {
                return Err(RelayError::Card(sw));
            }
            let (more, next) = split(
                transmit(vec![0x00, 0xC0, 0x00, 0x00, sw[1]]).map_err(RelayError::Transport)?,
            );
            reply.extend_from_slice(&more);
            sw = next;
        }
        if sw != [0x90, 0x00] {
            return Err(RelayError::Card(sw));
        }
    }
    if reply.is_empty() {
        // A CTAP reply always carries at least the status byte.
        return Err(RelayError::Card([0x6F, 0x00]));
    }
    Ok(reply)
}

fn split(mut response: Vec<u8>) -> (Vec<u8>, [u8; 2]) {
    if response.len() < 2 {
        return (Vec::new(), [0x6F, 0x00]);
    }
    let sw2 = response.pop().unwrap_or(0);
    let sw1 = response.pop().unwrap_or(0);
    (response, [sw1, sw2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted card: checks each APDU against the expected one, replies.
    struct Script(VecDeque<(Vec<u8>, Vec<u8>)>);

    impl Script {
        fn new(steps: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
            Self(steps.into())
        }
        fn transmit(&mut self, apdu: Vec<u8>) -> Result<Vec<u8>, &'static str> {
            let (expected, reply) = self.0.pop_front().expect("unexpected extra APDU");
            assert_eq!(apdu, expected, "APDU mismatch");
            Ok(reply)
        }
    }

    fn select_ok() -> (Vec<u8>, Vec<u8>) {
        let mut version = b"FIDO_2_0".to_vec();
        version.extend_from_slice(&[0x90, 0x00]);
        (select_apdu(), version)
    }

    #[test]
    fn select_uses_the_ctap_nfc_aid() {
        assert_eq!(
            select_apdu(),
            [
                0x00, 0xA4, 0x04, 0x00, 0x08, 0xA0, 0x00, 0x00, 0x06, 0x47, 0x2F, 0x00, 0x01, 0x00
            ]
        );
    }

    #[test]
    fn get_info_is_one_short_apdu() {
        let mut card = Script::new(vec![
            select_ok(),
            (
                vec![0x80, 0x10, 0x00, 0x00, 0x01, 0x04, 0x00],
                vec![0x00, 0xA0, 0x90, 0x00],
            ),
        ]);
        let reply = relay(&[0x04], |apdu| card.transmit(apdu)).unwrap();
        assert_eq!(reply, [0x00, 0xA0]);
        assert!(card.0.is_empty());
    }

    #[test]
    fn long_requests_chain_with_cla_90_and_end_with_80() {
        let request: Vec<u8> = (0..600u16).map(|i| i as u8).collect();
        let apdus = msg_apdus(&request);
        assert_eq!(apdus.len(), 3);
        assert_eq!(&apdus[0][..5], &[0x90, 0x10, 0x00, 0x00, 0xFF]);
        assert_eq!(&apdus[1][..5], &[0x90, 0x10, 0x00, 0x00, 0xFF]);
        assert_eq!(&apdus[2][..5], &[0x80, 0x10, 0x00, 0x00, 90]);
        let joined: Vec<u8> = apdus
            .iter()
            .flat_map(|a| a[5..a.len() - 1].to_vec())
            .collect();
        assert_eq!(joined, request);

        let mut steps = vec![select_ok()];
        steps.push((apdus[0].clone(), vec![0x90, 0x00]));
        steps.push((apdus[1].clone(), vec![0x90, 0x00]));
        steps.push((apdus[2].clone(), vec![0x00, 0x90, 0x00]));
        let mut card = Script::new(steps);
        assert_eq!(relay(&request, |a| card.transmit(a)).unwrap(), [0x00]);
    }

    #[test]
    fn long_replies_are_fetched_with_get_response() {
        let mut first = vec![0x00; 256];
        first.extend_from_slice(&[0x61, 0x00]);
        let mut second = vec![0x11; 256];
        second.extend_from_slice(&[0x61, 0x10]);
        let mut third = vec![0x22; 16];
        third.extend_from_slice(&[0x90, 0x00]);
        let mut card = Script::new(vec![
            select_ok(),
            (msg_apdus(&[0x01, 0xA0])[0].clone(), first),
            (vec![0x00, 0xC0, 0x00, 0x00, 0x00], second),
            (vec![0x00, 0xC0, 0x00, 0x00, 0x10], third),
        ]);
        let reply = relay(&[0x01, 0xA0], |a| card.transmit(a)).unwrap();
        assert_eq!(reply.len(), 256 + 256 + 16);
        assert_eq!(reply[300], 0x11);
        assert_eq!(reply[520], 0x22);
    }

    #[test]
    fn a_card_without_the_fido_applet_is_reported() {
        let mut card = Script::new(vec![(select_apdu(), vec![0x6A, 0x82])]);
        assert_eq!(
            relay(&[0x04], |a| card.transmit(a)),
            Err(RelayError::Card([0x6A, 0x82]))
        );
    }

    #[test]
    fn transport_failures_surface() {
        assert_eq!(
            relay(&[0x04], |_| Err::<Vec<u8>, _>("gone")),
            Err(RelayError::Transport("gone"))
        );
    }

    #[test]
    fn a_ctap_error_reply_is_passed_through_not_treated_as_failure() {
        // CTAP2_ERR_NO_CREDENTIALS (0x2E) is a normal answer for the browser.
        let mut card = Script::new(vec![
            select_ok(),
            (msg_apdus(&[0x02, 0xA0])[0].clone(), vec![0x2E, 0x90, 0x00]),
        ]);
        assert_eq!(relay(&[0x02, 0xA0], |a| card.transmit(a)).unwrap(), [0x2E]);
    }

    #[test]
    fn empty_requests_are_refused() {
        assert_eq!(relay(&[], |_| Ok::<_, ()>(vec![])), Err(RelayError::Empty));
    }
}
