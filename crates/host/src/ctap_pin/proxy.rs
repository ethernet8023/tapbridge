//! The CTAP2 clientPIN proxy.
//!
//! Why: the PIN protocol is bound to the card's key-agreement key, which
//! the card regenerates on every power-up, i.e. every NFC tap. macOS runs
//! getKeyAgreement → (user types the PIN) → getPinToken → makeCredential
//! as separate requests, so a re-tap anywhere in between broke the flow,
//! and a pinHashEnc encrypted to the stale key decrypts to garbage on the
//! card, which counts it as a wrong PIN.
//!
//! How: tapbridge terminates clientPIN towards the platform with its own
//! key-agreement key and a *virtual* pinUvAuthToken, and runs a real
//! clientPIN exchange towards the card inside one card session (one tap)
//! whenever it needs the card's token: getKeyAgreement, then getPinToken
//! with the verified pinHash re-encrypted to the card's fresh key, then
//! the command with pinUvAuthParam recomputed with the real token.
//!
//! Safety rules this module enforces:
//! - a pinHash reaches the card only freshly encrypted to the key the card
//!   just gave us in the same session (never a stale shared secret);
//! - a pinHash the card has not accepted yet is sent at most once: if the
//!   card is lost after that request went out, the platform gets
//!   `CTAP1_ERR_OTHER` and the user retries from the browser;
//! - only a pinHash the card accepted is reused (re-key after a re-tap);
//! - PIN hashes, tokens and shared secrets are zeroized and never logged;
//! - the platform-facing key-agreement key lives as long as the proxy: if
//!   it changed under the platform, the platform's next pinHashEnc would
//!   decrypt to garbage here and cost the user a retry on the card.
//!
//! Pure: the card is a [`CardPort`] (waiting for taps, transport and the
//! phone's sign-in animation live in the desktop crate), time is passed
//! in, so every rule is unit-tested against [`super::fake`].

use super::cbor::{self, RawMap, Value};
use super::protocol::{KeyAgreement, Protocol, SharedSecret, random};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use tapbridge_core::wire::auth;
use zeroize::Zeroizing;

/// CTAP status codes the proxy produces itself.
pub mod status {
    pub const OK: u8 = 0x00;
    pub const INVALID_COMMAND: u8 = 0x01;
    pub const INVALID_PARAMETER: u8 = 0x02;
    pub const INVALID_CBOR: u8 = 0x12;
    pub const MISSING_PARAMETER: u8 = 0x14;
    pub const UNSUPPORTED_OPTION: u8 = 0x2B;
    pub const KEEPALIVE_CANCEL: u8 = 0x2D;
    pub const USER_ACTION_TIMEOUT: u8 = 0x2F;
    pub const PIN_INVALID: u8 = 0x31;
    pub const PIN_BLOCKED: u8 = 0x32;
    pub const PIN_AUTH_INVALID: u8 = 0x33;
    pub const PIN_AUTH_BLOCKED: u8 = 0x34;
    pub const PIN_NOT_SET: u8 = 0x35;
    pub const PUAT_REQUIRED: u8 = 0x36;
    pub const PIN_POLICY_VIOLATION: u8 = 0x37;
    pub const OTHER: u8 = 0x7F;
}

/// The cached PIN state is forgotten after this long without use.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Re-taps one request may go through before giving up.
const MAX_RETAPS: usize = 8;

/// Why a card step did not produce a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortError {
    /// The card left (transport failure, or a different tap generation)
    /// and the request may have reached it.
    Lost,
    /// The card left before any byte of this request went out (the port
    /// saw the new tap first): the card never saw it, so even a
    /// not-yet-verified pinHash may be retried.
    NotSent,
    /// The card refused at the ISO 7816 level (no FIDO applet, bad SW).
    Card,
    /// The request ends with this CTAP status (cancel, timeout).
    Ended(u8),
}

/// The card on the phone, as the proxy needs it.
pub trait CardPort {
    /// Waits for a card (a *new* tap when `new_tap`: the previous one was
    /// lost) and SELECTs the FIDO applet. Returns the tap's generation.
    fn open(&mut self, new_tap: bool) -> Result<u64, PortError>;
    /// One CTAP request on the opened card (same tap).
    fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, PortError>;
    /// Shows a [`auth`] state on the phone.
    fn auth_state(&mut self, state: u8);
}

const CMD_MAKE_CREDENTIAL: u8 = 0x01;
const CMD_GET_ASSERTION: u8 = 0x02;
const CMD_GET_INFO: u8 = 0x04;
const CMD_CLIENT_PIN: u8 = 0x06;
const CMD_BIO_ENROLLMENT: u8 = 0x09;
const CMD_CREDENTIAL_MANAGEMENT: u8 = 0x0A;
const CMD_LARGE_BLOBS: u8 = 0x0C;
const CMD_CONFIG: u8 = 0x0D;
const CMD_BIO_ENROLLMENT_PRE: u8 = 0x40;
const CMD_CREDENTIAL_MANAGEMENT_PRE: u8 = 0x41;

const SUB_GET_RETRIES: u64 = 0x01;
const SUB_GET_KEY_AGREEMENT: u64 = 0x02;
const SUB_SET_PIN: u64 = 0x03;
const SUB_CHANGE_PIN: u64 = 0x04;
const SUB_GET_PIN_TOKEN: u64 = 0x05;
const SUB_GET_UV_TOKEN: u64 = 0x06;
const SUB_GET_UV_RETRIES: u64 = 0x07;
const SUB_GET_PIN_TOKEN_PERMISSIONS: u64 = 0x09;

/// Protocols offered to the platform, in preference order.
const PROXY_PROTOCOLS: [u64; 2] = [2, 1];

/// The verified PIN state of one sign-in.
struct Session {
    /// LEFT(SHA-256(PIN), 16); the card has accepted it in this flow.
    pin_hash: Zeroizing<[u8; 16]>,
    /// What the platform holds: authenticates its pinUvAuthParams.
    virtual_token: Zeroizing<[u8; 32]>,
    platform_protocol: Protocol,
    /// The token request as the platform made it (sub-command,
    /// permissions, rpId), replayed on a re-key.
    request: TokenRequest,
    card: Option<CardToken>,
    last_used: Instant,
    /// CTAPHID channel that obtained the token (dropped on its resync).
    cid: u32,
}

#[derive(Clone)]
struct TokenRequest {
    sub: u64,
    permissions: Option<u64>,
    rp_id: Option<String>,
}

/// The card's real token, valid for one tap generation.
struct CardToken {
    token: Zeroizing<Vec<u8>>,
    protocol: Protocol,
    generation: u64,
}

/// Where a command keeps its pinUvAuthParam and what it authenticates.
struct ParamLayout {
    param_key: i64,
    protocol_key: i64,
    message: Vec<u8>,
}

/// A card step failed.
enum Fail {
    /// The card left; the step may be retried on a new tap.
    Lost,
    /// The card left after a not-yet-verified pinHash went out.
    LostUnverified,
    /// End the request with this CTAP status.
    Status(u8),
}

impl From<PortError> for Fail {
    fn from(error: PortError) -> Self {
        match error {
            PortError::Lost | PortError::NotSent => Fail::Lost,
            PortError::Card => Fail::Status(status::OTHER),
            PortError::Ended(code) => Fail::Status(code),
        }
    }
}

pub struct PinProxy {
    key: KeyAgreement,
    session: Option<Session>,
    /// pinUvAuthProtocols from the card's last GetInfo.
    card_protocols: Option<Vec<u64>>,
    last_state: Option<u8>,
}

impl Default for PinProxy {
    fn default() -> Self {
        Self::new()
    }
}

impl PinProxy {
    pub fn new() -> Self {
        Self {
            key: KeyAgreement::generate(),
            session: None,
            card_protocols: None,
            last_state: None,
        }
    }

    /// Whether a verified PIN state is cached (tests, diagnostics).
    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// Forgets the cached pinHash and tokens (cancel, resync, timeout).
    pub fn reset(&mut self) {
        self.session = None;
    }

    /// CTAPHID INIT resync of channel `cid`.
    pub fn on_resync(&mut self, cid: u32) {
        if self.session.as_ref().is_some_and(|s| s.cid == cid) {
            self.reset();
        }
    }

    /// Drops the cached PIN state once it has been idle for
    /// [`IDLE_TIMEOUT`] (called on every request and periodically).
    pub fn expire(&mut self, now: Instant) {
        if self
            .session
            .as_ref()
            .is_some_and(|s| now.saturating_duration_since(s.last_used) >= IDLE_TIMEOUT)
        {
            self.reset();
        }
    }

    /// Carries out one CTAP request from the platform on channel `cid`.
    /// Always returns a CTAP reply (status byte first).
    pub fn handle(
        &mut self,
        cid: u32,
        request: &[u8],
        port: &mut dyn CardPort,
        now: Instant,
    ) -> Vec<u8> {
        self.expire(now);
        let Some((&command, body)) = request.split_first() else {
            return vec![status::INVALID_COMMAND];
        };
        let reply = match command {
            CMD_GET_INFO => self.get_info(request, port),
            CMD_CLIENT_PIN => self.client_pin(cid, body, port, now),
            CMD_MAKE_CREDENTIAL | CMD_GET_ASSERTION => self.signing(command, body, port, now),
            CMD_CREDENTIAL_MANAGEMENT
            | CMD_CREDENTIAL_MANAGEMENT_PRE
            | CMD_CONFIG
            | CMD_LARGE_BLOBS
            | CMD_BIO_ENROLLMENT
            | CMD_BIO_ENROLLMENT_PRE => self.authorized_command(command, body, port, now),
            _ => self.relay_neutral(request, port),
        };
        if reply.first() == Some(&status::KEEPALIVE_CANCEL) {
            self.reset();
        }
        reply
    }

    /// Sends `state` to the phone on transitions only.
    fn state(&mut self, port: &mut dyn CardPort, state: u8) {
        if self.last_state != Some(state) {
            self.last_state = Some(state);
            port.auth_state(state);
        }
    }

    /// The end state of a request that failed with `code`: neutral for a
    /// cancel, FAILED otherwise.
    fn end_failed(&mut self, port: &mut dyn CardPort, code: u8) -> Vec<u8> {
        let state = if code == status::KEEPALIVE_CANCEL {
            auth::IDLE
        } else {
            auth::FAILED
        };
        self.state(port, state);
        vec![code]
    }

    /// Runs `step` on the card, retrying it on a new tap whenever the card
    /// is lost (up to [`MAX_RETAPS`] times, within the port's deadline).
    /// The step gets the opened tap's generation.
    fn with_card<T>(
        &mut self,
        port: &mut dyn CardPort,
        mut step: impl FnMut(&mut Self, &mut dyn CardPort, u64) -> Result<T, Fail>,
    ) -> Result<T, u8> {
        let mut new_tap = false;
        for _ in 0..=MAX_RETAPS {
            let generation = match port.open(new_tap) {
                Ok(generation) => generation,
                Err(PortError::Lost | PortError::NotSent) => {
                    self.state(port, auth::REMOVED);
                    new_tap = true;
                    continue;
                }
                Err(error) => return Err(Self::code(error.into())),
            };
            self.state(port, auth::WORKING);
            match step(self, port, generation) {
                Ok(value) => return Ok(value),
                Err(Fail::Lost) => {
                    self.state(port, auth::REMOVED);
                    new_tap = true;
                }
                Err(fail) => return Err(Self::code(fail)),
            }
        }
        Err(status::OTHER)
    }

    fn code(fail: Fail) -> u8 {
        match fail {
            Fail::Status(code) => code,
            Fail::Lost | Fail::LostUnverified => status::OTHER,
        }
    }

    /// Relays a request untouched; the request ends neutrally (IDLE) on
    /// any card answer.
    fn relay_neutral(&mut self, request: &[u8], port: &mut dyn CardPort) -> Vec<u8> {
        match self.with_card(port, |_, port, _| Ok(port.exchange(request)?)) {
            Ok(reply) => {
                self.state(port, auth::IDLE);
                reply
            }
            Err(code) => self.end_failed(port, code),
        }
    }

    /// GetInfo: the card's answer with pinUvAuthProtocols (0x06) replaced
    /// by the protocols the proxy speaks to the platform.
    fn get_info(&mut self, request: &[u8], port: &mut dyn CardPort) -> Vec<u8> {
        let reply = self.relay_neutral(request, port);
        if reply.first() != Some(&status::OK) {
            return reply;
        }
        let Ok(mut info) = RawMap::parse(&reply[1..]) else {
            return reply;
        };
        self.card_protocols = Some(Self::protocols_of(&info));
        let offered = PROXY_PROTOCOLS.iter().map(|p| Value::Uint(*p)).collect();
        info.set(6, &Value::Array(offered));
        let mut rewritten = vec![status::OK];
        rewritten.extend(info.to_bytes());
        rewritten
    }

    /// A card's pinUvAuthProtocols; CTAP 2.0 cards that omit the list
    /// speak protocol 1.
    fn protocols_of(info: &RawMap) -> Vec<u64> {
        match info.get(6) {
            Some(Value::Array(items)) => items.iter().filter_map(Value::as_uint).collect(),
            _ => vec![1],
        }
    }

    /// The protocol to use with the card: the platform's when the card
    /// has it, else the best one both of us speak. Learns the card's list
    /// with a GetInfo on this tap when it is not known yet.
    fn card_protocol(
        &mut self,
        port: &mut dyn CardPort,
        preferred: Protocol,
    ) -> Result<Protocol, Fail> {
        if self.card_protocols.is_none() {
            let reply = port.exchange(&[CMD_GET_INFO])?;
            if reply.first() != Some(&status::OK) {
                return Err(Fail::Status(status::OTHER));
            }
            let info = RawMap::parse(&reply[1..]).map_err(|_| Fail::Status(status::OTHER))?;
            self.card_protocols = Some(Self::protocols_of(&info));
        }
        let known = self.card_protocols.as_deref().unwrap_or(&[]);
        if known.contains(&preferred.number()) {
            return Ok(preferred);
        }
        PROXY_PROTOCOLS
            .iter()
            .find(|p| known.contains(p))
            .and_then(|p| Protocol::from_number(*p))
            .ok_or(Fail::Status(status::UNSUPPORTED_OPTION))
    }

    /// The card's getKeyAgreement on this tap: a fresh shared secret and
    /// the COSE key to send with the next clientPIN command.
    fn card_agreement(
        port: &mut dyn CardPort,
        protocol: Protocol,
    ) -> Result<(SharedSecret, Value), Fail> {
        let request = client_pin_request(protocol, SUB_GET_KEY_AGREEMENT, vec![]);
        let reply = port.exchange(&request)?;
        match reply.first() {
            Some(&status::OK) => {}
            Some(&code) => return Err(Fail::Status(code)),
            None => return Err(Fail::Status(status::OTHER)),
        }
        let peer = cbor::decode(&reply[1..])
            .ok()
            .and_then(|v| v.get(1).cloned())
            .ok_or(Fail::Status(status::OTHER))?;
        let mine = KeyAgreement::generate();
        let shared = mine
            .shared_secret(protocol, &peer)
            .ok_or(Fail::Status(status::OTHER))?;
        Ok((shared, mine.cose_public()))
    }

    /// The card's getPinToken / permissions variant with `pin_hash` on
    /// this tap. `verified` says the card accepted this hash before in
    /// this flow; an unverified hash is never sent twice (a lost reply
    /// becomes [`Fail::LostUnverified`]).
    fn card_token(
        &mut self,
        port: &mut dyn CardPort,
        generation: u64,
        pin_hash: &[u8; 16],
        request: &TokenRequest,
        preferred: Protocol,
        verified: bool,
    ) -> Result<CardToken, Fail> {
        let protocol = self.card_protocol(port, preferred)?;
        let (shared, cose) = Self::card_agreement(port, protocol)?;
        let mut rest = vec![(3, cose), (6, Value::Bytes(shared.encrypt(pin_hash)))];
        if let Some(permissions) = request.permissions {
            rest.push((9, Value::Uint(permissions)));
        }
        if let Some(rp_id) = &request.rp_id {
            rest.push((0x0A, Value::Text(rp_id.clone())));
        }
        let reply = match port.exchange(&client_pin_request(protocol, request.sub, rest)) {
            Ok(reply) => reply,
            Err(PortError::Lost) if !verified => return Err(Fail::LostUnverified),
            Err(error) => return Err(error.into()),
        };
        match reply.first() {
            Some(&status::OK) => {}
            Some(&code) => return Err(Fail::Status(code)),
            None => return Err(Fail::Status(status::OTHER)),
        }
        let token = cbor::decode(&reply[1..])
            .ok()
            .and_then(|v| v.get(2).and_then(|t| t.as_bytes().map(<[u8]>::to_vec)))
            .and_then(|encrypted| shared.decrypt(&encrypted))
            .ok_or(Fail::Status(status::OTHER))?;
        Ok(CardToken {
            token,
            protocol,
            generation,
        })
    }

    fn client_pin(
        &mut self,
        cid: u32,
        body: &[u8],
        port: &mut dyn CardPort,
        now: Instant,
    ) -> Vec<u8> {
        let Ok(params) = RawMap::parse(body) else {
            return vec![status::INVALID_CBOR];
        };
        let Some(sub) = params.get(2).and_then(|v| v.as_uint()) else {
            return vec![status::MISSING_PARAMETER];
        };
        match sub {
            SUB_GET_RETRIES | SUB_GET_UV_RETRIES => {
                let mut request = vec![CMD_CLIENT_PIN];
                request.extend_from_slice(body);
                match self.with_card(port, |_, port, _| Ok(port.exchange(&request)?)) {
                    Ok(reply) => {
                        // macOS asks for the retries right before its
                        // PIN prompt.
                        let prompting =
                            sub == SUB_GET_RETRIES && reply.first() == Some(&status::OK);
                        self.state(port, if prompting { auth::PIN } else { auth::IDLE });
                        reply
                    }
                    Err(code) => self.end_failed(port, code),
                }
            }
            SUB_GET_KEY_AGREEMENT => match platform_protocol(&params) {
                Ok(_) => {
                    self.state(port, auth::PIN);
                    let mut reply = vec![status::OK];
                    reply.extend(cbor::encode(&cbor::int_map(vec![(
                        1,
                        self.key.cose_public(),
                    )])));
                    reply
                }
                Err(code) => vec![code],
            },
            SUB_GET_PIN_TOKEN | SUB_GET_PIN_TOKEN_PERMISSIONS => {
                self.get_pin_token(cid, sub, &params, port, now)
            }
            SUB_SET_PIN => self.set_pin(&params, port),
            SUB_CHANGE_PIN => self.change_pin(&params, port),
            SUB_GET_UV_TOKEN => vec![status::UNSUPPORTED_OPTION],
            _ => {
                let mut request = vec![CMD_CLIENT_PIN];
                request.extend_from_slice(body);
                self.relay_neutral(&request, port)
            }
        }
    }

    /// The platform's shared secret from its key agreement (key 3).
    fn platform_shared(&self, params: &RawMap) -> Result<SharedSecret, u8> {
        let protocol = platform_protocol(params)?;
        let peer = params.get(3).ok_or(status::MISSING_PARAMETER)?;
        self.key
            .shared_secret(protocol, &peer)
            .ok_or(status::INVALID_PARAMETER)
    }

    fn get_pin_token(
        &mut self,
        cid: u32,
        sub: u64,
        params: &RawMap,
        port: &mut dyn CardPort,
        now: Instant,
    ) -> Vec<u8> {
        // A new token request replaces whatever was cached.
        self.reset();
        let shared = match self.platform_shared(params) {
            Ok(shared) => shared,
            Err(code) => return vec![code],
        };
        let Some(pin_hash_enc) = params.get(6).and_then(|v| v.as_bytes().map(<[u8]>::to_vec))
        else {
            return vec![status::MISSING_PARAMETER];
        };
        let pin_hash: Zeroizing<[u8; 16]> = match shared.decrypt(&pin_hash_enc) {
            Some(plain) if plain.len() == 16 => Zeroizing::new(plain[..].try_into().unwrap()),
            _ => return vec![status::INVALID_PARAMETER],
        };
        let request = TokenRequest {
            sub,
            permissions: (sub == SUB_GET_PIN_TOKEN_PERMISSIONS)
                .then(|| params.get(9).and_then(|v| v.as_uint()))
                .flatten(),
            rp_id: params
                .get(0x0A)
                .and_then(|v| v.as_text().map(str::to_owned)),
        };
        if sub == SUB_GET_PIN_TOKEN_PERMISSIONS && request.permissions.unwrap_or(0) == 0 {
            return vec![status::INVALID_PARAMETER];
        }
        let preferred = shared.protocol();
        let result = self.with_card(port, |proxy, port, generation| {
            proxy.card_token(port, generation, &pin_hash, &request, preferred, false)
        });
        let card = match result {
            Ok(card) => card,
            Err(code) => return self.end_failed(port, code),
        };
        let mut virtual_token = Zeroizing::new([0u8; 32]);
        random(virtual_token.as_mut());
        let mut reply = vec![status::OK];
        reply.extend(cbor::encode(&cbor::int_map(vec![(
            2,
            Value::Bytes(shared.encrypt(virtual_token.as_ref())),
        )])));
        self.session = Some(Session {
            pin_hash,
            virtual_token,
            platform_protocol: preferred,
            request,
            card: Some(card),
            last_used: now,
            cid,
        });
        reply
    }

    /// setPIN: newPinEnc is re-encrypted to the card's shared secret.
    /// The proxy sees the new PIN here (setPIN carries the PIN itself,
    /// not its hash); it lives only in a zeroized buffer for this call.
    fn set_pin(&mut self, params: &RawMap, port: &mut dyn CardPort) -> Vec<u8> {
        let shared = match self.platform_shared(params) {
            Ok(shared) => shared,
            Err(code) => return vec![code],
        };
        let (Some(param), Some(new_pin_enc)) = (bytes_of(params, 4), bytes_of(params, 5)) else {
            return vec![status::MISSING_PARAMETER];
        };
        if !shared.verify(&new_pin_enc, &param) {
            return vec![status::PIN_AUTH_INVALID];
        }
        let new_pin = match shared.decrypt(&new_pin_enc) {
            Some(plain) if plain.len() == 64 => plain,
            _ => return vec![status::INVALID_PARAMETER],
        };
        let preferred = shared.protocol();
        self.pin_change(
            port,
            preferred,
            |shared| {
                let new_pin_enc = shared.encrypt(&new_pin);
                vec![
                    (4, Value::Bytes(shared.authenticate(&new_pin_enc))),
                    (5, Value::Bytes(new_pin_enc)),
                ]
            },
            SUB_SET_PIN,
        )
    }

    /// changePIN: the current pinHash and the new PIN are re-encrypted;
    /// the current pinHash is unverified, so it is never resent.
    fn change_pin(&mut self, params: &RawMap, port: &mut dyn CardPort) -> Vec<u8> {
        self.reset();
        let shared = match self.platform_shared(params) {
            Ok(shared) => shared,
            Err(code) => return vec![code],
        };
        let (Some(param), Some(new_pin_enc), Some(pin_hash_enc)) = (
            bytes_of(params, 4),
            bytes_of(params, 5),
            bytes_of(params, 6),
        ) else {
            return vec![status::MISSING_PARAMETER];
        };
        let mut message = new_pin_enc.clone();
        message.extend_from_slice(&pin_hash_enc);
        if !shared.verify(&message, &param) {
            return vec![status::PIN_AUTH_INVALID];
        }
        let (new_pin, pin_hash) =
            match (shared.decrypt(&new_pin_enc), shared.decrypt(&pin_hash_enc)) {
                (Some(new_pin), Some(pin_hash)) if new_pin.len() == 64 && pin_hash.len() == 16 => {
                    (new_pin, pin_hash)
                }
                _ => return vec![status::INVALID_PARAMETER],
            };
        let preferred = shared.protocol();
        self.pin_change(
            port,
            preferred,
            |shared| {
                let new_pin_enc = shared.encrypt(&new_pin);
                let pin_hash_enc = shared.encrypt(&pin_hash);
                let mut message = new_pin_enc.clone();
                message.extend_from_slice(&pin_hash_enc);
                vec![
                    (4, Value::Bytes(shared.authenticate(&message))),
                    (5, Value::Bytes(new_pin_enc)),
                    (6, Value::Bytes(pin_hash_enc)),
                ]
            },
            SUB_CHANGE_PIN,
        )
    }

    /// setPIN / changePIN on the card: getKeyAgreement, then the command
    /// built by `fields` for the card's shared secret. Neither is resent
    /// once it went out (a lost reply is `CTAP1_ERR_OTHER`).
    fn pin_change(
        &mut self,
        port: &mut dyn CardPort,
        preferred: Protocol,
        fields: impl Fn(&SharedSecret) -> Vec<(i64, Value)>,
        sub: u64,
    ) -> Vec<u8> {
        let result = self.with_card(port, |proxy, port, _| {
            let protocol = proxy.card_protocol(port, preferred)?;
            let (shared, cose) = Self::card_agreement(port, protocol)?;
            let mut rest = vec![(3, cose)];
            rest.extend(fields(&shared));
            match port.exchange(&client_pin_request(protocol, sub, rest)) {
                Ok(reply) => Ok(reply),
                Err(PortError::Lost) => Err(Fail::LostUnverified),
                Err(error) => Err(error.into()),
            }
        });
        match result {
            Ok(reply) if reply.first() == Some(&status::OK) => {
                self.state(port, auth::IDLE);
                vec![status::OK]
            }
            Ok(reply) => self.end_failed(port, reply.first().copied().unwrap_or(status::OTHER)),
            Err(code) => self.end_failed(port, code),
        }
    }

    /// makeCredential / getAssertion.
    fn signing(
        &mut self,
        command: u8,
        body: &[u8],
        port: &mut dyn CardPort,
        now: Instant,
    ) -> Vec<u8> {
        let Ok(params) = RawMap::parse(body) else {
            return vec![status::INVALID_CBOR];
        };
        let (cdh_key, layout_keys, options_key) = if command == CMD_MAKE_CREDENTIAL {
            (1, (8, 9), 7)
        } else {
            (2, (6, 7), 5)
        };
        // up=false is a silent preflight (does a credential exist?): it
        // neither finishes the sign-in nor spends the token.
        let silent = params
            .get(options_key)
            .is_some_and(|options| match options {
                Value::Map(entries) => entries
                    .iter()
                    .any(|(k, v)| k.as_text() == Some("up") && *v == Value::Bool(false)),
                _ => false,
            });
        let mut request = vec![command];
        request.extend_from_slice(body);
        let reply = match params.get(layout_keys.0) {
            // No param, or the zero-length probe (is a PIN set?): as is.
            None => self.relay_signing(&request, port),
            Some(Value::Bytes(param)) if param.is_empty() => {
                return self.relay_neutral(&request, port);
            }
            Some(_) => {
                let Some(cdh) = bytes_of(&params, cdh_key) else {
                    return vec![status::MISSING_PARAMETER];
                };
                let layout = ParamLayout {
                    param_key: layout_keys.0,
                    protocol_key: layout_keys.1,
                    message: cdh,
                };
                match self.proxied(command, params, &layout, port, now) {
                    Ok(reply) => reply,
                    Err(code) => return self.end_failed(port, code),
                }
            }
        };
        match reply.first() {
            Some(&status::OK) if !silent => {
                self.reset();
                self.state(port, auth::SUCCESS);
            }
            Some(&status::OK | &status::PUAT_REQUIRED) => self.state(port, auth::IDLE),
            _ if silent => self.state(port, auth::IDLE),
            _ => self.state(port, auth::FAILED),
        }
        reply
    }

    /// A signing request without a param: relayed, end state by caller.
    fn relay_signing(&mut self, request: &[u8], port: &mut dyn CardPort) -> Vec<u8> {
        match self.with_card(port, |_, port, _| Ok(port.exchange(request)?)) {
            Ok(reply) => reply,
            Err(code) => vec![code],
        }
    }

    /// Credential management, authenticatorConfig, largeBlobs and bio
    /// enrollment: the same param proxying over their own messages.
    /// Commands without a param are relayed as is. The session is kept
    /// (these come in series, e.g. an enumeration).
    fn authorized_command(
        &mut self,
        command: u8,
        body: &[u8],
        port: &mut dyn CardPort,
        now: Instant,
    ) -> Vec<u8> {
        let mut request = vec![command];
        request.extend_from_slice(body);
        let Ok(params) = RawMap::parse(body) else {
            return self.relay_neutral(&request, port);
        };
        let layout = match command {
            CMD_CREDENTIAL_MANAGEMENT | CMD_CREDENTIAL_MANAGEMENT_PRE => ParamLayout {
                param_key: 4,
                protocol_key: 3,
                message: raw_concat(&params, &[1, 2]),
            },
            CMD_BIO_ENROLLMENT | CMD_BIO_ENROLLMENT_PRE => ParamLayout {
                param_key: 5,
                protocol_key: 4,
                message: raw_concat(&params, &[1, 2, 3]),
            },
            CMD_CONFIG => {
                let Some(sub) = params.get(1).and_then(|v| v.as_uint()) else {
                    return vec![status::MISSING_PARAMETER];
                };
                let mut message = vec![0xFF; 32];
                message.extend_from_slice(&[CMD_CONFIG, sub as u8]);
                message.extend_from_slice(params.raw(2).unwrap_or_default());
                ParamLayout {
                    param_key: 4,
                    protocol_key: 3,
                    message,
                }
            }
            _ => {
                // largeBlobs: only a set carries a param.
                let (Some(set), Some(offset)) = (
                    bytes_of(&params, 2),
                    params.get(3).and_then(|v| v.as_uint()),
                ) else {
                    return self.relay_neutral(&request, port);
                };
                let mut message = vec![0xFF; 32];
                message.extend_from_slice(&[CMD_LARGE_BLOBS, 0x00]);
                message.extend_from_slice(&(offset as u32).to_le_bytes());
                message.extend_from_slice(&Sha256::digest(&set));
                ParamLayout {
                    param_key: 5,
                    protocol_key: 6,
                    message,
                }
            }
        };
        if params.get(layout.param_key).is_none() {
            return self.relay_neutral(&request, port);
        }
        match self.proxied(command, params, &layout, port, now) {
            Ok(reply) => {
                let state = if reply.first() == Some(&status::OK) {
                    auth::IDLE
                } else {
                    auth::FAILED
                };
                self.state(port, state);
                reply
            }
            Err(code) => self.end_failed(port, code),
        }
    }

    /// Checks the platform's param with the virtual token, then on the
    /// card (one tap): re-keys when needed, recomputes the param with the
    /// real token and sends the rewritten command. Retried on a new tap
    /// when the card is lost; a card refusing a reused token gets one
    /// re-key with the verified pinHash.
    fn proxied(
        &mut self,
        command: u8,
        mut params: RawMap,
        layout: &ParamLayout,
        port: &mut dyn CardPort,
        now: Instant,
    ) -> Result<Vec<u8>, u8> {
        let Some(session) = self.session.as_mut() else {
            return Err(status::PIN_AUTH_INVALID);
        };
        let param = bytes_of(&params, layout.param_key).ok_or(status::INVALID_PARAMETER)?;
        let protocol = params
            .get(layout.protocol_key)
            .and_then(|v| v.as_uint())
            .ok_or(status::MISSING_PARAMETER)?;
        if protocol != session.platform_protocol.number()
            || !session.platform_protocol.verify(
                session.virtual_token.as_ref(),
                &layout.message,
                &param,
            )
        {
            return Err(status::PIN_AUTH_INVALID);
        }
        session.last_used = now;
        self.with_card(port, |proxy, port, generation| {
            let mut rekeyed = false;
            loop {
                let fresh = proxy.ensure_card_token(port, generation)?;
                let session = proxy.session.as_ref().ok_or(Fail::Status(status::OTHER))?;
                let card = session.card.as_ref().ok_or(Fail::Status(status::OTHER))?;
                params.replace(
                    layout.param_key,
                    &Value::Bytes(card.protocol.authenticate(&card.token, &layout.message)),
                );
                params.replace(layout.protocol_key, &Value::Uint(card.protocol.number()));
                let mut request = vec![command];
                request.extend(params.to_bytes());
                let reply = port.exchange(&request)?;
                if reply.first() == Some(&status::PIN_AUTH_INVALID) && !fresh && !rekeyed {
                    // The card dropped a token we reused (its own timer,
                    // spent permissions): re-key once with the verified
                    // hash.
                    if let Some(session) = proxy.session.as_mut() {
                        session.card = None;
                    }
                    rekeyed = true;
                    continue;
                }
                return Ok(reply);
            }
        })
    }

    /// Makes sure the session holds a card token for tap `generation`:
    /// re-keys with the verified pinHash after a re-tap. True when a new
    /// token was just obtained.
    fn ensure_card_token(
        &mut self,
        port: &mut dyn CardPort,
        generation: u64,
    ) -> Result<bool, Fail> {
        let session = self
            .session
            .as_mut()
            .ok_or(Fail::Status(status::PIN_AUTH_INVALID))?;
        if session
            .card
            .as_ref()
            .is_some_and(|c| c.generation == generation)
        {
            return Ok(false);
        }
        session.card = None;
        let pin_hash = session.pin_hash.clone();
        let request = session.request.clone();
        let preferred = session.platform_protocol;
        match self.card_token(port, generation, &pin_hash, &request, preferred, true) {
            Ok(card) => {
                if let Some(session) = self.session.as_mut() {
                    session.card = Some(card);
                }
                Ok(true)
            }
            Err(Fail::Status(code)) => {
                // The card refused a hash it accepted before (PIN changed
                // elsewhere, blocked): never try it again.
                self.reset();
                Err(Fail::Status(code))
            }
            Err(fail) => Err(fail),
        }
    }
}

/// A clientPIN request (command byte || map) with protocol and
/// sub-command first, then `rest` (keys in ascending order).
fn client_pin_request(protocol: Protocol, sub: u64, rest: Vec<(i64, Value)>) -> Vec<u8> {
    let mut entries = vec![(1, Value::Uint(protocol.number())), (2, Value::Uint(sub))];
    entries.extend(rest);
    let mut request = vec![CMD_CLIENT_PIN];
    request.extend(cbor::encode(&cbor::int_map(entries)));
    request
}

/// The platform's pinUvAuthProtocol (key 1 of clientPIN).
fn platform_protocol(params: &RawMap) -> Result<Protocol, u8> {
    let number = params
        .get(1)
        .ok_or(status::MISSING_PARAMETER)?
        .as_uint()
        .ok_or(status::INVALID_PARAMETER)?;
    Protocol::from_number(number).ok_or(status::INVALID_PARAMETER)
}

fn bytes_of(params: &RawMap, key: i64) -> Option<Vec<u8>> {
    params
        .get(key)
        .and_then(|v| v.as_bytes().map(<[u8]>::to_vec))
}

/// The raw bytes of `keys`' values, concatenated (absent keys skipped):
/// what credential management and bio enrollment authenticate.
fn raw_concat(params: &RawMap, keys: &[i64]) -> Vec<u8> {
    keys.iter()
        .filter_map(|key| params.raw(*key))
        .flatten()
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::fake::{self, Authenticator, Platform, pin_hash};
    use super::*;

    /// The phone as the proxy sees it, around the software card. Taps
    /// power-cycle the card; `drop_at` makes the transport fail on the
    /// n-th exchange (the card left); `retap_at` silently re-taps before
    /// the n-th exchange (the generation changes under a running flow).
    struct Phone {
        card: Authenticator,
        generation: u64,
        opened: Option<u64>,
        exchanges: usize,
        drop_at: Vec<usize>,
        taps_left: usize,
        states: Vec<u8>,
        /// Requests that went out on a tap other than the opened one.
        stale_sends: usize,
        cancel_after: Option<usize>,
    }

    impl Phone {
        fn new(card: Authenticator) -> Self {
            Self {
                card,
                generation: 1,
                opened: None,
                exchanges: 0,
                drop_at: Vec::new(),
                taps_left: 10,
                states: Vec::new(),
                stale_sends: 0,
                cancel_after: None,
            }
        }

        /// The user lifts the card and taps it again.
        fn retap(&mut self) {
            self.card.power_cycle();
            self.generation += 1;
        }

        fn send(&mut self, proxy: &mut PinProxy, request: &[u8]) -> Vec<u8> {
            self.send_at(proxy, request, Instant::now())
        }

        fn send_at(&mut self, proxy: &mut PinProxy, request: &[u8], now: Instant) -> Vec<u8> {
            proxy.handle(7, request, self, now)
        }
    }

    impl CardPort for Phone {
        fn open(&mut self, new_tap: bool) -> Result<u64, PortError> {
            if new_tap {
                if self.taps_left == 0 {
                    return Err(PortError::Ended(status::USER_ACTION_TIMEOUT));
                }
                self.taps_left -= 1;
                self.retap();
            }
            let reply = self.card.apdu(&crate::ctap_nfc::select_apdu());
            assert!(reply.ends_with(&[0x90, 0x00]));
            self.opened = Some(self.generation);
            Ok(self.generation)
        }

        fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, PortError> {
            self.exchanges += 1;
            if self.cancel_after.is_some_and(|n| self.exchanges > n) {
                return Err(PortError::Ended(status::KEEPALIVE_CANCEL));
            }
            if self.drop_at.contains(&self.exchanges) {
                // The card leaves while this request is in the air: it
                // may have run it; the reply never arrives.
                self.card.ctap(request);
                self.card.power_cycle();
                self.generation += 1;
                return Err(PortError::Lost);
            }
            if self.opened != Some(self.generation) {
                self.stale_sends += 1;
                return Err(PortError::Lost);
            }
            crate::ctap_nfc::exchange(request, |apdu| Ok::<_, ()>(self.card.apdu(&apdu)))
                .map_err(|_| PortError::Card)
        }

        fn auth_state(&mut self, state: u8) {
            self.states.push(state);
        }
    }

    const PIN: &str = "1234";
    const RP: &str = "webauthn.io";

    fn info_protocols(reply: &[u8]) -> Option<Value> {
        cbor::decode(&reply[1..]).unwrap().get(6).cloned()
    }

    /// The platform's whole PIN dance through the proxy; returns the
    /// virtual token.
    fn platform_token(
        phone: &mut Phone,
        proxy: &mut PinProxy,
        platform: &Platform,
        permissions: Option<(u8, Option<&str>)>,
    ) -> Result<Vec<u8>, u8> {
        let mut send = |request: &[u8]| phone.send(proxy, request);
        platform.token(&mut send, PIN, permissions)
    }

    #[test]
    fn get_info_is_relayed_with_the_proxy_protocols() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[1]));
        let mut proxy = PinProxy::new();
        let reply = phone.send(&mut proxy, &[0x04]);
        assert_eq!(reply[0], status::OK);
        assert_eq!(
            info_protocols(&reply),
            Some(Value::Array(vec![Value::Uint(2), Value::Uint(1)]))
        );
        // Everything else is the card's, untouched.
        let card_info = phone.card.ctap(&[0x04]);
        let mut expected = RawMap::parse(&card_info[1..]).unwrap();
        expected.replace(6, &Value::Array(vec![Value::Uint(2), Value::Uint(1)]));
        assert_eq!(reply[1..], expected.to_bytes());
        assert_eq!(phone.states, [auth::WORKING, auth::IDLE]);
    }

    #[test]
    fn get_key_agreement_is_answered_without_the_card() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[1]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let reply = phone.send(&mut proxy, &platform.client_pin(0x02, vec![]));
        assert_eq!(reply[0], status::OK);
        let key = cbor::decode(&reply[1..]).unwrap().get(1).cloned().unwrap();
        assert_eq!(key.get(3), Some(&Value::int(-25)));
        assert_eq!(phone.exchanges, 0, "no card involved");
        assert!(phone.card.log.is_empty(), "not even a SELECT");
        assert_eq!(phone.states, [auth::PIN]);
    }

    #[test]
    fn safari_sequence_survives_a_retap_between_every_step() {
        for (platform_protocol, card_protocols) in [
            (Protocol::One, &[1u64][..]),
            (Protocol::Two, &[1, 2][..]),
            (Protocol::Two, &[1][..]),
            (Protocol::One, &[2][..]),
        ] {
            let mut phone = Phone::new(Authenticator::new(Some(PIN), card_protocols));
            let mut proxy = PinProxy::new();
            let platform = Platform::new(platform_protocol);
            let cdh = [0x5Au8; 32];
            assert_eq!(phone.send(&mut proxy, &[0x04])[0], status::OK);
            phone.retap();
            let probe = platform.make_credential(&cdh, RP, Some(vec![]));
            assert_eq!(phone.send(&mut proxy, &probe), [status::PIN_INVALID]);
            phone.retap();
            let retries = phone.send(&mut proxy, &platform.client_pin(0x01, vec![]));
            assert_eq!(retries[0], status::OK);
            // getKeyAgreement ... the user types the PIN ... the card is
            // lifted and tapped again ... getPinToken.
            let mut send = |request: &[u8]| phone.send(&mut proxy, request);
            let (shared, cose) = platform.agree(&mut send).unwrap();
            phone.retap();
            let request = platform.token_request(&shared, cose, PIN, None);
            let reply = phone.send(&mut proxy, &request);
            let token = platform.open_token(&shared, &reply).unwrap();
            phone.retap();
            let param = platform_protocol.authenticate(&token, &cdh);
            let reply = phone.send(&mut proxy, &platform.make_credential(&cdh, RP, Some(param)));
            assert_eq!(
                reply[0],
                status::OK,
                "{platform_protocol:?} {card_protocols:?}"
            );
            assert_eq!(phone.card.counters.wrong_pin, 0);
            assert_eq!(phone.card.counters.signatures, 1);
            assert_eq!(
                phone.card.counters.tokens_issued, 2,
                "issued, then re-keyed"
            );
            assert_eq!(phone.stale_sends, 0);
        }
    }

    #[test]
    fn the_virtual_token_is_not_the_card_token() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        // The platform's token does not work on the card directly.
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let direct = phone
            .card
            .ctap(&platform.get_assertion(&cdh, RP, Some(param)));
        assert_eq!(direct, [status::PIN_AUTH_INVALID]);
    }

    #[test]
    fn a_wrong_pin_returns_pin_invalid_and_costs_exactly_one_retry() {
        let mut phone = Phone::new(Authenticator::new(Some("9999"), &[1, 2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let result = platform_token(&mut phone, &mut proxy, &platform, None);
        assert_eq!(result, Err(status::PIN_INVALID));
        assert_eq!(phone.card.retries, 7);
        assert_eq!(phone.card.counters.wrong_pin, 1);
        assert!(!proxy.has_session());
        assert_eq!(phone.states.last(), Some(&auth::FAILED));
        // Nothing is retried behind the user's back on the next tap.
        phone.retap();
        assert_eq!(phone.send(&mut proxy, &[0x04])[0], status::OK);
        assert_eq!(phone.card.counters.wrong_pin, 1);
    }

    #[test]
    fn a_blocked_pin_is_returned_as_is() {
        let mut card = Authenticator::new(Some("9999"), &[2]);
        card.retries = 0;
        let mut phone = Phone::new(card);
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let result = platform_token(&mut phone, &mut proxy, &platform, None);
        assert_eq!(result, Err(status::PIN_BLOCKED));
    }

    #[test]
    fn a_wrong_platform_param_is_pin_auth_invalid_and_never_reaches_the_card() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        let requests_before = phone.card.requests.len();
        let cdh = [3u8; 32];
        let bad = Protocol::Two.authenticate(&[0u8; 32], &cdh);
        let reply = phone.send(&mut proxy, &platform.make_credential(&cdh, RP, Some(bad)));
        assert_eq!(reply, [status::PIN_AUTH_INVALID]);
        assert_eq!(phone.card.requests.len(), requests_before);
        // A param for a different protocol than the token's is refused too.
        let mut wrong_protocol = platform.make_credential(&cdh, RP, Some(vec![0; 16]));
        let mut map = RawMap::parse(&wrong_protocol[1..]).unwrap();
        map.replace(9, &Value::Uint(1));
        wrong_protocol.truncate(1);
        wrong_protocol.extend(map.to_bytes());
        assert_eq!(
            phone.send(&mut proxy, &wrong_protocol),
            [status::PIN_AUTH_INVALID]
        );
    }

    #[test]
    fn a_param_without_a_session_is_pin_auth_invalid() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let cdh = [3u8; 32];
        let param = Protocol::Two.authenticate(&[9u8; 32], &cdh);
        let reply = phone.send(&mut proxy, &platform.get_assertion(&cdh, RP, Some(param)));
        assert_eq!(reply, [status::PIN_AUTH_INVALID]);
        assert!(phone.card.requests.is_empty());
    }

    #[test]
    fn the_param_is_rewritten_for_the_card_and_nothing_else_changes() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[1]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        let cdh = [0x77u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let request = platform.make_credential(&cdh, RP, Some(param));
        assert_eq!(phone.send(&mut proxy, &request)[0], status::OK);
        let sent = phone.card.requests.last().unwrap().clone();
        assert_eq!(sent[0], 0x01);
        let sent_map = RawMap::parse(&sent[1..]).unwrap();
        let original = RawMap::parse(&request[1..]).unwrap();
        let keys = |map: &RawMap| {
            map.entries
                .iter()
                .map(|e| e.key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&sent_map), keys(&original), "same keys, same order");
        for entry in &original.entries {
            if entry.key != Value::Uint(8) && entry.key != Value::Uint(9) {
                assert_eq!(sent_map.raw_for(&entry.key), Some(&entry.raw_value[..]));
            }
        }
        assert_eq!(sent_map.get(9), Some(Value::Uint(1)), "card protocol");
        let card_param = sent_map.get(8).unwrap();
        assert_eq!(
            card_param.as_bytes().unwrap().len(),
            16,
            "protocol 1 truncation"
        );
    }

    #[test]
    fn the_zero_length_probe_is_relayed_as_is() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let probe = platform.make_credential(&[0; 32], RP, Some(vec![]));
        assert_eq!(phone.send(&mut proxy, &probe), [status::PIN_INVALID]);
        assert_eq!(phone.card.requests.last(), Some(&probe));
        assert_eq!(phone.card.counters.wrong_pin, 0);
        assert_eq!(phone.states, [auth::WORKING, auth::IDLE]);
        let mut no_pin = Phone::new(Authenticator::new(None, &[2]));
        assert_eq!(no_pin.send(&mut proxy, &probe), [status::PIN_NOT_SET]);
    }

    #[test]
    fn commands_without_a_param_are_relayed_untouched() {
        let mut phone = Phone::new(Authenticator::new(None, &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let request = platform.make_credential(&[4; 32], RP, None);
        assert_eq!(phone.send(&mut proxy, &request)[0], status::OK);
        assert_eq!(phone.card.requests.last(), Some(&request));
        assert_eq!(phone.states, [auth::WORKING, auth::SUCCESS]);
    }

    #[test]
    fn the_session_ends_after_a_successful_make_credential() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let request = platform.make_credential(&cdh, RP, Some(param));
        assert_eq!(phone.send(&mut proxy, &request)[0], status::OK);
        assert!(!proxy.has_session());
        assert_eq!(phone.states.last(), Some(&auth::SUCCESS));
        assert_eq!(phone.send(&mut proxy, &request), [status::PIN_AUTH_INVALID]);
    }

    #[test]
    fn the_session_expires_after_two_idle_minutes() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let request = platform.get_assertion(&cdh, RP, Some(param));
        let later = Instant::now() + IDLE_TIMEOUT + Duration::from_secs(1);
        assert_eq!(
            phone.send_at(&mut proxy, &request, later),
            [status::PIN_AUTH_INVALID]
        );
        assert!(!proxy.has_session());
    }

    #[test]
    fn resync_of_the_token_channel_and_reset_forget_the_session() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        proxy.on_resync(99);
        assert!(proxy.has_session(), "another channel");
        proxy.on_resync(7);
        assert!(!proxy.has_session());
        platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        proxy.reset();
        assert!(!proxy.has_session());
    }

    #[test]
    fn a_cancel_mid_request_forgets_the_session() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        phone.cancel_after = Some(phone.exchanges);
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let reply = phone.send(&mut proxy, &platform.get_assertion(&cdh, RP, Some(param)));
        assert_eq!(reply, [status::KEEPALIVE_CANCEL]);
        assert!(!proxy.has_session());
    }

    #[test]
    fn a_card_lost_mid_make_credential_is_retried_on_the_next_tap() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        // Same tap: the real token is reused, so the next exchange is the
        // makeCredential itself; the card leaves during it.
        phone.drop_at = vec![phone.exchanges + 1];
        phone.states.clear();
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let reply = phone.send(&mut proxy, &platform.make_credential(&cdh, RP, Some(param)));
        assert_eq!(reply[0], status::OK);
        assert_eq!(phone.card.counters.wrong_pin, 0);
        assert_eq!(phone.taps_left, 9, "one re-tap");
        // WORKING was already showing (getPinToken): transitions only.
        assert_eq!(phone.states, [auth::REMOVED, auth::WORKING, auth::SUCCESS]);
    }

    #[test]
    fn a_card_lost_during_the_rekey_is_retried_with_the_verified_pin() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        phone.retap();
        // Exchanges after the re-tap: getKeyAgreement, getPinToken (lost).
        phone.drop_at = vec![phone.exchanges + 2];
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let reply = phone.send(&mut proxy, &platform.get_assertion(&cdh, RP, Some(param)));
        assert_eq!(reply[0], status::OK);
        assert_eq!(phone.card.counters.wrong_pin, 0);
        assert_eq!(phone.stale_sends, 0);
    }

    #[test]
    fn a_card_lost_during_an_unverified_get_pin_token_is_not_retried() {
        let mut phone = Phone::new(Authenticator::new(Some("9999"), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        // The token flow's exchanges: getInfo (protocols unknown yet),
        // getKeyAgreement, getPinToken: the third is lost.
        phone.drop_at = vec![3];
        let result = platform_token(&mut phone, &mut proxy, &platform, None);
        assert_eq!(result, Err(status::OTHER));
        assert_eq!(
            phone.card.counters.wrong_pin, 1,
            "the one attempt the card saw"
        );
        assert_eq!(phone.taps_left, 10, "no new tap was waited for");
        assert_eq!(phone.states.last(), Some(&auth::FAILED));
    }

    #[test]
    fn a_retap_caught_before_get_pin_token_went_out_is_retried() {
        // The port saw the new tap before sending anything: the card never
        // saw this pinHash, so a retry on the new tap is safe.
        struct Stale<'a>(&'a mut Phone, usize);
        impl CardPort for Stale<'_> {
            fn open(&mut self, new_tap: bool) -> Result<u64, PortError> {
                self.0.open(new_tap)
            }
            fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, PortError> {
                let is_token = request[0] == 0x06 && request.windows(2).any(|w| w == [0x02, 0x05]);
                if is_token && self.1 == 0 {
                    self.1 += 1;
                    self.0.retap();
                    return Err(PortError::NotSent);
                }
                self.0.exchange(request)
            }
            fn auth_state(&mut self, state: u8) {
                self.0.auth_state(state);
            }
        }
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let mut port = Stale(&mut phone, 0);
        let mut send = |request: &[u8]| proxy.handle(7, request, &mut port, Instant::now());
        assert!(platform.token(&mut send, PIN, None).is_ok());
        assert_eq!(phone.card.counters.wrong_pin, 0);
        assert_eq!(phone.card.counters.tokens_issued, 1);
    }

    #[test]
    fn a_card_lost_before_the_unverified_pin_went_out_is_retried() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        phone.drop_at = vec![2]; // getKeyAgreement
        let token = platform_token(&mut phone, &mut proxy, &platform, None);
        assert!(token.is_ok());
        assert_eq!(phone.card.counters.wrong_pin, 0);
        assert!(phone.states.contains(&auth::REMOVED));
    }

    #[test]
    fn a_generation_change_mid_sequence_never_sends_to_the_stale_key() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        // A silent re-tap between the proxy's open and its first exchange.
        struct Retapping<'a>(&'a mut Phone, bool);
        impl CardPort for Retapping<'_> {
            fn open(&mut self, new_tap: bool) -> Result<u64, PortError> {
                let generation = self.0.open(new_tap)?;
                if !self.1 {
                    self.1 = true;
                    self.0.retap();
                }
                Ok(generation)
            }
            fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, PortError> {
                self.0.exchange(request)
            }
            fn auth_state(&mut self, state: u8) {
                self.0.auth_state(state);
            }
        }
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let request = platform.make_credential(&cdh, RP, Some(param));
        let mut port = Retapping(&mut phone, false);
        let reply = proxy.handle(7, &request, &mut port, Instant::now());
        assert_eq!(reply[0], status::OK);
        assert_eq!(phone.card.counters.wrong_pin, 0);
    }

    #[test]
    fn a_token_the_card_dropped_on_the_same_tap_is_rekeyed_once() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        phone.card.expire_token();
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let reply = phone.send(&mut proxy, &platform.get_assertion(&cdh, RP, Some(param)));
        assert_eq!(reply[0], status::OK);
        assert_eq!(phone.card.counters.tokens_issued, 2);
        assert_eq!(phone.card.counters.wrong_pin, 0);
    }

    #[test]
    fn a_silent_preflight_keeps_the_session_for_the_real_request() {
        // Browsers check for an existing credential with up=false first.
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let mut preflight = platform.get_assertion(&cdh, RP, Some(param.clone()));
        let mut map = RawMap::parse(&preflight[1..]).unwrap();
        map.set(
            5,
            &Value::Map(vec![(Value::Text("up".into()), Value::Bool(false))]),
        );
        preflight.truncate(1);
        preflight.extend(map.to_bytes());
        assert_eq!(phone.send(&mut proxy, &preflight)[0], status::OK);
        assert!(proxy.has_session());
        assert_ne!(phone.states.last(), Some(&auth::SUCCESS));
        let reply = phone.send(&mut proxy, &platform.get_assertion(&cdh, RP, Some(param)));
        assert_eq!(reply[0], status::OK);
        assert_eq!(phone.states.last(), Some(&auth::SUCCESS));
    }

    #[test]
    fn running_out_of_taps_ends_with_the_ports_status() {
        let mut phone = Phone::new(Authenticator::new(None, &[2]));
        let mut proxy = PinProxy::new();
        phone.taps_left = 0;
        phone.drop_at = vec![1];
        assert_eq!(
            phone.send(&mut proxy, &[0x04]),
            [status::USER_ACTION_TIMEOUT]
        );
        assert_eq!(phone.states.last(), Some(&auth::FAILED));
    }

    #[test]
    fn permissions_and_rp_id_reach_the_card_and_survive_a_rekey() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token =
            platform_token(&mut phone, &mut proxy, &platform, Some((0x02, Some(RP)))).unwrap();
        phone.retap();
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let reply = phone.send(
            &mut proxy,
            &platform.get_assertion(&cdh, RP, Some(param.clone())),
        );
        assert_eq!(reply[0], status::OK);
        // The real token is bound to the rpId: another rp is refused by
        // the card itself.
        let token =
            platform_token(&mut phone, &mut proxy, &platform, Some((0x02, Some(RP)))).unwrap();
        let param = Protocol::Two.authenticate(&token, &cdh);
        let reply = phone.send(
            &mut proxy,
            &platform.get_assertion(&cdh, "evil.example", Some(param)),
        );
        assert_eq!(reply, [status::PIN_AUTH_INVALID]);
    }

    #[test]
    fn credential_management_is_proxied() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[1]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, Some((0x04, None))).unwrap();
        phone.retap();
        // getCredsMetadata: {1: 1, 3: protocol, 4: param over 0x01}.
        let param = Protocol::Two.authenticate(&token, &[0x01]);
        let mut request = vec![0x0A];
        request.extend(cbor::encode(&cbor::int_map(vec![
            (1, Value::Uint(1)),
            (3, Value::Uint(2)),
            (4, Value::Bytes(param)),
        ])));
        let reply = phone.send(&mut proxy, &request);
        assert_eq!(reply[0], status::OK, "{reply:02x?}");
        assert!(proxy.has_session(), "kept for the enumeration that follows");
    }

    #[test]
    fn authenticator_config_and_large_blobs_are_proxied() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let token = platform_token(&mut phone, &mut proxy, &platform, Some((0x30, None))).unwrap();
        // toggleAlwaysUv: message = 32 x 0xFF || 0x0D || 0x02.
        let mut message = vec![0xFF; 32];
        message.extend_from_slice(&[0x0D, 0x02]);
        let mut request = vec![0x0D];
        request.extend(cbor::encode(&cbor::int_map(vec![
            (1, Value::Uint(2)),
            (3, Value::Uint(2)),
            (
                4,
                Value::Bytes(Protocol::Two.authenticate(&token, &message)),
            ),
        ])));
        assert_eq!(phone.send(&mut proxy, &request)[0], status::OK);
        // largeBlobs set at offset 0.
        let blob = vec![0x80, 1, 2, 3];
        let mut message = vec![0xFF; 32];
        message.extend_from_slice(&[0x0C, 0x00, 0, 0, 0, 0]);
        message.extend_from_slice(&Sha256::digest(&blob));
        let mut request = vec![0x0C];
        request.extend(cbor::encode(&cbor::int_map(vec![
            (2, Value::Bytes(blob)),
            (3, Value::Uint(0)),
            (4, Value::Uint(4)),
            (
                5,
                Value::Bytes(Protocol::Two.authenticate(&token, &message)),
            ),
            (6, Value::Uint(2)),
        ])));
        assert_eq!(phone.send(&mut proxy, &request)[0], status::OK);
    }

    #[test]
    fn set_pin_is_reencrypted_for_the_card() {
        let mut phone = Phone::new(Authenticator::new(None, &[1]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let mut send = |request: &[u8]| phone.send(&mut proxy, request);
        assert_eq!(platform.set_pin(&mut send, "4321"), status::OK);
        assert!(phone.card.pin_is("4321"));
    }

    #[test]
    fn change_pin_is_reencrypted_and_a_wrong_current_pin_costs_one_retry() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::One);
        let mut send = |request: &[u8]| phone.send(&mut proxy, request);
        assert_eq!(
            platform.change_pin(&mut send, "0000", "5678"),
            status::PIN_INVALID
        );
        assert_eq!(phone.card.counters.wrong_pin, 1);
        let mut send = |request: &[u8]| phone.send(&mut proxy, request);
        assert_eq!(platform.change_pin(&mut send, PIN, "5678"), status::OK);
        assert!(phone.card.pin_is("5678"));
        assert_eq!(phone.card.counters.wrong_pin, 1);
    }

    #[test]
    fn a_bad_set_pin_param_never_reaches_the_card() {
        let mut phone = Phone::new(Authenticator::new(None, &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let mut send = |request: &[u8]| phone.send(&mut proxy, request);
        let (shared, cose) = platform.agree(&mut send).unwrap();
        let new_pin_enc = shared.encrypt(&fake::padded_pin("4321"));
        let request = platform.client_pin(
            0x03,
            vec![
                (3, cose),
                (4, Value::Bytes(vec![0; 32])),
                (5, Value::Bytes(new_pin_enc)),
            ],
        );
        assert_eq!(phone.send(&mut proxy, &request), [status::PIN_AUTH_INVALID]);
        assert!(phone.card.requests.is_empty());
    }

    #[test]
    fn uv_token_requests_are_refused_cleanly() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let reply = phone.send(&mut proxy, &platform.client_pin(0x06, vec![]));
        assert_eq!(reply, [status::UNSUPPORTED_OPTION]);
    }

    #[test]
    fn get_key_agreement_with_an_unknown_protocol_is_invalid() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let mut request = vec![0x06];
        request.extend(cbor::encode(&cbor::int_map(vec![
            (1, Value::Uint(3)),
            (2, Value::Uint(2)),
        ])));
        assert_eq!(
            phone.send(&mut proxy, &request),
            [status::INVALID_PARAMETER]
        );
        assert_eq!(
            phone.send(&mut proxy, &[0x06, 0x01]),
            [status::INVALID_CBOR]
        );
    }

    #[test]
    fn the_platform_key_is_stable_across_resets() {
        // A key change under the platform would turn its next pinHashEnc
        // into garbage on the card (a lost retry).
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        let first = phone.send(&mut proxy, &platform.client_pin(0x02, vec![]));
        proxy.reset();
        proxy.on_resync(7);
        let second = phone.send(&mut proxy, &platform.client_pin(0x02, vec![]));
        assert_eq!(first, second);
    }

    #[test]
    fn auth_states_follow_the_pin_flow_without_repeats() {
        let mut phone = Phone::new(Authenticator::new(Some(PIN), &[2]));
        let mut proxy = PinProxy::new();
        let platform = Platform::new(Protocol::Two);
        phone.send(&mut proxy, &platform.client_pin(0x01, vec![]));
        let token = platform_token(&mut phone, &mut proxy, &platform, None).unwrap();
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        phone.send(&mut proxy, &platform.make_credential(&cdh, RP, Some(param)));
        assert_eq!(
            phone.states,
            [auth::WORKING, auth::PIN, auth::WORKING, auth::SUCCESS],
            "getRetries, getKeyAgreement, getPinToken, makeCredential"
        );
        assert_eq!(pin_hash(PIN).len(), 16);
    }
}
