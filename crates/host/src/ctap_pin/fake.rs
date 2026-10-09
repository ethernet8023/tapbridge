//! A software FIDO2 authenticator for tests: real clientPIN protocol 1
//! and 2 crypto, a PIN retry counter, and the power-up behaviour that
//! broke the live flow (every NFC tap regenerates the key-agreement key
//! and invalidates the pinUvAuthToken). Also speaks ISO 7816 (SELECT,
//! NFCCTAP_MSG with chaining, GET RESPONSE) so the desktop relay can be
//! tested against it, and offers the platform half of the PIN protocol
//! ([`Platform`]) the way macOS drives it.
//!
//! Test-only code (`cfg(test)` or the `test-support` feature).

use super::cbor::{self, RawMap, Value};
use super::protocol::{KeyAgreement, Protocol, SharedSecret, random};
use sha2::{Digest, Sha256};

pub const OK: u8 = 0x00;
pub const PIN_INVALID: u8 = 0x31;
pub const PIN_BLOCKED: u8 = 0x32;
pub const PIN_AUTH_INVALID: u8 = 0x33;
pub const PIN_AUTH_BLOCKED: u8 = 0x34;
pub const PIN_NOT_SET: u8 = 0x35;
pub const PUAT_REQUIRED: u8 = 0x36;
pub const MISSING_PARAMETER: u8 = 0x14;
pub const INVALID_PARAMETER: u8 = 0x02;

/// LEFT(SHA-256(pin), 16): what the platform sends and the card stores.
pub fn pin_hash(pin: &str) -> [u8; 16] {
    Sha256::digest(pin.as_bytes())[..16].try_into().unwrap()
}

/// What the card saw, for assertions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counters {
    /// getPinToken / changePIN attempts whose decrypted pinHash did not
    /// match (each one decrements the retry counter).
    pub wrong_pin: usize,
    /// Successful getPinToken calls.
    pub tokens_issued: usize,
    /// makeCredential / getAssertion that returned OK.
    pub signatures: usize,
}

pub struct Authenticator {
    protocols: Vec<u64>,
    pin_hash: Option<[u8; 16]>,
    pub retries: u8,
    consecutive_failures: u8,
    key: KeyAgreement,
    token: Option<Token>,
    pub counters: Counters,
    /// CTAP command names in order (ClientPIN as `clientPIN:<sub>`).
    pub log: Vec<String>,
    /// Every request after SELECT, raw (command byte first).
    pub requests: Vec<Vec<u8>>,
    selected: bool,
    chain: Vec<u8>,
    pending: Vec<u8>,
}

struct Token {
    bytes: [u8; 32],
    permissions: u8,
    rp_id: Option<String>,
}

const PERM_MC: u8 = 0x01;
const PERM_GA: u8 = 0x02;
const PERM_CM: u8 = 0x04;
const PERM_LBW: u8 = 0x10;
const PERM_ACFG: u8 = 0x20;

impl Authenticator {
    /// A card with `pin` set (or none) supporting `protocols`.
    pub fn new(pin: Option<&str>, protocols: &[u64]) -> Self {
        Self {
            protocols: protocols.to_vec(),
            pin_hash: pin.map(pin_hash),
            retries: 8,
            consecutive_failures: 0,
            key: KeyAgreement::generate(),
            token: None,
            counters: Counters::default(),
            log: Vec::new(),
            requests: Vec::new(),
            selected: false,
            chain: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// The card left the field and came back: new key agreement key, no
    /// token, applet deselected (CTAP 2.1 §6.5.5.1 power-up).
    pub fn power_cycle(&mut self) {
        self.key = KeyAgreement::generate();
        self.token = None;
        self.consecutive_failures = 0;
        self.selected = false;
        self.chain.clear();
        self.pending.clear();
    }

    pub fn pin_is(&self, pin: &str) -> bool {
        self.pin_hash == Some(pin_hash(pin))
    }

    /// One ISO 7816 APDU (data || SW).
    pub fn apdu(&mut self, apdu: &[u8]) -> Vec<u8> {
        if apdu.len() < 4 {
            return vec![0x67, 0x00];
        }
        let (cla, ins) = (apdu[0], apdu[1]);
        let data = if apdu.len() > 5 {
            let len = usize::from(apdu[4]);
            apdu.get(5..5 + len).unwrap_or(&[])
        } else {
            &[]
        };
        match ins {
            0xA4 => {
                if data == super::super::ctap_nfc::FIDO_AID {
                    self.selected = true;
                    self.log.push("SELECT".into());
                    let mut reply = b"FIDO_2_0".to_vec();
                    reply.extend_from_slice(&[0x90, 0x00]);
                    reply
                } else {
                    vec![0x6A, 0x82]
                }
            }
            0x10 if !self.selected => vec![0x69, 0x85],
            0x10 => {
                self.chain.extend_from_slice(data);
                if cla & 0x10 != 0 {
                    return vec![0x90, 0x00];
                }
                let request = std::mem::take(&mut self.chain);
                self.pending = self.ctap(&request);
                self.next_chunk()
            }
            0xC0 => self.next_chunk(),
            _ => vec![0x6D, 0x00],
        }
    }

    fn next_chunk(&mut self) -> Vec<u8> {
        let take = self.pending.len().min(256);
        let mut reply: Vec<u8> = self.pending.drain(..take).collect();
        match self.pending.len() {
            0 => reply.extend_from_slice(&[0x90, 0x00]),
            left => reply.extend_from_slice(&[0x61, left.min(256) as u8]),
        }
        reply
    }

    /// One CTAP request (command || CBOR) to its reply (status || CBOR).
    pub fn ctap(&mut self, request: &[u8]) -> Vec<u8> {
        self.requests.push(request.to_vec());
        let Some((&command, body)) = request.split_first() else {
            return vec![0x01];
        };
        let params = if body.is_empty() {
            Some(RawMap { entries: vec![] })
        } else {
            RawMap::parse(body).ok()
        };
        let Some(params) = params else {
            return vec![0x12]; // CTAP2_ERR_INVALID_CBOR
        };
        let result = match command {
            0x04 => {
                self.log.push("getInfo".into());
                Ok(self.info())
            }
            0x06 => self.client_pin(&params),
            0x01 => {
                self.log.push("makeCredential".into());
                self.signing(&params, 1, 8, 9, PERM_MC, 2)
            }
            0x02 => {
                self.log.push("getAssertion".into());
                self.signing(&params, 2, 6, 7, PERM_GA, 1)
            }
            0x0A | 0x41 => {
                self.log.push("credentialManagement".into());
                let message = sub_and_params(&params, 1, 2);
                self.authorized(&params, 4, 3, PERM_CM, &message, None)
                    .map(|()| cbor::int_map(vec![(1, Value::Uint(0)), (2, Value::Uint(25))]))
            }
            0x0D => {
                self.log.push("authenticatorConfig".into());
                let mut message = vec![0xFF; 32];
                message.push(0x0D);
                message.extend(sub_and_params(&params, 1, 2));
                self.authorized(&params, 4, 3, PERM_ACFG, &message, None)
                    .map(|()| Value::Map(vec![]))
            }
            0x0C => {
                self.log.push("largeBlobs".into());
                self.large_blobs(&params)
            }
            _ => Err(0x01),
        };
        match result {
            Ok(Value::Map(entries)) if entries.is_empty() => vec![OK],
            Ok(value) => {
                let mut reply = vec![OK];
                reply.extend(cbor::encode(&value));
                reply
            }
            Err(status) => vec![status],
        }
    }

    fn info(&self) -> Value {
        cbor::int_map(vec![
            (
                1,
                Value::Array(vec![
                    Value::Text("FIDO_2_0".into()),
                    Value::Text("FIDO_2_1".into()),
                ]),
            ),
            (2, Value::Array(vec![Value::Text("hmac-secret".into())])),
            (3, Value::Bytes(vec![0x42; 16])),
            (
                4,
                Value::Map(vec![
                    (Value::Text("rk".into()), Value::Bool(true)),
                    (
                        Value::Text("clientPin".into()),
                        Value::Bool(self.pin_hash.is_some()),
                    ),
                    (Value::Text("pinUvAuthToken".into()), Value::Bool(true)),
                ]),
            ),
            (5, Value::Uint(1200)),
            (
                6,
                Value::Array(self.protocols.iter().map(|p| Value::Uint(*p)).collect()),
            ),
            (0x14, Value::Uint(25)),
        ])
    }

    fn protocol(&self, params: &RawMap, key: i64) -> Result<Protocol, u8> {
        let number = params
            .get(key)
            .ok_or(MISSING_PARAMETER)?
            .as_uint()
            .ok_or(INVALID_PARAMETER)?;
        if !self.protocols.contains(&number) {
            return Err(INVALID_PARAMETER);
        }
        Protocol::from_number(number).ok_or(INVALID_PARAMETER)
    }

    fn shared(&self, params: &RawMap, protocol: Protocol) -> Result<SharedSecret, u8> {
        let peer = params.get(3).ok_or(MISSING_PARAMETER)?;
        self.key
            .shared_secret(protocol, &peer)
            .ok_or(INVALID_PARAMETER)
    }

    fn client_pin(&mut self, params: &RawMap) -> Result<Value, u8> {
        let sub = params
            .get(2)
            .and_then(|v| v.as_uint())
            .ok_or(MISSING_PARAMETER)?;
        self.log.push(format!("clientPIN:{sub}"));
        match sub {
            0x01 => Ok(cbor::int_map(vec![(
                3,
                Value::Uint(u64::from(self.retries)),
            )])),
            0x02 => {
                self.protocol(params, 1)?;
                Ok(cbor::int_map(vec![(1, self.key.cose_public())]))
            }
            0x03 => self.set_pin(params),
            0x04 => self.change_pin(params),
            0x05 | 0x09 => self.get_token(params, sub),
            _ => Err(0x3E),
        }
    }

    /// The decrypted new PIN (64-byte padded block), checked for policy.
    fn new_pin(shared: &SharedSecret, new_pin_enc: &[u8]) -> Result<[u8; 16], u8> {
        let padded = shared.decrypt(new_pin_enc).ok_or(INVALID_PARAMETER)?;
        if padded.len() != 64 {
            return Err(INVALID_PARAMETER);
        }
        let pin: Vec<u8> = padded.iter().copied().take_while(|b| *b != 0).collect();
        if pin.len() < 4 {
            return Err(0x37); // PIN_POLICY_VIOLATION
        }
        Ok(Sha256::digest(&pin)[..16].try_into().unwrap())
    }

    fn set_pin(&mut self, params: &RawMap) -> Result<Value, u8> {
        let protocol = self.protocol(params, 1)?;
        if self.pin_hash.is_some() {
            return Err(0x30); // NOT_ALLOWED
        }
        let shared = self.shared(params, protocol)?;
        let new_pin_enc = bytes(params, 5)?;
        if !shared.verify(&new_pin_enc, &bytes(params, 4)?) {
            return Err(PIN_AUTH_INVALID);
        }
        self.pin_hash = Some(Self::new_pin(&shared, &new_pin_enc)?);
        self.retries = 8;
        Ok(Value::Map(vec![]))
    }

    fn change_pin(&mut self, params: &RawMap) -> Result<Value, u8> {
        let protocol = self.protocol(params, 1)?;
        self.guard_retries()?;
        let shared = self.shared(params, protocol)?;
        let new_pin_enc = bytes(params, 5)?;
        let pin_hash_enc = bytes(params, 6)?;
        let mut message = new_pin_enc.clone();
        message.extend_from_slice(&pin_hash_enc);
        if !shared.verify(&message, &bytes(params, 4)?) {
            return Err(PIN_AUTH_INVALID);
        }
        self.check_pin(&shared, &pin_hash_enc)?;
        self.pin_hash = Some(Self::new_pin(&shared, &new_pin_enc)?);
        self.token = None;
        Ok(Value::Map(vec![]))
    }

    fn guard_retries(&self) -> Result<(), u8> {
        if self.pin_hash.is_none() {
            return Err(PIN_NOT_SET);
        }
        if self.retries == 0 {
            return Err(PIN_BLOCKED);
        }
        if self.consecutive_failures >= 3 {
            return Err(PIN_AUTH_BLOCKED);
        }
        Ok(())
    }

    /// Decrypts and compares pinHashEnc; a mismatch costs a retry and
    /// regenerates the key agreement key (CTAP 2.1 §6.5.5.5).
    fn check_pin(&mut self, shared: &SharedSecret, pin_hash_enc: &[u8]) -> Result<(), u8> {
        let decrypted = shared.decrypt(pin_hash_enc).ok_or(INVALID_PARAMETER)?;
        if decrypted.len() != 16 || Some(&decrypted[..]) != self.pin_hash.as_ref().map(|h| &h[..]) {
            self.retries -= 1;
            self.consecutive_failures += 1;
            self.counters.wrong_pin += 1;
            self.key = KeyAgreement::generate();
            return Err(if self.retries == 0 {
                PIN_BLOCKED
            } else if self.consecutive_failures >= 3 {
                PIN_AUTH_BLOCKED
            } else {
                PIN_INVALID
            });
        }
        self.retries = 8;
        self.consecutive_failures = 0;
        Ok(())
    }

    fn get_token(&mut self, params: &RawMap, sub: u64) -> Result<Value, u8> {
        let protocol = self.protocol(params, 1)?;
        let permissions = if sub == 0x09 {
            let permissions = params
                .get(9)
                .ok_or(MISSING_PARAMETER)?
                .as_uint()
                .ok_or(INVALID_PARAMETER)?;
            if permissions == 0 {
                return Err(INVALID_PARAMETER);
            }
            permissions as u8
        } else {
            PERM_MC | PERM_GA
        };
        self.guard_retries()?;
        let shared = self.shared(params, protocol)?;
        self.check_pin(&shared, &bytes(params, 6)?)?;
        let mut token = [0u8; 32];
        random(&mut token);
        self.token = Some(Token {
            bytes: token,
            permissions,
            rp_id: params
                .get(0x0A)
                .and_then(|v| v.as_text().map(str::to_owned)),
        });
        self.counters.tokens_issued += 1;
        Ok(cbor::int_map(vec![(
            2,
            Value::Bytes(shared.encrypt(&token)),
        )]))
    }

    /// Checks pinUvAuthParam (key `param_key`) over `message` with the
    /// current token and permission `permission`.
    fn authorized(
        &mut self,
        params: &RawMap,
        param_key: i64,
        protocol_key: i64,
        permission: u8,
        message: &[u8],
        rp_id: Option<&str>,
    ) -> Result<(), u8> {
        let param = bytes(params, param_key)?;
        let protocol = self.protocol(params, protocol_key)?;
        let token = self.token.as_ref().ok_or(PIN_AUTH_INVALID)?;
        if !protocol.verify(&token.bytes, message, &param)
            || token.permissions & permission == 0
            || token
                .rp_id
                .as_deref()
                .is_some_and(|bound| Some(bound) != rp_id)
        {
            return Err(PIN_AUTH_INVALID);
        }
        Ok(())
    }

    /// makeCredential / getAssertion: the PIN rules plus a dummy result.
    fn signing(
        &mut self,
        params: &RawMap,
        cdh_key: i64,
        param_key: i64,
        protocol_key: i64,
        permission: u8,
        rp_key: i64,
    ) -> Result<Value, u8> {
        let cdh = bytes(params, cdh_key)?;
        let rp_id = match params.get(rp_key) {
            Some(Value::Text(id)) => Some(id),
            Some(map @ Value::Map(_)) => map.get_text("id").map(str::to_owned),
            _ => None,
        };
        match params.get(param_key) {
            Some(Value::Bytes(param)) if param.is_empty() => {
                return Err(if self.pin_hash.is_some() {
                    PIN_INVALID
                } else {
                    PIN_NOT_SET
                });
            }
            Some(_) => {
                self.authorized(
                    params,
                    param_key,
                    protocol_key,
                    permission,
                    &cdh,
                    rp_id.as_deref(),
                )?;
                // CTAP 2.1: the permissions are spent (lbw survives).
                if let Some(token) = self.token.as_mut() {
                    token.permissions &= PERM_LBW;
                }
            }
            None if self.pin_hash.is_some() => return Err(PUAT_REQUIRED),
            None => {}
        }
        self.counters.signatures += 1;
        let mut auth_data = Sha256::digest(rp_id.unwrap_or_default().as_bytes()).to_vec();
        auth_data.extend_from_slice(&[0x05, 0, 0, 0, 1]);
        Ok(cbor::int_map(vec![
            (1, Value::Text("none".into())),
            (2, Value::Bytes(auth_data)),
            (3, Value::Map(vec![])),
        ]))
    }

    fn large_blobs(&mut self, params: &RawMap) -> Result<Value, u8> {
        let offset = params
            .get(3)
            .and_then(|v| v.as_uint())
            .ok_or(MISSING_PARAMETER)?;
        match params.get(2) {
            Some(Value::Bytes(set)) => {
                let mut message = vec![0xFF; 32];
                message.extend_from_slice(&[0x0C, 0x00]);
                message.extend_from_slice(&(offset as u32).to_le_bytes());
                message.extend_from_slice(&Sha256::digest(&set));
                self.authorized(params, 5, 6, PERM_LBW, &message, None)?;
                Ok(Value::Map(vec![]))
            }
            _ => Ok(cbor::int_map(vec![(1, Value::Bytes(vec![0x80]))])),
        }
    }
}

fn bytes(params: &RawMap, key: i64) -> Result<Vec<u8>, u8> {
    match params.get(key) {
        Some(Value::Bytes(bytes)) => Ok(bytes),
        Some(_) => Err(INVALID_PARAMETER),
        None => Err(MISSING_PARAMETER),
    }
}

/// subCommand || subCommandParams (raw), the message credential
/// management and authenticatorConfig authenticate.
pub fn sub_and_params(params: &RawMap, sub_key: i64, params_key: i64) -> Vec<u8> {
    let mut message = params.raw(sub_key).unwrap_or_default().to_vec();
    message.extend_from_slice(params.raw(params_key).unwrap_or_default());
    message
}

impl Value {
    /// The value of text key `key` in a map.
    pub fn get_text(&self, key: &str) -> Option<&str> {
        let Value::Map(entries) = self else {
            return None;
        };
        entries
            .iter()
            .find(|(k, _)| k.as_text() == Some(key))
            .and_then(|(_, v)| v.as_text())
    }
}

/// The platform (macOS) half of clientPIN, driving whatever `send`
/// reaches (the proxy in tests).
pub struct Platform {
    pub protocol: Protocol,
}

impl Platform {
    pub fn new(protocol: Protocol) -> Self {
        Self { protocol }
    }

    /// ClientPIN request bytes.
    pub fn client_pin(&self, sub: u64, rest: Vec<(i64, Value)>) -> Vec<u8> {
        let mut entries = vec![
            (1, Value::Uint(self.protocol.number())),
            (2, Value::Uint(sub)),
        ];
        entries.extend(rest);
        let mut request = vec![0x06];
        request.extend(cbor::encode(&cbor::int_map(entries)));
        request
    }

    /// getKeyAgreement, then our own ephemeral key: the shared secret and
    /// the COSE key to send with the next command.
    pub fn agree(
        &self,
        send: &mut dyn FnMut(&[u8]) -> Vec<u8>,
    ) -> Result<(SharedSecret, Value), u8> {
        let reply = send(&self.client_pin(0x02, vec![]));
        if reply[0] != OK {
            return Err(reply[0]);
        }
        let peer = cbor::decode(&reply[1..])
            .ok()
            .and_then(|v| v.get(1).cloned())
            .ok_or(0xFF)?;
        let mine = KeyAgreement::generate();
        let shared = mine.shared_secret(self.protocol, &peer).ok_or(0xFE)?;
        Ok((shared, mine.cose_public()))
    }

    /// getPinToken (permissions None) or the permissions variant; the
    /// decrypted token.
    pub fn token(
        &self,
        send: &mut dyn FnMut(&[u8]) -> Vec<u8>,
        pin: &str,
        permissions: Option<(u8, Option<&str>)>,
    ) -> Result<Vec<u8>, u8> {
        let (shared, cose) = self.agree(send)?;
        let request = self.token_request(&shared, cose, pin, permissions);
        self.open_token(&shared, &send(&request))
    }

    pub fn token_request(
        &self,
        shared: &SharedSecret,
        cose: Value,
        pin: &str,
        permissions: Option<(u8, Option<&str>)>,
    ) -> Vec<u8> {
        let mut rest = vec![(3, cose), (6, Value::Bytes(shared.encrypt(&pin_hash(pin))))];
        let sub = match permissions {
            None => 0x05,
            Some((permissions, rp_id)) => {
                rest.push((9, Value::Uint(u64::from(permissions))));
                if let Some(rp_id) = rp_id {
                    rest.push((0x0A, Value::Text(rp_id.into())));
                }
                0x09
            }
        };
        self.client_pin(sub, rest)
    }

    pub fn open_token(&self, shared: &SharedSecret, reply: &[u8]) -> Result<Vec<u8>, u8> {
        if reply[0] != OK {
            return Err(reply[0]);
        }
        let encrypted = cbor::decode(&reply[1..])
            .ok()
            .and_then(|v| v.get(2).and_then(|t| t.as_bytes().map(<[u8]>::to_vec)))
            .ok_or(0xFF)?;
        shared.decrypt(&encrypted).map(|t| t.to_vec()).ok_or(0xFE)
    }

    /// makeCredential for `rp_id`; `param` None leaves keys 8/9 out,
    /// Some(empty) is the probe.
    pub fn make_credential(&self, cdh: &[u8; 32], rp_id: &str, param: Option<Vec<u8>>) -> Vec<u8> {
        let mut entries = vec![
            (1, Value::Bytes(cdh.to_vec())),
            (
                2,
                Value::Map(vec![(Value::Text("id".into()), Value::Text(rp_id.into()))]),
            ),
            (
                3,
                Value::Map(vec![(
                    Value::Text("id".into()),
                    Value::Bytes(vec![1, 2, 3]),
                )]),
            ),
            (
                4,
                Value::Array(vec![Value::Map(vec![
                    (Value::Text("alg".into()), Value::int(-7)),
                    (Value::Text("type".into()), Value::Text("public-key".into())),
                ])]),
            ),
        ];
        if let Some(param) = param {
            entries.push((8, Value::Bytes(param)));
            entries.push((9, Value::Uint(self.protocol.number())));
        }
        let mut request = vec![0x01];
        request.extend(cbor::encode(&cbor::int_map(entries)));
        request
    }

    pub fn get_assertion(&self, cdh: &[u8; 32], rp_id: &str, param: Option<Vec<u8>>) -> Vec<u8> {
        let mut entries = vec![
            (1, Value::Text(rp_id.into())),
            (2, Value::Bytes(cdh.to_vec())),
        ];
        if let Some(param) = param {
            entries.push((6, Value::Bytes(param)));
            entries.push((7, Value::Uint(self.protocol.number())));
        }
        let mut request = vec![0x02];
        request.extend(cbor::encode(&cbor::int_map(entries)));
        request
    }

    /// setPIN (no current PIN).
    pub fn set_pin(&self, send: &mut dyn FnMut(&[u8]) -> Vec<u8>, pin: &str) -> u8 {
        let (shared, cose) = match self.agree(send) {
            Ok(agreed) => agreed,
            Err(status) => return status,
        };
        let new_pin_enc = shared.encrypt(&padded_pin(pin));
        let param = shared.authenticate(&new_pin_enc);
        send(&self.client_pin(
            0x03,
            vec![
                (3, cose),
                (4, Value::Bytes(param)),
                (5, Value::Bytes(new_pin_enc)),
            ],
        ))[0]
    }

    /// changePIN from `old` to `new`.
    pub fn change_pin(&self, send: &mut dyn FnMut(&[u8]) -> Vec<u8>, old: &str, new: &str) -> u8 {
        let (shared, cose) = match self.agree(send) {
            Ok(agreed) => agreed,
            Err(status) => return status,
        };
        let new_pin_enc = shared.encrypt(&padded_pin(new));
        let pin_hash_enc = shared.encrypt(&pin_hash(old));
        let mut message = new_pin_enc.clone();
        message.extend_from_slice(&pin_hash_enc);
        send(&self.client_pin(
            0x04,
            vec![
                (3, cose),
                (4, Value::Bytes(shared.authenticate(&message))),
                (5, Value::Bytes(new_pin_enc)),
                (6, Value::Bytes(pin_hash_enc)),
            ],
        ))[0]
    }
}

/// A PIN zero-padded to 64 bytes (newPinEnc plaintext).
pub fn padded_pin(pin: &str) -> [u8; 64] {
    let mut padded = [0u8; 64];
    padded[..pin.len()].copy_from_slice(pin.as_bytes());
    padded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(card: &mut Authenticator) -> impl FnMut(&[u8]) -> Vec<u8> + '_ {
        move |request| card.ctap(request)
    }

    #[test]
    fn the_fake_card_issues_a_token_that_signs_a_make_credential() {
        for protocol in [Protocol::One, Protocol::Two] {
            let mut card = Authenticator::new(Some("1234"), &[1, 2]);
            let platform = Platform::new(protocol);
            let token = platform
                .token(&mut direct(&mut card), "1234", None)
                .unwrap();
            let cdh = [7u8; 32];
            let param = protocol.authenticate(&token, &cdh);
            let reply = card.ctap(&platform.make_credential(&cdh, "example.com", Some(param)));
            assert_eq!(reply[0], OK, "{protocol:?}");
            assert_eq!(card.counters.signatures, 1);
        }
    }

    #[test]
    fn the_fake_card_counts_wrong_pins_and_forgets_tokens_on_power_up() {
        let mut card = Authenticator::new(Some("1234"), &[2]);
        let platform = Platform::new(Protocol::Two);
        assert_eq!(
            platform.token(&mut direct(&mut card), "0000", None),
            Err(PIN_INVALID)
        );
        assert_eq!((card.retries, card.counters.wrong_pin), (7, 1));
        let token = platform
            .token(&mut direct(&mut card), "1234", None)
            .unwrap();
        assert_eq!(card.retries, 8);
        card.power_cycle();
        let cdh = [1u8; 32];
        let param = Protocol::Two.authenticate(&token, &cdh);
        let reply = card.ctap(&platform.get_assertion(&cdh, "example.com", Some(param)));
        assert_eq!(reply, [PIN_AUTH_INVALID]);
    }

    #[test]
    fn a_pin_hash_encrypted_to_a_stale_key_counts_as_a_wrong_pin() {
        // The live bug: getKeyAgreement, re-tap, getPinToken.
        let mut card = Authenticator::new(Some("1234"), &[2]);
        let platform = Platform::new(Protocol::Two);
        let (shared, cose) = platform.agree(&mut direct(&mut card)).unwrap();
        card.power_cycle();
        let reply = card.ctap(&platform.token_request(&shared, cose, "1234", None));
        assert_eq!(reply, [PIN_INVALID]);
        assert_eq!(card.counters.wrong_pin, 1);
    }

    #[test]
    fn the_fake_card_speaks_nfc_with_chaining_and_get_response() {
        let mut card = Authenticator::new(None, &[1]);
        let reply = crate::ctap_nfc::relay(&[0x04], |apdu| Ok::<_, ()>(card.apdu(&apdu))).unwrap();
        let info = cbor::decode(&reply[1..]).unwrap();
        assert_eq!(info.get(5), Some(&Value::Uint(1200)));
        // A 300-byte getAssertion chains; the empty-param probe answers.
        let platform = Platform::new(Protocol::One);
        let long_rp = "a".repeat(260);
        let request = platform.get_assertion(&[0; 32], &long_rp, Some(vec![]));
        assert!(request.len() > 255);
        let reply = crate::ctap_nfc::relay(&request, |apdu| Ok::<_, ()>(card.apdu(&apdu))).unwrap();
        assert_eq!(reply, [PIN_NOT_SET]);
    }
}
