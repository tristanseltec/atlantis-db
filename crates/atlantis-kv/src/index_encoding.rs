//! Canonical, order-preserving encoding of values in secondary index keys.
//!
//! [`KeyWriter::value`](crate::encoding::KeyWriter::value) keeps integers
//! and floats apart (`1` and `1.0` encode differently), which is right for
//! record keys but wrong for an index: Cypher treats `1 = 1.0` as true, so
//! both must land on the same index key. This encoding is canonical for the
//! values an index stores:
//!
//! - a number is `NUMBER`, the big-endian ordered bits of a float, then
//!   `EXACT` when that float is the number exactly (every float, and every
//!   integer with |i| ≤ 2^53), or `INEXACT` followed by the `i64` for the
//!   few larger integers no float can hold. `-0.0` encodes as `0.0`.
//! - strings, bytes, datetimes, durations, UUIDs, booleans and record ids
//!   (except ids with array keys) use their [`KeyWriter`] form;
//! - arrays encode element by element (whole-value index entries only).
//!
//! `null`, NaN, objects, vectors, geometries and arrays holding any of them
//! are not indexable: [`index_value`] returns `None` and the record gets no
//! entry for that value.
//!
//! Encoded values are self-delimiting, so composite keys concatenate them.
//! In a composite entry a non-null value that is not indexable is written
//! as [`OPAQUE`], so that every node with all fields non-null has an entry.

use atlantis_core::{RecordKey, Value};

use crate::encoding::{prefix_end, tag, KeyWriter};

/// Number sub-tags.
const EXACT: u8 = 0x01;
const INEXACT: u8 = 0x02;

/// Integers up to this magnitude are exactly representable as `f64`.
pub const MAX_EXACT_INT: i64 = 1 << 53;

/// The largest `f64` not greater than `i`.
fn f64_floor_of(i: i64) -> f64 {
    let f = i as f64;
    if (f as i128) <= i as i128 {
        return f;
    }
    f.next_down()
}

fn ordered_f64(w: &mut KeyWriter, v: f64) {
    // Normalize -0.0 so that it and 0.0 share a key, as they compare equal.
    w.f64(if v == 0.0 { 0.0 } else { v });
}

fn write_number_int(w: &mut KeyWriter, i: i64) {
    w.u8(tag::NUMBER);
    if i.unsigned_abs() <= MAX_EXACT_INT as u64 {
        ordered_f64(w, i as f64);
        w.u8(EXACT);
    } else {
        ordered_f64(w, f64_floor_of(i));
        w.u8(INEXACT).i64(i);
    }
}

fn write(w: &mut KeyWriter, v: &Value) -> bool {
    match v {
        Value::Null | Value::Object(_) | Value::Vector(_) | Value::Geometry(_) => return false,
        Value::Float(f) if f.is_nan() => return false,
        Value::Bool(false) => {
            w.u8(tag::FALSE);
        }
        Value::Bool(true) => {
            w.u8(tag::TRUE);
        }
        Value::Integer(i) => write_number_int(w, *i),
        Value::Float(f) => {
            w.u8(tag::NUMBER);
            ordered_f64(w, *f);
            w.u8(EXACT);
        }
        Value::String(s) => {
            w.u8(tag::STRING).str(s);
        }
        Value::Bytes(b) => {
            w.u8(tag::BYTES).bytes(b);
        }
        Value::Datetime(d) => {
            w.u8(tag::DATETIME).i64(d.as_nanos());
        }
        Value::Duration(d) => {
            w.u8(tag::DURATION).i64(d.as_nanos());
        }
        Value::Uuid(u) => {
            w.u8(tag::UUID).raw(u.as_bytes());
        }
        Value::RecordId(id) => {
            if matches!(id.key, RecordKey::Array(_)) {
                return false;
            }
            w.u8(tag::RECORD);
            if w.record_id(id).is_err() {
                return false;
            }
        }
        Value::Array(items) => {
            w.u8(tag::ARRAY);
            for item in items {
                if !write(w, item) {
                    return false;
                }
            }
            w.u8(tag::END);
        }
    }
    true
}

/// The canonical index encoding of `v`, or `None` if `v` is not indexable.
pub fn index_value(v: &Value) -> Option<Vec<u8>> {
    let mut w = KeyWriter::new();
    write(&mut w, v).then(|| w.finish())
}

/// Tag for a non-null value that cannot be encoded, in composite index
/// entries: an entry then exists exactly when every field is non-null. It
/// sorts after every encodable value, and no lookup ever seeks it.
pub const OPAQUE: u8 = 0xFE;

/// Appends the canonical encoding of `v`, or [`OPAQUE`] if `v` is a
/// non-null value that is not indexable. `false` only for `null`.
pub fn append_index_value_or_opaque(out: &mut Vec<u8>, v: &Value) -> bool {
    if v.is_null() {
        return false;
    }
    if !append_index_value(out, v) {
        out.push(OPAQUE);
    }
    true
}

/// Appends the canonical encoding of `v` to `out`; `false` if `v` is not
/// indexable (then `out` is left unchanged).
pub fn append_index_value(out: &mut Vec<u8>, v: &Value) -> bool {
    match index_value(v) {
        Some(bytes) => {
            out.extend_from_slice(&bytes);
            true
        }
        None => false,
    }
}

/// The class of values a range bound belongs to. A range scan stays inside
/// one class, because Cypher comparisons across classes yield `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeClass {
    Number,
    String,
    Datetime,
    Duration,
}

impl RangeClass {
    /// The class of `v`, if values of that class can be range-scanned.
    pub fn of(v: &Value) -> Option<Self> {
        match v {
            Value::Integer(_) => Some(RangeClass::Number),
            Value::Float(f) if !f.is_nan() => Some(RangeClass::Number),
            Value::String(_) => Some(RangeClass::String),
            Value::Datetime(_) => Some(RangeClass::Datetime),
            Value::Duration(_) => Some(RangeClass::Duration),
            _ => None,
        }
    }

    fn tag(self) -> u8 {
        match self {
            RangeClass::Number => tag::NUMBER,
            RangeClass::String => tag::STRING,
            RangeClass::Datetime => tag::DATETIME,
            RangeClass::Duration => tag::DURATION,
        }
    }
}

/// Byte bounds `[start, end)` (relative to an index prefix) that contain
/// every indexed value of `class` between `lower` and `upper`, inclusive.
///
/// The bounds may admit a few values just outside the range (numbers are
/// widened by one float step so that large integers are never missed); the
/// caller re-checks the predicate on every candidate.
pub fn range_bounds(
    class: RangeClass,
    lower: Option<&Value>,
    upper: Option<&Value>,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let class_prefix = vec![class.tag()];
    let class_end = prefix_end(&class_prefix)?;
    let bound = |v: &Value, low: bool| -> Option<Vec<u8>> {
        if RangeClass::of(v)? != class {
            return None;
        }
        let mut w = KeyWriter::new();
        match v {
            Value::Integer(_) | Value::Float(_) => {
                let f = v.as_float()?;
                w.u8(tag::NUMBER);
                if low {
                    ordered_f64(&mut w, f.next_down());
                    Some(w.finish())
                } else {
                    ordered_f64(&mut w, f.next_up());
                    prefix_end(&w.finish())
                }
            }
            other => {
                write(&mut w, other).then_some(())?;
                let enc = w.finish();
                if low {
                    Some(enc)
                } else {
                    prefix_end(&enc)
                }
            }
        }
    };
    let start = match lower {
        Some(v) => bound(v, true)?,
        None => class_prefix,
    };
    let end = match upper {
        Some(v) => bound(v, false)?,
        None => class_end,
    };
    Some((start, end))
}

/// Byte bounds for strings starting with `prefix`.
pub fn string_prefix_bounds(prefix: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut w = KeyWriter::new();
    w.u8(tag::STRING);
    // Escaped like `KeyWriter::str`, without the terminator.
    for &b in prefix.as_bytes() {
        w.u8(b);
        if b == 0 {
            w.u8(0xFF);
        }
    }
    let start = w.finish();
    let end = prefix_end(&start)?;
    Some((start, end))
}

/// `true` if an equality lookup of `v` is exact: equal values (in Cypher
/// terms) have the same encoding. Cypher compares an integer with a float
/// by rounding the integer, so a float of magnitude 2^53 or more can equal
/// integers stored in their inexact form (`2^53 + 1` rounds to `2^53`);
/// such lookups, and integers beyond ±2^53, are not exact.
pub fn eq_seekable(v: &Value) -> bool {
    match v {
        Value::Integer(i) => i.unsigned_abs() <= MAX_EXACT_INT as u64,
        Value::Float(f) => f.is_finite() && f.abs() < MAX_EXACT_INT as f64,
        Value::Array(items) => items.iter().all(eq_seekable),
        Value::Datetime(_) | Value::Duration(_) | Value::Uuid(_) => true,
        Value::Bool(_) | Value::String(_) | Value::Bytes(_) => true,
        Value::RecordId(id) => !matches!(id.key, RecordKey::Array(_)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn enc(v: &Value) -> Vec<u8> {
        index_value(v).expect("indexable")
    }

    #[test]
    fn integers_and_floats_share_keys() {
        assert_eq!(enc(&Value::Integer(1)), enc(&Value::Float(1.0)));
        assert_eq!(enc(&Value::Integer(0)), enc(&Value::Float(-0.0)));
        assert_eq!(
            enc(&Value::Integer(MAX_EXACT_INT)),
            enc(&Value::Float(MAX_EXACT_INT as f64))
        );
        assert_ne!(enc(&Value::Integer(1)), enc(&Value::Float(1.5)));
    }

    #[test]
    fn not_indexable() {
        for v in [
            Value::Null,
            Value::Float(f64::NAN),
            Value::Object(Default::default()),
            Value::Vector(vec![1.0]),
            Value::Array(vec![Value::Integer(1), Value::Null]),
        ] {
            assert!(index_value(&v).is_none(), "{v:?}");
        }
    }

    #[test]
    fn large_integers_keep_their_identity() {
        let a = enc(&Value::Integer(MAX_EXACT_INT + 1));
        let b = enc(&Value::Integer(MAX_EXACT_INT + 2));
        assert_ne!(a, b);
        assert!(a < b);
        assert!(enc(&Value::Integer(MAX_EXACT_INT)) < a);
        assert!(!eq_seekable(&Value::Integer(MAX_EXACT_INT + 1)));
        // 2^53 + 1 rounds to 2^53, so Cypher finds them equal.
        assert_eq!(
            Value::Integer(MAX_EXACT_INT + 1),
            Value::Float(MAX_EXACT_INT as f64)
        );
        assert!(!eq_seekable(&Value::Float(MAX_EXACT_INT as f64)));
        assert!(eq_seekable(&Value::Integer(MAX_EXACT_INT)));
        assert!(eq_seekable(&Value::Float(MAX_EXACT_INT as f64 - 1.0)));
    }

    #[test]
    fn prefix_bounds_cover_matching_strings() {
        let (start, end) = string_prefix_bounds("ab").unwrap();
        for s in ["ab", "abc", "ab\0", "abz"] {
            let e = enc(&Value::from(s));
            assert!(start <= e && e < end, "{s:?}");
        }
        for s in ["a", "aa", "b", "ac"] {
            let e = enc(&Value::from(s));
            assert!(!(start <= e && e < end), "{s:?}");
        }
    }

    proptest! {
        #[test]
        fn numbers_sort_numerically(a in any::<i64>(), b in any::<i64>(), x in -1e300f64..1e300) {
            let (ea, eb, ex) = (enc(&Value::Integer(a)), enc(&Value::Integer(b)), enc(&Value::Float(x)));
            prop_assert_eq!(a.cmp(&b), ea.cmp(&eb));
            // An integer and a float: the encoding order agrees with the
            // exact numeric order (i128 vs f64 compared without rounding).
            let exact = if (a as f64) < x { std::cmp::Ordering::Less }
                else if (a as f64) > x { std::cmp::Ordering::Greater }
                else { (a as i128).cmp(&(x as i128)) };
            prop_assert_eq!(exact, ea.cmp(&ex));
        }

        #[test]
        fn number_ranges_contain_their_members(v in any::<i64>(), lo in any::<i64>(), hi in any::<i64>()) {
            let (lo, hi) = (lo.min(hi), lo.max(hi));
            let (start, end) = range_bounds(
                RangeClass::Number,
                Some(&Value::Integer(lo)),
                Some(&Value::Integer(hi)),
            ).unwrap();
            let e = enc(&Value::Integer(v));
            if lo <= v && v <= hi {
                prop_assert!(start <= e && e < end);
            }
        }

        #[test]
        fn string_ranges_contain_their_members(v in ".{0,6}", lo in ".{0,6}", hi in ".{0,6}") {
            let (lo, hi) = (lo.clone().min(hi.clone()), lo.max(hi));
            let (start, end) = range_bounds(
                RangeClass::String,
                Some(&Value::from(lo.as_str())),
                Some(&Value::from(hi.as_str())),
            ).unwrap();
            let e = enc(&Value::from(v.as_str()));
            if lo <= v && v <= hi {
                prop_assert!(start <= e && e < end);
            }
        }
    }
}
