//! Wake hint: where the phone should dial after an FCM wake.
//!
//! The hint travels through the relay and FCM without authentication. It only
//! tells the phone which addresses to try first; the iroh handshake still
//! authenticates the pinned host identity. Text form:
//! `HOSTID[;r=RELAYURL][;d=IP:PORT]*`.

use std::{fmt, net::SocketAddr, str::FromStr};

use iroh::{EndpointAddr, EndpointId, RelayUrl};

/// Maximum encoded hint length in bytes.
pub const MAX_HINT: usize = 1024;
/// Maximum direct addresses carried in one hint.
pub const MAX_DIRECT: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub host: EndpointId,
    pub relay: Option<RelayUrl>,
    pub direct: Vec<SocketAddr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidHint;

impl fmt::Display for InvalidHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid wake hint")
    }
}

impl std::error::Error for InvalidHint {}

impl Hint {
    /// Hint for `addr`, keeping the first relay and at most [`MAX_DIRECT`]
    /// direct addresses, dropping addresses that would exceed [`MAX_HINT`].
    pub fn from_endpoint_addr(addr: &EndpointAddr) -> Self {
        let mut hint = Self {
            host: addr.id,
            relay: addr.relay_urls().next().cloned(),
            direct: Vec::new(),
        };
        for direct in addr.ip_addrs().take(MAX_DIRECT) {
            hint.direct.push(*direct);
            if hint.to_string().len() > MAX_HINT {
                hint.direct.pop();
                break;
            }
        }
        hint
    }

    /// The iroh address to dial.
    pub fn endpoint_addr(&self) -> EndpointAddr {
        let mut addr = EndpointAddr::new(self.host);
        if let Some(relay) = &self.relay {
            addr = addr.with_relay_url(relay.clone());
        }
        for direct in &self.direct {
            addr = addr.with_ip_addr(*direct);
        }
        addr
    }
}

impl fmt::Display for Hint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.host)?;
        if let Some(relay) = &self.relay {
            write!(f, ";r={relay}")?;
        }
        for direct in &self.direct {
            write!(f, ";d={direct}")?;
        }
        Ok(())
    }
}

impl FromStr for Hint {
    type Err = InvalidHint;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() > MAX_HINT {
            return Err(InvalidHint);
        }
        let mut parts = text.split(';');
        let host = parts
            .next()
            .ok_or(InvalidHint)?
            .parse()
            .map_err(|_| InvalidHint)?;
        let mut hint = Self {
            host,
            relay: None,
            direct: Vec::new(),
        };
        for part in parts {
            match part.split_once('=') {
                Some(("r", url)) if hint.relay.is_none() => {
                    hint.relay = Some(url.parse().map_err(|_| InvalidHint)?);
                }
                Some(("d", addr)) if hint.direct.len() < MAX_DIRECT => {
                    hint.direct.push(addr.parse().map_err(|_| InvalidHint)?);
                }
                _ => return Err(InvalidHint),
            }
        }
        Ok(hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn host() -> EndpointId {
        SecretKey::generate().public()
    }

    #[test]
    fn round_trips_relay_and_direct_addresses() {
        let hint = Hint {
            host: host(),
            relay: Some("https://relay.example.com./".parse().unwrap()),
            direct: vec![
                "192.168.1.20:51000".parse().unwrap(),
                "[fe80::1]:51000".parse().unwrap(),
            ],
        };
        let text = hint.to_string();
        assert_eq!(text.parse::<Hint>().unwrap(), hint);
        let addr = hint.endpoint_addr();
        assert_eq!(addr.id, hint.host);
        assert_eq!(addr.ip_addrs().count(), 2);
        assert_eq!(addr.relay_urls().count(), 1);
    }

    #[test]
    fn host_only_hint_is_valid() {
        let id = host();
        let hint: Hint = id.to_string().parse().unwrap();
        assert_eq!(hint.host, id);
        assert!(hint.relay.is_none() && hint.direct.is_empty());
    }

    #[test]
    fn rejects_unknown_duplicate_and_excess_fields() {
        let id = host();
        for bad in [
            format!("{id};x=1"),
            format!("{id};r=https://a.example/;r=https://b.example/"),
            format!("{id};d=not-an-addr"),
            format!("{id};"),
            "not-a-host".to_string(),
            format!("{id}{}", ";d=10.0.0.1:1".repeat(MAX_DIRECT + 1)),
            format!("{id};r=https://{}.example/", "a".repeat(MAX_HINT)),
        ] {
            assert_eq!(bad.parse::<Hint>(), Err(InvalidHint), "{bad}");
        }
    }

    #[test]
    fn from_endpoint_addr_caps_direct_addresses() {
        let id = host();
        let mut addr = EndpointAddr::new(id);
        for port in 1..=20u16 {
            addr = addr.with_ip_addr(SocketAddr::from(([10, 0, 0, 1], port)));
        }
        let hint = Hint::from_endpoint_addr(&addr);
        assert_eq!(hint.direct.len(), MAX_DIRECT);
        assert!(hint.to_string().len() <= MAX_HINT);
        assert_eq!(hint.to_string().parse::<Hint>().unwrap(), hint);
    }
}
