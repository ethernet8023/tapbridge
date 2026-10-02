//! Pairing authorization policy: pure in-memory state, no persistence.
//!
//! The host caller owns durable persistence of the authenticated phone
//! identity BEFORE sending `Accepted`. This module only decides who may pair
//! and when an invitation is consumed: [`PairingGate::commit`] consumes the
//! invitation and must be called only after the caller's persistence
//! succeeded. A commit failure (or persistence failure) leaves the
//! invitation usable, so a phone that lost its acknowledgement can reconnect
//! with the same capability. After a commit, reuse of that capability is a
//! replay and is rejected.
//!
//! No custom signatures or cryptography: the capability is CSPRNG (via
//! `rand`), and identity authentication is delegated to iroh TLS.

use std::fmt;
use std::time::{Duration, Instant};

use iroh::EndpointId;

/// Invitation lifetime in host memory. Minting a new invitation revokes the
/// previous one.
pub const INVITATION_TTL: Duration = Duration::from_secs(5 * 60);

/// Invitation prefix.
pub const PREFIX: &str = "tapbridge";
/// Invitation format version.
pub const VERSION: u32 = 1;

/// A pairing invitation: host endpoint id plus a cryptorandom capability.
///
/// Rendered as `tapbridge:1:HOST_ENDPOINT_ID:CAPABILITY_HEX` for the QR
/// code. [`Debug`] is secret-safe and never shows the capability.
pub struct Invitation {
    host_id: EndpointId,
    capability: [u8; 32],
}

impl Invitation {
    /// Mint an invitation for `host_id` with a fresh CSPRNG capability.
    pub fn new(host_id: EndpointId) -> Self {
        Self {
            host_id,
            capability: rand::random(),
        }
    }

    /// The host endpoint id to dial.
    pub fn host_id(&self) -> EndpointId {
        self.host_id
    }

    /// The shared capability the phone presents in its Hello.
    pub fn capability(&self) -> [u8; 32] {
        self.capability
    }

    /// Parse the QR invitation string `tapbridge:1:HOST_ID:CAPABILITY_HEX`.
    pub fn parse(s: &str) -> Result<Self, PairingError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 4 || parts[0] != PREFIX {
            return Err(PairingError::Malformed);
        }
        if parts[1] != "1" {
            return Err(PairingError::Malformed);
        }
        let host_id: EndpointId = parts[2].parse().map_err(|_| PairingError::Malformed)?;
        let capability = decode_hex32(parts[3]).ok_or(PairingError::Malformed)?;
        Ok(Self {
            host_id,
            capability,
        })
    }
}

impl fmt::Display for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{PREFIX}:{VERSION}:{}:{}",
            self.host_id,
            encode_hex(&self.capability)
        )
    }
}

impl fmt::Debug for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invitation(REDACTED)")
    }
}

/// Extract the capability bytes from an invitation string (phone side).
pub fn capability_from_invitation(s: &str) -> Result<[u8; 32], PairingError> {
    Invitation::parse(s).map(|i| i.capability)
}

/// Why a pairing attempt was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingError {
    /// The connecting peer is not the pinned/known phone.
    UnknownPeer,
    /// No invitation is currently outstanding.
    NoInvitation,
    /// The presented capability does not match the outstanding invitation.
    CapabilityMismatch,
    /// The outstanding invitation has expired.
    Expired,
    /// The capability matches an already-consumed invitation.
    Replay,
    /// The invitation string was not valid.
    Malformed,
}

impl fmt::Display for PairingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::UnknownPeer => "unknown peer",
            Self::NoInvitation => "no invitation outstanding",
            Self::CapabilityMismatch => "capability mismatch",
            Self::Expired => "invitation expired",
            Self::Replay => "invitation replayed",
            Self::Malformed => "malformed invitation",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for PairingError {}

/// An authorized-but-not-yet-committed pairing. Hold it while the host
/// persists the phone identity, then pass it to [`PairingGate::commit`].
pub struct PendingPairing {
    phone: EndpointId,
    capability: Option<[u8; 32]>,
}

impl fmt::Debug for PendingPairing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PendingPairing(REDACTED)")
    }
}

impl PendingPairing {
    /// The iroh-authenticated phone identity to persist.
    pub fn phone(&self) -> EndpointId {
        self.phone
    }
}

struct Outstanding {
    capability: [u8; 32],
    expires: Instant,
}

/// In-memory pairing gate. Not `Clone`: minting is stateful and single-use.
pub struct PairingGate {
    outstanding: Option<Outstanding>,
    consumed: Option<[u8; 32]>,
    ttl: Duration,
}

impl PairingGate {
    /// A gate with the default 5 minute invitation lifetime.
    pub fn new() -> Self {
        Self::with_ttl(INVITATION_TTL)
    }

    /// A gate with a custom lifetime (tests use this for expiry).
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            outstanding: None,
            consumed: None,
            ttl,
        }
    }

    /// Mint a new invitation for this host, revoking any previous one.
    pub fn mint(&mut self, host_id: EndpointId) -> Invitation {
        let capability = rand::random();
        self.outstanding = Some(Outstanding {
            capability,
            expires: Instant::now() + self.ttl,
        });
        Invitation {
            host_id,
            capability,
        }
    }

    /// Authorize a pairing attempt from the iroh-authenticated `peer`
    /// presenting `capability`. Validation only: nothing is consumed.
    pub fn authorize(
        &mut self,
        peer: EndpointId,
        capability: Option<&[u8; 32]>,
    ) -> Result<PendingPairing, PairingError> {
        let capability = capability.ok_or(PairingError::UnknownPeer)?;
        if self.consumed.as_ref() == Some(capability) {
            return Err(PairingError::Replay);
        }
        let outstanding = self
            .outstanding
            .as_ref()
            .ok_or(PairingError::NoInvitation)?;
        if outstanding.capability != *capability {
            return Err(PairingError::CapabilityMismatch);
        }
        if Instant::now() >= outstanding.expires {
            return Err(PairingError::Expired);
        }
        Ok(PendingPairing {
            phone: peer,
            capability: Some(*capability),
        })
    }

    /// Authorize a reconnect from a pinned (already paired) identity without
    /// any capability. Use after the Hello carried no capability.
    pub fn authorize_pinned(
        &mut self,
        peer: EndpointId,
        pinned: EndpointId,
    ) -> Result<PendingPairing, PairingError> {
        if peer == pinned {
            Ok(PendingPairing {
                phone: peer,
                capability: None,
            })
        } else {
            Err(PairingError::UnknownPeer)
        }
    }

    /// Consume the invitation for this pairing. Call only after the host
    /// caller's durable persistence of `pending.phone()` succeeded.
    pub fn commit(&mut self, pending: PendingPairing) -> Result<EndpointId, PairingError> {
        if let Some(capability) = pending.capability {
            let current = self
                .outstanding
                .as_ref()
                .ok_or(PairingError::NoInvitation)?;
            if current.capability != capability {
                return Err(PairingError::CapabilityMismatch);
            }
            if Instant::now() >= current.expires {
                return Err(PairingError::Expired);
            }
            self.outstanding = None;
            self.consumed = Some(capability);
        }
        Ok(pending.phone())
    }
}

impl Default for PairingGate {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PairingGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingGate(REDACTED)")
    }
}

fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)? as u8;
        let lo = (chunk[1] as char).to_digit(16)? as u8;
        out[i] = hi << 4 | lo;
    }
    Some(out)
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
