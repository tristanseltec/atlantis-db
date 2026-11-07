//! Order-preserving ("memcomparable") byte encoding.
//!
//! Encoded byte strings compare (with plain `memcmp`) in the same order as
//! the values they encode, so ordered key-value scans see records, index
//! entries and composite keys in logical order.
//!
//! Rules:
//! - strings and byte strings: every `0x00` byte is escaped as `0x00 0xFF`,
//!   and the string ends with `0x00 0x01`, so a prefix sorts first and no
//!   encoded string is a prefix of another;
//! - `i64`: big-endian with the sign bit flipped;
//! - `f64`: big-endian IEEE bits, all bits flipped for negatives and the sign
//!   bit flipped for positives;
//! - values are tagged, so different types sort by tag (see [`tag`]).

use atlantis_core::{AtlantisError, RecordId, RecordKey, Ulid, Uuid, Value};

/// Type tags. Values of different types sort in this order.
pub mod tag {
    pub const END: u8 = 0x00;
    pub const NULL: u8 = 0x01;
    pub const FALSE: u8 = 0x02;
    pub const TRUE: u8 = 0x03;
    pub const NUMBER: u8 = 0x04;
    pub const STRING: u8 = 0x05;
    pub const BYTES: u8 = 0x06;
    pub const DATETIME: u8 = 0x07;
    pub const DURATION: u8 = 0x08;
    pub const UUID: u8 = 0x09;
    pub const ARRAY: u8 = 0x0A;
    pub const OBJECT: u8 = 0x0B;
    pub const RECORD: u8 = 0x0C;

    // Record key kinds.
    pub const KEY_INTEGER: u8 = 0x10;
    pub const KEY_STRING: u8 = 0x11;
    pub const KEY_UUID: u8 = 0x12;
    pub const KEY_ULID: u8 = 0x13;
    pub const KEY_ARRAY: u8 = 0x14;

    // Number sub-tags: equal magnitudes order floats before integers.
    pub const NUM_FLOAT: u8 = 0x01;
    pub const NUM_INT: u8 = 0x02;
}

/// The largest `f64` not greater than `i`. Integers sort after floats with
/// the same prefix and before the next larger float, so mixed integer/float
/// keys stay in numeric order even where `i as f64` would round up.
fn f64_floor_of(i: i64) -> f64 {
    let f = i as f64;
    if (f as i128) <= i as i128 {
        return f;
    }
    // `f` rounded up; step to the previous representable value. Rounding up
    // only happens for |i| >= 2^53, so `f` is never zero here.
    let bits = f.to_bits();
    f64::from_bits(if f > 0.0 { bits - 1 } else { bits + 1 })
}

fn err(msg: impl Into<String>) -> AtlantisError {
    AtlantisError::Encoding(msg.into())
}

/// Appends order-preserving encodings to a byte buffer.
#[derive(Debug, Default, Clone)]
pub struct KeyWriter {
    buf: Vec<u8>,
}

impl KeyWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts from an existing prefix.
    pub fn from_prefix(prefix: &[u8]) -> Self {
        KeyWriter {
            buf: prefix.to_vec(),
        }
    }

    /// Appends raw bytes without escaping (for fixed literal markers).
    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(bytes);
        self
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    /// Escaped, terminated byte string.
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        for &b in v {
            self.buf.push(b);
            if b == 0 {
                self.buf.push(0xFF);
            }
        }
        self.buf.extend_from_slice(&[0x00, 0x01]);
        self
    }

    pub fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }

    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.buf
            .extend_from_slice(&((v as u64) ^ (1 << 63)).to_be_bytes());
        self
    }

    pub fn f64(&mut self, v: f64) -> &mut Self {
        let bits = v.to_bits();
        let ordered = if bits >> 63 == 1 {
            !bits
        } else {
            bits | (1 << 63)
        };
        self.buf.extend_from_slice(&ordered.to_be_bytes());
        self
    }

    pub fn u128(&mut self, v: u128) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Encodes a value. `Vector` and `Geometry` values cannot appear in keys.
    pub fn value(&mut self, v: &Value) -> Result<&mut Self, AtlantisError> {
        match v {
            Value::Null => {
                self.u8(tag::NULL);
            }
            Value::Bool(false) => {
                self.u8(tag::FALSE);
            }
            Value::Bool(true) => {
                self.u8(tag::TRUE);
            }
            Value::Integer(i) => {
                self.u8(tag::NUMBER)
                    .f64(f64_floor_of(*i))
                    .u8(tag::NUM_INT)
                    .i64(*i);
            }
            Value::Float(f) => {
                self.u8(tag::NUMBER).f64(*f).u8(tag::NUM_FLOAT);
            }
            Value::String(s) => {
                self.u8(tag::STRING).str(s);
            }
            Value::Bytes(b) => {
                self.u8(tag::BYTES).bytes(b);
            }
            Value::Datetime(d) => {
                self.u8(tag::DATETIME).i64(d.as_nanos());
            }
            Value::Duration(d) => {
                self.u8(tag::DURATION).i64(d.as_nanos());
            }
            Value::Uuid(u) => {
                self.u8(tag::UUID).raw(u.as_bytes());
            }
            Value::Array(items) => {
                self.u8(tag::ARRAY);
                for item in items {
                    self.value(item)?;
                }
                self.u8(tag::END);
            }
            Value::Object(obj) => {
                self.u8(tag::OBJECT);
                for (k, item) in obj {
                    self.u8(tag::STRING).str(k);
                    self.value(item)?;
                }
                self.u8(tag::END);
            }
            Value::RecordId(id) => {
                self.u8(tag::RECORD).record_id(id)?;
            }
            Value::Vector(_) | Value::Geometry(_) => {
                return Err(err(format!(
                    "a {} value cannot be part of a key",
                    v.type_name()
                )))
            }
        }
        Ok(self)
    }

    /// Encodes a record key, tagged by kind.
    pub fn record_key(&mut self, key: &RecordKey) -> Result<&mut Self, AtlantisError> {
        match key {
            RecordKey::Integer(i) => {
                self.u8(tag::KEY_INTEGER).i64(*i);
            }
            RecordKey::String(s) => {
                self.u8(tag::KEY_STRING).str(s);
            }
            RecordKey::Uuid(u) => {
                self.u8(tag::KEY_UUID).raw(u.as_bytes());
            }
            RecordKey::Ulid(u) => {
                self.u8(tag::KEY_ULID).u128(u.0);
            }
            RecordKey::Array(items) => {
                self.u8(tag::KEY_ARRAY);
                for item in items {
                    self.value(item)?;
                }
                self.u8(tag::END);
            }
        }
        Ok(self)
    }

    /// Encodes `table` followed by the key.
    pub fn record_id(&mut self, id: &RecordId) -> Result<&mut Self, AtlantisError> {
        self.str(&id.table).record_key(&id.key)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }
}

/// Reads encodings written by [`KeyWriter`].
#[derive(Debug)]
pub struct KeyReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> KeyReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        KeyReader { buf, pos: 0 }
    }

    /// Bytes not yet consumed.
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], AtlantisError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
        let end = end.ok_or_else(|| err("unexpected end of key"))?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Consumes an exact literal marker.
    pub fn expect(&mut self, literal: &[u8]) -> Result<(), AtlantisError> {
        if self.take(literal.len())? == literal {
            Ok(())
        } else {
            Err(err("unexpected key segment"))
        }
    }

    pub fn u8(&mut self) -> Result<u8, AtlantisError> {
        Ok(self.take(1)?[0])
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    pub fn bytes(&mut self) -> Result<Vec<u8>, AtlantisError> {
        let mut out = Vec::new();
        loop {
            match self.u8()? {
                0x00 => match self.u8()? {
                    0xFF => out.push(0x00),
                    0x01 => return Ok(out),
                    _ => return Err(err("bad escape in key string")),
                },
                b => out.push(b),
            }
        }
    }

    pub fn str(&mut self) -> Result<String, AtlantisError> {
        String::from_utf8(self.bytes()?).map_err(|_| err("key string is not utf-8"))
    }

    pub fn i64(&mut self) -> Result<i64, AtlantisError> {
        let b: [u8; 8] = self.take(8)?.try_into().expect("8 bytes");
        Ok((u64::from_be_bytes(b) ^ (1 << 63)) as i64)
    }

    pub fn f64(&mut self) -> Result<f64, AtlantisError> {
        let b: [u8; 8] = self.take(8)?.try_into().expect("8 bytes");
        let ordered = u64::from_be_bytes(b);
        let bits = if ordered >> 63 == 1 {
            ordered & !(1 << 63)
        } else {
            !ordered
        };
        Ok(f64::from_bits(bits))
    }

    pub fn u128(&mut self) -> Result<u128, AtlantisError> {
        let b: [u8; 16] = self.take(16)?.try_into().expect("16 bytes");
        Ok(u128::from_be_bytes(b))
    }

    fn uuid(&mut self) -> Result<Uuid, AtlantisError> {
        Ok(Uuid(self.take(16)?.try_into().expect("16 bytes")))
    }

    pub fn value(&mut self) -> Result<Value, AtlantisError> {
        Ok(match self.u8()? {
            tag::NULL => Value::Null,
            tag::FALSE => Value::Bool(false),
            tag::TRUE => Value::Bool(true),
            tag::NUMBER => {
                let f = self.f64()?;
                match self.u8()? {
                    tag::NUM_INT => Value::Integer(self.i64()?),
                    tag::NUM_FLOAT => Value::Float(f),
                    _ => return Err(err("bad number sub-tag")),
                }
            }
            tag::STRING => Value::String(self.str()?),
            tag::BYTES => Value::Bytes(self.bytes()?),
            tag::DATETIME => Value::Datetime(atlantis_core::Datetime(self.i64()?)),
            tag::DURATION => Value::Duration(atlantis_core::Duration(self.i64()?)),
            tag::UUID => Value::Uuid(self.uuid()?),
            tag::ARRAY => {
                let mut items = Vec::new();
                while self.peek() != Some(tag::END) {
                    items.push(self.value()?);
                }
                self.u8()?;
                Value::Array(items)
            }
            tag::OBJECT => {
                let mut obj = atlantis_core::Object::new();
                while self.peek() != Some(tag::END) {
                    self.expect(&[tag::STRING])?;
                    let k = self.str()?;
                    obj.insert(k, self.value()?);
                }
                self.u8()?;
                Value::Object(obj)
            }
            tag::RECORD => Value::RecordId(self.record_id()?),
            t => return Err(err(format!("unknown value tag {t:#04x}"))),
        })
    }

    pub fn record_key(&mut self) -> Result<RecordKey, AtlantisError> {
        Ok(match self.u8()? {
            tag::KEY_INTEGER => RecordKey::Integer(self.i64()?),
            tag::KEY_STRING => RecordKey::String(self.str()?),
            tag::KEY_UUID => RecordKey::Uuid(self.uuid()?),
            tag::KEY_ULID => RecordKey::Ulid(Ulid(self.u128()?)),
            tag::KEY_ARRAY => {
                let mut items = Vec::new();
                while self.peek() != Some(tag::END) {
                    items.push(self.value()?);
                }
                self.u8()?;
                RecordKey::Array(items)
            }
            t => return Err(err(format!("unknown record key tag {t:#04x}"))),
        })
    }

    pub fn record_id(&mut self) -> Result<RecordId, AtlantisError> {
        let table = self.str()?;
        let key = self.record_key()?;
        Ok(RecordId { table, key })
    }
}

/// The smallest byte string greater than every string starting with
/// `prefix`, used as the exclusive end of a prefix scan. Returns `None` if
/// no such string exists (the prefix is all `0xFF`).
pub fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < 0xFF {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn enc(v: &Value) -> Vec<u8> {
        KeyWriter::new().value(v).unwrap().finish()
    }

    #[test]
    fn values_roundtrip() {
        let mut obj = atlantis_core::Object::new();
        obj.insert("a".into(), Value::Integer(1));
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Integer(-42),
            Value::Float(2.5),
            Value::String("nul\0byte".into()),
            Value::Bytes(vec![0, 0xFF, 1]),
            Value::Datetime(atlantis_core::Datetime(-1)),
            Value::Uuid(Uuid::new_v4()),
            Value::Array(vec![Value::from("x"), Value::Integer(3)]),
            Value::Object(obj),
            Value::RecordId("Person:alice".parse().unwrap()),
        ];
        for v in values {
            let bytes = enc(&v);
            let mut r = KeyReader::new(&bytes);
            assert_eq!(r.value().unwrap(), v);
            assert!(r.is_empty());
        }
    }

    #[test]
    fn rejects_vectors_in_keys() {
        assert!(KeyWriter::new().value(&Value::Vector(vec![1.0])).is_err());
    }

    #[test]
    fn mixed_numbers_order_numerically() {
        let ordered = [
            Value::Float(f64::NEG_INFINITY),
            Value::Integer(i64::MIN),
            Value::Float(-1.5),
            Value::Integer(-1),
            Value::Float(0.0),
            Value::Integer(0),
            Value::Float(0.5),
            Value::Integer(1),
            Value::Integer(i64::MAX),
            Value::Float(f64::INFINITY),
        ];
        for pair in ordered.windows(2) {
            assert!(enc(&pair[0]) < enc(&pair[1]), "{} < {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn type_order_and_array_prefixes() {
        assert!(enc(&Value::Null) < enc(&Value::Bool(false)));
        assert!(enc(&Value::Integer(i64::MAX)) < enc(&Value::String(String::new())));
        let short = Value::Array(vec![Value::from("a")]);
        let long = Value::Array(vec![Value::from("a"), Value::Null]);
        assert!(enc(&short) < enc(&long));
        assert!(enc(&Value::from("ab")) < enc(&Value::from("ab\0")));
        assert!(enc(&Value::from("ab\0")) < enc(&Value::from("abc")));
    }

    #[test]
    fn prefix_end_works() {
        assert_eq!(prefix_end(b"ab"), Some(b"ac".to_vec()));
        assert_eq!(prefix_end(&[0x01, 0xFF]), Some(vec![0x02]));
        assert_eq!(prefix_end(&[0xFF, 0xFF]), None);
    }

    proptest! {
        #[test]
        fn strings_sort_like_bytes(a in ".*", b in ".*") {
            prop_assert_eq!(a.as_bytes().cmp(b.as_bytes()), enc(&Value::from(a.as_str())).cmp(&enc(&Value::from(b.as_str()))));
        }

        #[test]
        fn integers_sort_numerically(a: i64, b: i64) {
            prop_assert_eq!(a.cmp(&b), enc(&Value::Integer(a)).cmp(&enc(&Value::Integer(b))));
        }

        #[test]
        fn floats_sort_numerically(a in -1e300f64..1e300, b in -1e300f64..1e300) {
            prop_assume!(a != b);
            prop_assert_eq!(a < b, enc(&Value::Float(a)) < enc(&Value::Float(b)));
        }

        #[test]
        fn record_keys_roundtrip(i: i64, s in "[a-z0-9 ]{1,12}") {
            for key in [RecordKey::Integer(i), RecordKey::String(s.clone()), RecordKey::Array(vec![Value::from(s.as_str()), Value::Integer(i)])] {
                let bytes = KeyWriter::new().record_key(&key).unwrap().finish();
                prop_assert_eq!(KeyReader::new(&bytes).record_key().unwrap(), key);
            }
        }
    }

    #[test]
    fn huge_integers_and_floats_interleave_correctly() {
        let two53 = 1i64 << 53;
        let ordered = [
            Value::Float(-(2f64.powi(63))),
            Value::Integer(i64::MIN + 1),
            Value::Float(two53 as f64 + 2.0),
            Value::Integer(two53 + 3),
            Value::Float(two53 as f64 + 4.0),
            Value::Integer(i64::MAX),
            Value::Float(2f64.powi(63)),
        ];
        for pair in ordered.windows(2) {
            assert!(enc(&pair[0]) < enc(&pair[1]), "{} < {}", pair[0], pair[1]);
        }
        for i in [i64::MIN, i64::MIN + 1, -(two53 + 3), two53 + 3, i64::MAX] {
            let bytes = enc(&Value::Integer(i));
            assert_eq!(KeyReader::new(&bytes).value().unwrap(), Value::Integer(i));
        }
    }
}
