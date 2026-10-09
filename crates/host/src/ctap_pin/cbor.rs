//! The little CBOR the PIN proxy needs (RFC 8949, CTAP2 canonical form):
//! decode/encode of definite-length items, plus splitting a map into its
//! raw entries so a request can be rewritten (one or two keys replaced)
//! while every other byte the platform sent goes to the card untouched.
//!
//! Hand-rolled on purpose: CTAP2 forbids indefinite lengths, floats and
//! tags in the messages we touch, so a small strict decoder is easier to
//! audit than a general crate. Anything unexpected is an error, never a
//! guess.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Uint(u64),
    /// A negative integer `-1 - n`.
    Nint(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Bool(bool),
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

impl Value {
    pub fn int(n: i64) -> Self {
        if n >= 0 {
            Self::Uint(n as u64)
        } else {
            Self::Nint((-1 - n) as u64)
        }
    }

    /// The value of integer key `key` in a map.
    pub fn get(&self, key: i64) -> Option<&Value> {
        let Self::Map(entries) = self else {
            return None;
        };
        let key = Value::int(key);
        entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::Uint(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// Encodes `value` (shortest-form lengths; map entries in the given
/// order, so callers list keys canonically).
pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

fn write(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Uint(n) => head(0, *n, out),
        Value::Nint(n) => head(1, *n, out),
        Value::Bytes(bytes) => {
            head(2, bytes.len() as u64, out);
            out.extend_from_slice(bytes);
        }
        Value::Text(text) => {
            head(3, text.len() as u64, out);
            out.extend_from_slice(text.as_bytes());
        }
        Value::Array(items) => {
            head(4, items.len() as u64, out);
            for item in items {
                write(item, out);
            }
        }
        Value::Map(entries) => {
            head(5, entries.len() as u64, out);
            for (key, value) in entries {
                write(key, out);
                write(value, out);
            }
        }
        Value::Bool(false) => out.push(0xF4),
        Value::Bool(true) => out.push(0xF5),
        Value::Null => out.push(0xF6),
    }
}

/// An item header: major type plus shortest-form argument.
pub fn head(major: u8, n: u64, out: &mut Vec<u8>) {
    let major = major << 5;
    if n < 24 {
        out.push(major | n as u8);
    } else if n <= 0xFF {
        out.extend_from_slice(&[major | 24, n as u8]);
    } else if n <= 0xFFFF {
        out.push(major | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= 0xFFFF_FFFF {
        out.push(major | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(major | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

/// Decodes exactly one item filling all of `bytes`.
pub fn decode(bytes: &[u8]) -> Result<Value, Malformed> {
    let mut reader = Reader { bytes, pos: 0 };
    let value = reader.value(0)?;
    if reader.pos != bytes.len() {
        return Err(Malformed);
    }
    Ok(value)
}

/// Nesting bound: CTAP messages are a few levels deep.
const MAX_DEPTH: usize = 16;

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Malformed> {
        let end = self.pos.checked_add(n).ok_or(Malformed)?;
        let slice = self.bytes.get(self.pos..end).ok_or(Malformed)?;
        self.pos = end;
        Ok(slice)
    }

    /// Major type and argument of the next header.
    fn head(&mut self) -> Result<(u8, u8, u64), Malformed> {
        let initial = self.take(1)?[0];
        let (major, info) = (initial >> 5, initial & 0x1F);
        let n = match info {
            0..=23 => u64::from(info),
            24 => u64::from(self.take(1)?[0]),
            25 => u64::from(u16::from_be_bytes(self.take(2)?.try_into().unwrap())),
            26 => u64::from(u32::from_be_bytes(self.take(4)?.try_into().unwrap())),
            27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            // 28..=30 reserved, 31 indefinite: not allowed in CTAP2.
            _ => return Err(Malformed),
        };
        Ok((major, info, n))
    }

    fn len(n: u64) -> Result<usize, Malformed> {
        usize::try_from(n).map_err(|_| Malformed)
    }

    fn value(&mut self, depth: usize) -> Result<Value, Malformed> {
        if depth > MAX_DEPTH {
            return Err(Malformed);
        }
        let (major, info, n) = self.head()?;
        Ok(match major {
            0 => Value::Uint(n),
            1 => Value::Nint(n),
            2 => Value::Bytes(self.take(Self::len(n)?)?.to_vec()),
            3 => Value::Text(
                String::from_utf8(self.take(Self::len(n)?)?.to_vec()).map_err(|_| Malformed)?,
            ),
            4 => {
                let count = Self::len(n)?;
                if count > self.bytes.len() {
                    return Err(Malformed);
                }
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.value(depth + 1)?);
                }
                Value::Array(items)
            }
            5 => {
                let count = Self::len(n)?;
                if count > self.bytes.len() {
                    return Err(Malformed);
                }
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let key = self.value(depth + 1)?;
                    let value = self.value(depth + 1)?;
                    entries.push((key, value));
                }
                Value::Map(entries)
            }
            7 if info == 20 => Value::Bool(false),
            7 if info == 21 => Value::Bool(true),
            7 if info == 22 => Value::Null,
            _ => return Err(Malformed),
        })
    }

    /// Skips one item of any kind (tags and simple values included), so
    /// raw splitting never needs to understand the item.
    fn skip(&mut self, depth: usize) -> Result<(), Malformed> {
        if depth > MAX_DEPTH {
            return Err(Malformed);
        }
        let (major, _, n) = self.head()?;
        match major {
            0 | 1 | 7 => {}
            2 | 3 => {
                self.take(Self::len(n)?)?;
            }
            4 => {
                for _ in 0..n {
                    self.skip(depth + 1)?;
                }
            }
            5 => {
                for _ in 0..n {
                    self.skip(depth + 1)?;
                    self.skip(depth + 1)?;
                }
            }
            6 => self.skip(depth + 1)?,
            _ => return Err(Malformed),
        }
        Ok(())
    }
}

/// A map split into entries: the decoded key and the raw bytes of key and
/// value, exactly as received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMap {
    pub entries: Vec<RawEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEntry {
    pub key: Value,
    pub raw_key: Vec<u8>,
    pub raw_value: Vec<u8>,
}

impl RawMap {
    /// Splits a top-level map (the whole of `bytes`).
    pub fn parse(bytes: &[u8]) -> Result<Self, Malformed> {
        let mut reader = Reader { bytes, pos: 0 };
        let (major, _, n) = reader.head()?;
        if major != 5 || Reader::len(n)? > bytes.len() {
            return Err(Malformed);
        }
        let mut entries = Vec::new();
        for _ in 0..n {
            let start = reader.pos;
            let key = reader.value(1)?;
            let raw_key = bytes[start..reader.pos].to_vec();
            let start = reader.pos;
            reader.skip(1)?;
            entries.push(RawEntry {
                key,
                raw_key,
                raw_value: bytes[start..reader.pos].to_vec(),
            });
        }
        if reader.pos != bytes.len() {
            return Err(Malformed);
        }
        Ok(Self { entries })
    }

    /// The raw value bytes of integer key `key`.
    pub fn raw(&self, key: i64) -> Option<&[u8]> {
        let key = Value::int(key);
        self.entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.raw_value.as_slice())
    }

    /// The raw value bytes of any key.
    pub fn raw_for(&self, key: &Value) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|entry| entry.key == *key)
            .map(|entry| entry.raw_value.as_slice())
    }

    /// Replaces integer key `key`, or inserts it before the first larger
    /// integer key (CTAP2 canonical order for integer-keyed maps).
    pub fn set(&mut self, key: i64, value: &Value) {
        if self.replace(key, value) {
            return;
        }
        let rank = |k: &Value| match k {
            Value::Uint(n) => Some(i128::from(*n)),
            Value::Nint(n) => Some(-1 - i128::from(*n)),
            _ => None,
        };
        // Canonical CBOR sorts by encoded length then bytes: positive
        // integers before negative ones, smaller first.
        let canonical = |k: i128| if k >= 0 { (0, k) } else { (1, -k) };
        let wanted = canonical(i128::from(key));
        let at = self
            .entries
            .iter()
            .position(|entry| rank(&entry.key).is_none_or(|k| canonical(k) > wanted))
            .unwrap_or(self.entries.len());
        let key = Value::int(key);
        self.entries.insert(
            at,
            RawEntry {
                raw_key: encode(&key),
                key,
                raw_value: encode(value),
            },
        );
    }

    /// The decoded value of integer key `key` (None when absent or
    /// malformed).
    pub fn get(&self, key: i64) -> Option<Value> {
        self.raw(key).and_then(|raw| decode(raw).ok())
    }

    /// Replaces the value of an existing integer key; false when absent.
    pub fn replace(&mut self, key: i64, value: &Value) -> bool {
        let key = Value::int(key);
        match self.entries.iter_mut().find(|entry| entry.key == key) {
            Some(entry) => {
                entry.raw_value = encode(value);
                true
            }
            None => false,
        }
    }

    /// Re-encodes the map: entries in their original order, untouched
    /// entries byte for byte.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        head(5, self.entries.len() as u64, &mut out);
        for entry in &self.entries {
            out.extend_from_slice(&entry.raw_key);
            out.extend_from_slice(&entry.raw_value);
        }
        out
    }
}

/// Builds an integer-keyed map (keys in the given order).
pub fn int_map(entries: Vec<(i64, Value)>) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(key, value)| (Value::int(key), value))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_use_the_shortest_head() {
        assert_eq!(encode(&Value::Uint(23)), [0x17]);
        assert_eq!(encode(&Value::Uint(24)), [0x18, 24]);
        assert_eq!(encode(&Value::Uint(256)), [0x19, 1, 0]);
        assert_eq!(encode(&Value::Uint(65_536)), [0x1A, 0, 1, 0, 0]);
        assert_eq!(
            encode(&Value::Uint(1 << 32)),
            [0x1B, 0, 0, 0, 1, 0, 0, 0, 0]
        );
        assert_eq!(encode(&Value::int(-1)), [0x20]);
        assert_eq!(encode(&Value::int(-25)), [0x38, 24]);
    }

    #[test]
    fn a_cose_key_round_trips() {
        let key = int_map(vec![
            (1, Value::Uint(2)),
            (3, Value::int(-25)),
            (-1, Value::Uint(1)),
            (-2, Value::Bytes(vec![0xAA; 32])),
            (-3, Value::Bytes(vec![0xBB; 32])),
        ]);
        let bytes = encode(&key);
        assert_eq!(&bytes[..6], &[0xA5, 0x01, 0x02, 0x03, 0x38, 0x18]);
        assert_eq!(decode(&bytes).unwrap(), key);
        assert_eq!(
            decode(&bytes).unwrap().get(-2).unwrap().as_bytes(),
            Some(&[0xAA; 32][..])
        );
    }

    #[test]
    fn text_arrays_and_simple_values_round_trip() {
        let value = Value::Array(vec![
            Value::Text("FIDO_2_1".into()),
            Value::Bool(true),
            Value::Bool(false),
            Value::Null,
            Value::Map(vec![(Value::Text("rk".into()), Value::Bool(true))]),
        ]);
        assert_eq!(decode(&encode(&value)).unwrap(), value);
    }

    #[test]
    fn truncated_indefinite_and_trailing_input_are_malformed() {
        assert_eq!(decode(&[0x58, 0x05, 1, 2]), Err(Malformed));
        assert_eq!(decode(&[0x5F, 0x41, 0x00, 0xFF]), Err(Malformed));
        assert_eq!(decode(&[0x01, 0x02]), Err(Malformed));
        assert_eq!(decode(&[0xFA, 0, 0, 0, 0]), Err(Malformed), "floats");
        assert_eq!(decode(&[]), Err(Malformed));
    }

    #[test]
    fn raw_map_replaces_one_key_and_keeps_the_rest_byte_for_byte() {
        // {1: h'0102', 2: {"id": "x"}, 8: h'', 9: 1}
        let mut original = vec![0xA4, 0x01, 0x42, 0x01, 0x02, 0x02, 0xA1, 0x62];
        original.extend_from_slice(b"id");
        original.extend_from_slice(&[0x61, b'x', 0x08, 0x40, 0x09, 0x01]);
        let mut map = RawMap::parse(&original).unwrap();
        assert_eq!(map.to_bytes(), original, "identity");
        assert_eq!(map.get(8), Some(Value::Bytes(vec![])));
        assert_eq!(map.raw(2), Some(&[0xA1, 0x62, b'i', b'd', 0x61, b'x'][..]));
        assert!(map.replace(8, &Value::Bytes(vec![0x55; 16])));
        assert!(map.replace(9, &Value::Uint(2)));
        assert!(!map.replace(7, &Value::Null));
        let rewritten = decode(&map.to_bytes()).unwrap();
        assert_eq!(rewritten.get(8), Some(&Value::Bytes(vec![0x55; 16])));
        assert_eq!(rewritten.get(9), Some(&Value::Uint(2)));
        assert_eq!(rewritten.get(1), Some(&Value::Bytes(vec![1, 2])));
    }

    #[test]
    fn raw_map_set_inserts_missing_keys_in_canonical_order() {
        // {1: 1, 3: 3, 0x14: 20}
        let mut map = RawMap::parse(&[0xA3, 0x01, 0x01, 0x03, 0x03, 0x14, 0x14]).unwrap();
        map.set(6, &Value::Uint(6));
        map.set(0x20, &Value::Uint(0));
        map.set(3, &Value::Uint(4));
        assert_eq!(
            map.to_bytes(),
            [
                0xA5, 0x01, 0x01, 0x03, 0x04, 0x06, 0x06, 0x14, 0x14, 0x18, 0x20, 0x00
            ]
        );
        assert_eq!(map.raw_for(&Value::Uint(6)), Some(&[0x06][..]));
    }

    #[test]
    fn raw_map_skips_values_it_cannot_decode() {
        // {1: 1(1000) (a tag), 2: 3.0 as half float}
        let bytes = [0xA2, 0x01, 0xC1, 0x19, 0x03, 0xE8, 0x02, 0xF9, 0x42, 0x00];
        let map = RawMap::parse(&bytes).unwrap();
        assert_eq!(map.entries.len(), 2);
        assert_eq!(map.raw(2), Some(&[0xF9, 0x42, 0x00][..]));
        assert_eq!(map.to_bytes(), bytes);
    }

    #[test]
    fn raw_map_refuses_non_maps_and_huge_counts() {
        assert_eq!(RawMap::parse(&[0x80]), Err(Malformed));
        assert_eq!(
            RawMap::parse(&[0xBB, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
            Err(Malformed)
        );
    }
}
