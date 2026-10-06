//! Tunnel server domain decoding and WebSocket URL construction.
//!
//! CTAP 2.2 §11.5.1: the advert carries a u16 encoded tunnel-server domain.
//! Values 0..255 index the assigned-domain table (shared via QR key 2);
//! values >= 256 hash into a `cable.<base32>.tld` domain. The client connects
//! with `wss://<domain>/cable/connect/<routing-id-hex>/<tunnel-id-hex>`,
//! subprotocol `fido.cable`. State-assisted contact uses
//! `/cable/contact/<base64url(contact-id)>` with an `X-caBLE-Client-Payload`
//! header.

use crate::keys::tunnel_id_from_qr_secret;
use sha2::{Digest, Sha256};

/// WebSocket subprotocol for caBLEv2 tunnels.
pub const SUBPROTOCOL: &str = "fido.cable";

/// Assigned tunnel-server domains known to this implementation, in order
/// (mirrors the spec's example table; the authoritative count comes from QR
/// key 2 at runtime — a domain index >= the QR's count must use the hashed
/// fallback for compatibility with older clients).
pub const ASSIGNED_DOMAINS: [&str; 2] = ["cable.ua5v.com", "cable.auth.com"];

/// The literal SHA-256 domain string from the spec.
const DOMAIN_HASH_PREFIX: &[u8] = b"caBLEv2 tunnel server domain";

/// Errors from tunnel-server domain decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelServerDomainError {
    /// Encoded value 0..255 but not within the assigned table this
    /// implementation knows (or the QR advertised).
    UnassignedIndex(u16),
}

impl std::fmt::Display for TunnelServerDomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnassignedIndex(i) => write!(f, "tunnel domain index {i} not assigned"),
        }
    }
}

impl std::error::Error for TunnelServerDomainError {}

/// Decode a u16 tunnel-server domain identifier.
///
/// `assigned_count` is the number of assigned domains the client knows
/// (QR key 2); indices >= `assigned_count` MUST NOT be read from the local
/// table (an older client wouldn't recognise them) and instead use the
/// hashed fallback. Pass `Some(ASSIGNED_DOMAINS.len())`-bounded values from
/// the QR; for a caller with no QR context pass
/// `Some(ASSIGNED_DOMAINS.len() as u16)`.
pub fn decode_tunnel_server_domain(
    encoded: u16,
    assigned_count: u16,
) -> Result<String, TunnelServerDomainError> {
    if encoded >= 256 {
        return Ok(hashed_domain(encoded));
    }
    if encoded >= assigned_count || encoded as usize >= ASSIGNED_DOMAINS.len() {
        // Fall back to the hashed form: the local table may be behind, and
        // hashed domains are always decodable.
        return Ok(hashed_domain(encoded));
    }
    Ok(ASSIGNED_DOMAINS[encoded as usize].to_string())
}

/// The hashed fallback domain for values >= 256 (and unassigned indices):
/// SHA-256("caBLEv2 tunnel server domain" || encoded_le16 || 0x00), first 8
/// bytes little-endian -> base32 (low 5 bits at a time) under "cable.",
/// TLD indexed by bits 0..1.
fn hashed_domain(encoded: u16) -> String {
    let mut input = DOMAIN_HASH_PREFIX.to_vec();
    input.extend_from_slice(&encoded.to_le_bytes());
    input.push(0);
    let digest = Sha256::digest(&input);
    let mut v = u64::from_le_bytes(digest[..8].try_into().unwrap());
    let tld_index = (v & 3) as usize;
    v >>= 2;
    const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::from("cable.");
    while v != 0 {
        out.push(BASE32[(v & 31) as usize] as char);
        v >>= 5;
    }
    const TLDS: [&str; 4] = [".com", ".org", ".net", ".info"];
    out.push_str(TLDS[tld_index & 3]);
    out
}

/// A fully-resolved tunnel endpoint for a QR-initiated transaction.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TunnelConnect {
    /// Complete `wss://` URL: domain + `/cable/connect/` + routing id hex +
    /// `/` + tunnel id hex.
    pub url: String,
    /// The WebSocket subprotocol to request: always `fido.cable`.
    pub subprotocol: &'static str,
}

impl TunnelConnect {
    /// Build the QR-initiated connect URL from the decrypted advert + QR
    /// secret. `qr_assigned_count` is QR key 2.
    pub fn qr_initiated(
        advert: &crate::advert::AdvertPlaintext,
        qr_secret: &[u8; 16],
        qr_assigned_count: u16,
    ) -> Result<Self, TunnelServerDomainError> {
        let domain = decode_tunnel_server_domain(advert.tunnel_server_domain, qr_assigned_count)?;
        let tunnel_id = tunnel_id_from_qr_secret(qr_secret);
        Ok(Self {
            url: format!(
                "wss://{domain}/cable/connect/{}/{}",
                hex(&advert.routing_id),
                hex(&tunnel_id)
            ),
            subprotocol: SUBPROTOCOL,
        })
    }

    /// Build the state-assisted contact URL from a stored contact ID.
    pub fn state_assisted_contact(domain: &str, contact_id: &[u8]) -> String {
        format!(
            "wss://{domain}/cable/contact/{}",
            base64url_nopad(contact_id)
        )
    }
}

/// Minimal base64url (RFC 4648 §5, no padding).
pub fn base64url_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() >= 2 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advert::AdvertPlaintext;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // Independent python/openssl vectors.
    #[test]
    fn hashed_domains_match_vectors() {
        assert_eq!(
            decode_tunnel_server_domain(256, 2).unwrap(),
            "cable.qz2ekwmnd332c.info"
        );
        assert_eq!(
            decode_tunnel_server_domain(257, 2).unwrap(),
            "cable.4a6bwmj6hiyyd.net"
        );
        assert_eq!(
            decode_tunnel_server_domain(300, 2).unwrap(),
            "cable.mv2gnuwgd7fwb.net"
        );
    }

    #[test]
    fn assigned_domains_direct() {
        assert_eq!(decode_tunnel_server_domain(0, 2).unwrap(), "cable.ua5v.com");
        assert_eq!(decode_tunnel_server_domain(1, 2).unwrap(), "cable.auth.com");
    }

    #[test]
    fn unassigned_index_falls_back_to_hashed() {
        // index 5 with assigned_count 2: not in the table; hashed fallback.
        assert_eq!(decode_tunnel_server_domain(5, 2).unwrap(), hashed_domain(5));
        assert!(
            decode_tunnel_server_domain(5, 2)
                .unwrap()
                .starts_with("cable.")
        );
    }

    #[test]
    fn qr_connect_url_matches_derived_ids() {
        let advert = AdvertPlaintext::new([1; 10], [0xaa, 0xbb, 0xcc], 0);
        let secret: [u8; 16] = unhex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let connect = TunnelConnect::qr_initiated(&advert, &secret, 2).unwrap();
        assert_eq!(connect.subprotocol, "fido.cable");
        assert_eq!(
            connect.url,
            "wss://cable.ua5v.com/cable/connect/aabbcc/de1d8150b45f03349b2b00a52e2e893e"
        );
    }

    #[test]
    fn qr_connect_url_uses_tunnel_domain_from_advert() {
        let advert = AdvertPlaintext::new([1; 10], [0xaa, 0xbb, 0xcc], 256);
        let secret: [u8; 16] = unhex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let connect = TunnelConnect::qr_initiated(&advert, &secret, 2).unwrap();
        assert_eq!(
            connect.url,
            "wss://cable.qz2ekwmnd332c.info/cable/connect/aabbcc/de1d8150b45f03349b2b00a52e2e893e"
        );
    }

    #[test]
    fn state_assisted_contact_url() {
        let url = TunnelConnect::state_assisted_contact("cable.ua5v.com", &[0xF0, 0x0F, 0x42]);
        assert_eq!(url, "wss://cable.ua5v.com/cable/contact/8A9C");
    }

    #[test]
    fn base64url_known_vectors() {
        assert_eq!(base64url_nopad(&[]), "");
        assert_eq!(base64url_nopad(b"f"), "Zg");
        assert_eq!(base64url_nopad(b"fo"), "Zm8");
        assert_eq!(base64url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64url_nopad(b"foob"), "Zm9vYg");
        assert_eq!(base64url_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url_nopad(b"foobar"), "Zm9vYmFy");
        // RFC 4648 §5 vectors (same, minus padding; '-'/'_' only for 62/63):
        assert_eq!(base64url_nopad(&[0xFB, 0xFF]), "-_8");
    }
}
