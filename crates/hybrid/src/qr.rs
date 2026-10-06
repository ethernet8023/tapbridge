//! The QR code contents of a caBLEv2 QR-initiated transaction.
//!
//! CTAP 2.2 §11.5.1: the QR shows `FIDO:/` followed by the digit encoding of
//! a canonical-CBOR map:
//!
//! - key 0: 33-byte P-256 X9.62 compressed public key (client identity)
//! - key 1: 16-byte random QR secret
//! - key 2: number of assigned tunnel-server domains (u8)
//! - key 3 (optional): epoch seconds
//! - key 4 (optional): state-assisted-capable boolean
//! - key 5 (optional): "ga"/"mc" hint
//!
//! Digit encoding: seven-byte chunks read little-endian, each rendered as a
//! 17-digit zero-padded base-10 number; a trailing partial chunk of 1..6
//! bytes uses 3/5/8/10/13/15 digits respectively.
//!
//! The phone (authenticator) parses this after scanning the Mac's QR code.

/// QR URI prefix. Single foreslash, per the spec.
pub const QR_PREFIX: &str = "FIDO:/";

/// Errors while parsing QR contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrError {
    /// Missing `FIDO:/` prefix.
    BadPrefix,
    /// Digits after the prefix are empty or not all ASCII digits.
    BadDigits,
    /// Digit stream length is impossible for any CBOR byte length
    /// (not a whole number of 17-digit chunks plus a valid partial size).
    BadChunking,
    /// Decoded CBOR is not a well-formed map in the expected shape.
    BadCbor,
}

impl std::fmt::Display for QrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::BadPrefix => "QR contents must start with FIDO:/",
            Self::BadDigits => "QR data after FIDO:/ must be non-empty ASCII digits",
            Self::BadChunking => "QR digit stream is not a valid chunking",
            Self::BadCbor => "QR CBOR payload is malformed",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for QrError {}

/// Parsed QR contents.
#[derive(Clone, PartialEq, Eq)]
pub struct QrContents {
    /// Client platform identity public key, X9.62 compressed P-256.
    pub client_public_key: [u8; 33],
    /// The 16-byte QR secret: parent of the EID key, tunnel ID and PSK.
    pub qr_secret: [u8; 16],
    /// Number of assigned tunnel-server domains this client knows.
    pub assigned_domain_count: u8,
    /// Optional epoch seconds.
    pub epoch: Option<u64>,
    /// Optional: the client can perform state-assisted transactions.
    pub state_assisted: Option<bool>,
    /// Optional "ga" (getAssertion) / "mc" (makeCredential) hint.
    pub transaction_hint: Option<String>,
}

impl std::fmt::Debug for QrContents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The QR secret derives every key on this exchange; never log it.
        f.debug_struct("QrContents")
            .field("client_public_key", &"REDACTED")
            .field("qr_secret", &"REDACTED")
            .field("assigned_domain_count", &self.assigned_domain_count)
            .field("epoch", &self.epoch)
            .field("state_assisted", &self.state_assisted)
            .field("transaction_hint", &self.transaction_hint)
            .finish()
    }
}

impl QrContents {
    /// Parse a scanned QR string.
    pub fn parse(s: &str) -> Result<Self, QrError> {
        let rest = s.strip_prefix(QR_PREFIX).ok_or(QrError::BadPrefix)?;
        // The prefix is exactly "FIDO:/" — a second slash or any non-digit
        // right after it means the prefix itself is wrong (spec: single
        // foreslash, then digits only).
        let Some(first_digit) = rest.as_bytes().first() else {
            return Err(QrError::BadDigits);
        };
        if !first_digit.is_ascii_digit() {
            return Err(QrError::BadPrefix);
        }
        let digits = rest;
        let cbor = digit_decode(digits)?;
        let mut parser = CborReader {
            data: &cbor,
            pos: 0,
        };
        let contents = Self::from_cbor(&mut parser)?;
        parser.end().map_err(|_| QrError::BadCbor)?;
        Ok(contents)
    }

    fn from_cbor(p: &mut CborReader<'_>) -> Result<Self, QrError> {
        let bad = |_| QrError::BadCbor;
        let (major, entries) = p.head().map_err(bad)?;
        if major != 5 || entries == 0 || entries > 64 {
            // A map with a bounded entry count. (The spec's example uses 6,
            // GREASE may add one; any map with the required keys is fine.)
            return Err(QrError::BadCbor);
        }
        let mut contents = Self {
            client_public_key: [0; 33],
            qr_secret: [0; 16],
            assigned_domain_count: 0,
            epoch: None,
            state_assisted: None,
            transaction_hint: None,
        };
        let mut seen_required = 0u8;
        for _ in 0..entries {
            let (kmajor, karg) = p.head().map_err(bad)?;
            if kmajor != 0 {
                return Err(QrError::BadCbor); // integer keys only
            }
            match karg {
                0 => {
                    contents.client_public_key =
                        p.byte_string_exact(33).map_err(bad)?.try_into().unwrap();
                    seen_required |= 1;
                }
                1 => {
                    contents.qr_secret = p.byte_string_exact(16).map_err(bad)?.try_into().unwrap();
                    seen_required |= 2;
                }
                2 => {
                    let n = p.uint().map_err(bad)?;
                    if n > u8::MAX as u64 {
                        return Err(QrError::BadCbor);
                    }
                    contents.assigned_domain_count = n as u8;
                    seen_required |= 4;
                }
                3 => {
                    contents.epoch = Some(p.uint().map_err(bad)?);
                }
                4 => {
                    let (maj, a) = p.head().map_err(bad)?;
                    if maj == 7 && a == 21 {
                        contents.state_assisted = Some(true);
                    } else if maj == 7 && a == 20 {
                        contents.state_assisted = Some(false);
                    } else {
                        return Err(QrError::BadCbor);
                    }
                }
                5 => {
                    let raw = p.text_string_exact(2).map_err(bad)?;
                    let hint = std::str::from_utf8(raw).map_err(|_| QrError::BadCbor)?;
                    if hint != "ga" && hint != "mc" {
                        return Err(QrError::BadCbor);
                    }
                    contents.transaction_hint = Some(hint.to_string());
                }
                _ => {
                    // GREASE / future keys: skip the value.
                    p.skip_value().map_err(bad)?;
                }
            }
        }
        if seen_required != 7 {
            return Err(QrError::BadCbor);
        }
        Ok(contents)
    }
}

/// Decode the digit stream back into bytes. Inverse of the spec's
/// `digitEncode`.
pub fn digit_decode(digits: &str) -> Result<Vec<u8>, QrError> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(QrError::BadDigits);
    }
    // Possible partial-chunk digit counts for 1..=6 leftover bytes.
    const PARTIAL: [usize; 7] = [0, 3, 5, 8, 10, 13, 15];
    let len = digits.len();
    // Find a split point: full 17-digit chunks plus a valid partial count.
    // Partial digit counts 3,5,8,10,13,15 mod 17 = 3,5,8,10,13,15.
    let partial = {
        let r = len % 17;
        if r == 0 {
            0
        } else if PARTIAL.contains(&r) {
            r
        } else {
            return Err(QrError::BadChunking);
        }
    };
    let full = len - partial;
    let mut out = Vec::with_capacity(full / 17 * 7 + 7);
    for i in (0..full).step_by(17) {
        let chunk = &digits[i..i + 17];
        let v: u64 = chunk.parse().map_err(|_| QrError::BadDigits)?;
        // A 17-digit chunk must fit in 7 bytes (2^56); larger values are not
        // producible by the encoder and would corrupt the stream.
        let le = v.to_le_bytes();
        if le[7] != 0 {
            return Err(QrError::BadDigits);
        }
        out.extend_from_slice(&le[..7]);
    }
    if partial > 0 {
        let rem = digits[full..]
            .parse::<u64>()
            .map_err(|_| QrError::BadDigits)?;
        let bytes = PARTIAL.iter().position(|&p| p == partial).unwrap();
        // 7-byte little-endian encoding of rem has exactly `bytes` significant
        // bytes; verify no overflow beyond.
        let le = rem.to_le_bytes();
        if le[bytes..].iter().any(|&b| b != 0) {
            return Err(QrError::BadDigits);
        }
        out.extend_from_slice(&le[..bytes]);
    }
    Ok(out)
}

/// Encode bytes into the digit stream (authenticator-side tests and the
/// hybrid-client simulator use this to render QRs).
pub fn digit_encode(data: &[u8]) -> String {
    const CHUNK: usize = 7;
    const DIGITS: usize = 17;
    const PARTIAL: [usize; 7] = [0, 3, 5, 8, 10, 13, 15];
    let zeros = "0".repeat(DIGITS);
    let mut out = String::new();
    let mut d = data;
    while d.len() >= CHUNK {
        let mut le = [0u8; 8];
        le[..CHUNK].copy_from_slice(&d[..CHUNK]);
        let v = u64::from_le_bytes(le);
        let s = v.to_string();
        out.push_str(&zeros[..DIGITS - s.len()]);
        out.push_str(&s);
        d = &d[CHUNK..];
    }
    if !d.is_empty() {
        let digits = PARTIAL[d.len()];
        let mut le = [0u8; 8];
        le[..d.len()].copy_from_slice(d);
        let v = u64::from_le_bytes(le);
        let s = v.to_string();
        out.push_str(&zeros[..digits - s.len()]);
        out.push_str(&s);
    }
    out
}

/// Minimal strict canonical-CBOR reader for the QR map (integer keys, byte
/// strings, small uints, booleans, short strings).
struct CborReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> CborReader<'a> {
    fn head(&mut self) -> Result<(u8, u64), ()> {
        let Some(&initial) = self.data.get(self.pos) else {
            return Err(());
        };
        self.pos += 1;
        let major = initial >> 5;
        let info = initial & 0x1F;
        let arg = match info {
            0..=23 => u64::from(info),
            24 => {
                let v = u64::from(*self.data.get(self.pos).ok_or(())?);
                self.pos += 1;
                v
            }
            25 => {
                let b: [u8; 2] = self
                    .data
                    .get(self.pos..self.pos + 2)
                    .ok_or(())?
                    .try_into()
                    .unwrap();
                self.pos += 2;
                u64::from(u16::from_be_bytes(b))
            }
            26 => {
                let b: [u8; 4] = self
                    .data
                    .get(self.pos..self.pos + 4)
                    .ok_or(())?
                    .try_into()
                    .unwrap();
                self.pos += 4;
                u64::from(u32::from_be_bytes(b))
            }
            27 => {
                let b: [u8; 8] = self
                    .data
                    .get(self.pos..self.pos + 8)
                    .ok_or(())?
                    .try_into()
                    .unwrap();
                self.pos += 8;
                u64::from_be_bytes(b)
            }
            31 => return Err(()), // indefinite lengths are not canonical
            _ => return Err(()),  // 28..30 reserved
        };
        Ok((major, arg))
    }

    /// Read a byte string of exactly `expect` bytes (strict length).
    fn byte_string_exact(&mut self, expect: usize) -> Result<&'a [u8], ()> {
        let (major, arg) = self.head()?;
        if major != 2 || arg as usize != expect {
            return Err(());
        }
        self.take(expect)
    }

    /// Read a text string of exactly `expect` bytes.
    fn text_string_exact(&mut self, expect: usize) -> Result<&'a [u8], ()> {
        let (major, arg) = self.head()?;
        if major != 3 || arg as usize != expect {
            return Err(());
        }
        self.take(expect)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ()> {
        let slice = self.data.get(self.pos..self.pos + n).ok_or(())?;
        self.pos += n;
        Ok(slice)
    }

    fn uint(&mut self) -> Result<u64, ()> {
        let (major, arg) = self.head()?;
        if major != 0 {
            return Err(());
        }
        Ok(arg)
    }

    fn skip_value(&mut self) -> Result<(), ()> {
        let (major, arg) = self.head()?;
        match major {
            0 | 1 | 7 => Ok(()),
            2 | 3 => {
                let n = usize::try_from(arg).map_err(|_| ())?;
                self.take(n).map(|_| ())
            }
            4 => {
                for _ in 0..arg {
                    self.skip_value()?;
                }
                Ok(())
            }
            5 => {
                for _ in 0..arg {
                    self.skip_value()?;
                }
                Ok(())
            }
            6 => {
                let n = usize::try_from(arg).map_err(|_| ())?;
                self.take(n).map(|_| ())
            }
            _ => Err(()),
        }
    }

    fn end(&self) -> Result<(), ()> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // Independent python vector: 6-element canonical CBOR map, keys
    // 0,1,2,3,4,5, digit-encoded under FIDO:/.
    const QR_STRING: &str = "FIDO:/005640586143991420254210104918118704520152650222346064982042512635050847625585230466400564050807300127025421010491811870057950297627316259112589604029186109321447142660";
    const QR_CBOR_HEX: &str = "a6005821020102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f200150000102030405060708090a0b0c0d0e0f0202031a499602d204f505626d63";
    const QR_SECRET_HEX: &str = "000102030405060708090a0b0c0d0e0f";
    const PUB33_HEX: &str = "020102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

    #[test]
    fn digit_decode_independent_vector() {
        let digits = QR_STRING.strip_prefix("FIDO:/").unwrap();
        let cbor = digit_decode(digits).unwrap();
        assert_eq!(hex(&cbor), QR_CBOR_HEX);
    }

    #[test]
    fn digit_encode_roundtrip_various_lengths() {
        for len in 1..=40 {
            let data: Vec<u8> = (0..len as u8)
                .map(|i| i.wrapping_mul(7).wrapping_add(3))
                .collect();
            let digits = digit_encode(&data);
            assert_eq!(digit_decode(&digits).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn digit_decode_strict() {
        assert_eq!(digit_decode("").unwrap_err(), QrError::BadDigits);
        assert_eq!(digit_decode("12x4").unwrap_err(), QrError::BadDigits);
        // 4 digits: not a valid chunking (not 0/3/5/8/10/13/15 mod 17)
        assert_eq!(digit_decode("1234").unwrap_err(), QrError::BadChunking);
        // 34 digits: mod 17 = 0 -> valid chunking but value overflows 7 bytes
        let big = "9".repeat(34);
        assert!(digit_decode(&big).is_err());
    }

    #[test]
    fn qr_parse_independent_vector() {
        let parsed = QrContents::parse(QR_STRING).unwrap();
        assert_eq!(hex(&parsed.client_public_key), PUB33_HEX);
        assert_eq!(hex(&parsed.qr_secret), QR_SECRET_HEX);
        assert_eq!(parsed.assigned_domain_count, 2);
        assert_eq!(parsed.epoch, Some(1234567890));
        assert_eq!(parsed.state_assisted, Some(true));
        assert_eq!(parsed.transaction_hint.as_deref(), Some("mc"));
    }

    #[test]
    fn qr_parse_debug_redacts_secrets() {
        let parsed = QrContents::parse(QR_STRING).unwrap();
        let dbg = format!("{parsed:?}");
        assert!(dbg.contains("REDACTED"));
        assert!(!dbg.contains(QR_SECRET_HEX));
    }

    #[test]
    fn qr_parse_strict() {
        // Wrong prefix
        assert_eq!(
            QrContents::parse("FIDO://abc").unwrap_err(),
            QrError::BadPrefix
        );
        assert_eq!(
            QrContents::parse("fido:/123").unwrap_err(),
            QrError::BadPrefix
        );
        // Valid digits, truncated CBOR
        let digits = QR_STRING.strip_prefix("FIDO:/").unwrap();
        assert!(QrContents::parse(&format!("FIDO:/{}", &digits[..digits.len() - 20])).is_err());
        // Trailing garbage in CBOR
        let mut cbor = unhex(QR_CBOR_HEX);
        cbor.push(0);
        let encoded = format!("FIDO:/{}", digit_encode(&cbor));
        assert!(QrContents::parse(&encoded).is_err());
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}
