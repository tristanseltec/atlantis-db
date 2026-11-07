//! Temporal value types: [`Datetime`] and [`Duration`].

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::AtlantisError;

const NANOS_PER_SEC: i64 = 1_000_000_000;
const SECS_PER_DAY: i64 = 86_400;

/// A UTC instant with nanosecond precision, stored as nanoseconds since the
/// Unix epoch. Covers roughly the years 1678 to 2262.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Datetime(pub i64);

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

impl Datetime {
    /// The current time.
    pub fn now() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        Datetime(nanos)
    }

    /// Builds a datetime from UTC calendar fields.
    pub fn from_ymd_hms(
        year: i64,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        nanos: u32,
    ) -> Result<Self, AtlantisError> {
        let invalid = || {
            AtlantisError::InvalidValue(format!(
                "invalid datetime {year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
            ))
        };
        if !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
            || nanos >= NANOS_PER_SEC as u32
        {
            return Err(invalid());
        }
        let secs = days_from_civil(year, month, day) * SECS_PER_DAY
            + hour as i64 * 3600
            + minute as i64 * 60
            + second as i64;
        secs.checked_mul(NANOS_PER_SEC)
            .and_then(|n| n.checked_add(nanos as i64))
            .map(Datetime)
            .ok_or_else(invalid)
    }

    /// Nanoseconds since the Unix epoch.
    pub fn as_nanos(&self) -> i64 {
        self.0
    }

    /// Adds a duration, failing on overflow.
    pub fn checked_add(&self, d: Duration) -> Option<Self> {
        self.0.checked_add(d.0).map(Datetime)
    }
}

impl fmt::Display for Datetime {
    /// Formats as RFC 3339 in UTC, e.g. `2026-11-07T13:46:10.5Z`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.0.div_euclid(NANOS_PER_SEC);
        let nanos = self.0.rem_euclid(NANOS_PER_SEC);
        let (y, m, d) = civil_from_days(secs.div_euclid(SECS_PER_DAY));
        let sod = secs.rem_euclid(SECS_PER_DAY);
        write!(
            f,
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
            sod / 3600,
            sod % 3600 / 60,
            sod % 60
        )?;
        if nanos != 0 {
            let frac = format!("{nanos:09}");
            write!(f, ".{}", frac.trim_end_matches('0'))?;
        }
        f.write_str("Z")
    }
}

impl FromStr for Datetime {
    type Err = AtlantisError;

    /// Parses RFC 3339 (`2026-11-07T13:46:10Z`, `...+07:00`, fractional
    /// seconds allowed) or a plain date (`2026-11-07`, midnight UTC).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || AtlantisError::InvalidValue(format!("invalid datetime: {s}"));
        let num = |part: &str| part.parse::<u32>().map_err(|_| err());
        let b = s.as_bytes();
        if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
            return Err(err());
        }
        let year: i64 = s[..4].parse().map_err(|_| err())?;
        let (month, day) = (num(&s[5..7])?, num(&s[8..10])?);
        if b.len() == 10 {
            return Datetime::from_ymd_hms(year, month, day, 0, 0, 0, 0);
        }
        if b.len() < 20 || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
            return Err(err());
        }
        let (hour, minute, second) = (num(&s[11..13])?, num(&s[14..16])?, num(&s[17..19])?);
        let mut rest = &s[19..];
        let mut nanos = 0u32;
        if let Some(frac) = rest.strip_prefix('.') {
            let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 || digits > 9 {
                return Err(err());
            }
            nanos = num(&frac[..digits])? * 10u32.pow(9 - digits as u32);
            rest = &frac[digits..];
        }
        let offset_secs: i64 = match rest {
            "Z" | "z" => 0,
            _ if rest.len() == 6
                && matches!(rest.as_bytes()[0], b'+' | b'-')
                && rest.as_bytes()[3] == b':' =>
            {
                let mins = num(&rest[1..3])? as i64 * 60 + num(&rest[4..6])? as i64;
                if rest.starts_with('-') {
                    -mins * 60
                } else {
                    mins * 60
                }
            }
            _ => return Err(err()),
        };
        let local = Datetime::from_ymd_hms(year, month, day, hour, minute, second, nanos)?;
        local
            .0
            .checked_sub(offset_secs * NANOS_PER_SEC)
            .map(Datetime)
            .ok_or_else(err)
    }
}

/// A signed length of time with nanosecond precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Duration(pub i64);

const UNITS: [(&str, i64); 8] = [
    ("w", 7 * SECS_PER_DAY * NANOS_PER_SEC),
    ("d", SECS_PER_DAY * NANOS_PER_SEC),
    ("h", 3600 * NANOS_PER_SEC),
    ("m", 60 * NANOS_PER_SEC),
    ("s", NANOS_PER_SEC),
    ("ms", 1_000_000),
    ("us", 1_000),
    ("ns", 1),
];

impl Duration {
    /// Builds a duration from whole seconds.
    pub fn from_secs(secs: i64) -> Self {
        Duration(secs * NANOS_PER_SEC)
    }

    /// Total nanoseconds.
    pub fn as_nanos(&self) -> i64 {
        self.0
    }

    fn parse_compact(s: &str) -> Option<i64> {
        let mut total: i64 = 0;
        let mut rest = s;
        while !rest.is_empty() {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return None;
            }
            let n: i64 = rest[..digits].parse().ok()?;
            rest = &rest[digits..];
            let unit_len = rest.bytes().take_while(|c| c.is_ascii_alphabetic()).count();
            let unit = match &rest[..unit_len] {
                "µs" => "us",
                u => u,
            };
            let (_, scale) = UNITS.iter().find(|(name, _)| *name == unit)?;
            total = total.checked_add(n.checked_mul(*scale)?)?;
            rest = &rest[unit_len..];
        }
        Some(total)
    }

    /// ISO 8601 durations without years or months, e.g. `PT5M`, `P1DT2H`, `P2W`.
    fn parse_iso(s: &str) -> Option<i64> {
        let body = s.strip_prefix('P').or_else(|| s.strip_prefix('p'))?;
        let (date, time) = match body.find(['T', 't']) {
            Some(i) => (&body[..i], Some(&body[i + 1..])),
            None => (body, None),
        };
        let mut total: i64 = 0;
        let mut add = |part: &str, units: &[(char, i64)]| -> Option<()> {
            let mut rest = part;
            while !rest.is_empty() {
                let len = rest
                    .bytes()
                    .take_while(|c| c.is_ascii_digit() || *c == b'.')
                    .count();
                let n: f64 = rest[..len].parse().ok()?;
                let unit = rest[len..].chars().next()?.to_ascii_uppercase();
                let (_, scale) = units.iter().find(|(u, _)| *u == unit)?;
                total = total.checked_add((n * *scale as f64).round() as i64)?;
                rest = &rest[len + 1..];
            }
            Some(())
        };
        add(date, &[('W', UNITS[0].1), ('D', UNITS[1].1)])?;
        if let Some(time) = time {
            if time.is_empty() {
                return None;
            }
            add(
                time,
                &[('H', UNITS[2].1), ('M', UNITS[3].1), ('S', UNITS[4].1)],
            )?;
        }
        (!body.is_empty()).then_some(total)
    }
}

impl fmt::Display for Duration {
    /// Formats compactly, e.g. `1h30m`, `250ms`, `0ns`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("0ns");
        }
        if self.0 < 0 {
            f.write_str("-")?;
        }
        let mut rest = self.0.unsigned_abs();
        for (name, scale) in UNITS {
            let scale = scale as u64;
            if rest >= scale {
                write!(f, "{}{name}", rest / scale)?;
                rest %= scale;
            }
        }
        Ok(())
    }
}

impl FromStr for Duration {
    type Err = AtlantisError;

    /// Parses `1h30m`, `7d`, `250ms`, `-5s`, or ISO 8601 such as `PT5M`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (neg, body) = match s.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, s),
        };
        let parsed = if body.starts_with(['P', 'p']) {
            Duration::parse_iso(body)
        } else {
            (!body.is_empty())
                .then(|| Duration::parse_compact(body))
                .flatten()
        };
        parsed
            .map(|n| Duration(if neg { -n } else { n }))
            .ok_or_else(|| AtlantisError::InvalidValue(format!("invalid duration: {s}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_epoch_and_known_dates() {
        assert_eq!(Datetime(0).to_string(), "1970-01-01T00:00:00Z");
        let dt = Datetime::from_ymd_hms(2026, 11, 7, 13, 46, 10, 500_000_000).unwrap();
        assert_eq!(dt.to_string(), "2026-11-07T13:46:10.5Z");
        let leap = Datetime::from_ymd_hms(2024, 2, 29, 0, 0, 0, 0).unwrap();
        assert_eq!(leap.to_string(), "2024-02-29T00:00:00Z");
        assert!(Datetime::from_ymd_hms(2023, 2, 29, 0, 0, 0, 0).is_err());
        let before_epoch = Datetime::from_ymd_hms(1969, 12, 31, 23, 59, 59, 0).unwrap();
        assert_eq!(before_epoch.0, -NANOS_PER_SEC);
        assert_eq!(before_epoch.to_string(), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn datetime_parse_roundtrip_and_offsets() {
        for s in [
            "2026-11-07T13:46:10Z",
            "2000-01-01T00:00:00.123456789Z",
            "1900-03-01T12:00:00Z",
        ] {
            assert_eq!(s.parse::<Datetime>().unwrap().to_string(), s);
        }
        let saigon: Datetime = "2026-11-07T20:46:10+07:00".parse().unwrap();
        assert_eq!(saigon.to_string(), "2026-11-07T13:46:10Z");
        let date: Datetime = "2026-11-07".parse().unwrap();
        assert_eq!(date.to_string(), "2026-11-07T00:00:00Z");
        assert!("2026-13-01".parse::<Datetime>().is_err());
        assert!("2026-11-07T10:00:00".parse::<Datetime>().is_err()); // offset required
    }

    #[test]
    fn duration_parse_and_display() {
        let d: Duration = "1h30m".parse().unwrap();
        assert_eq!(d.as_nanos(), 5400 * NANOS_PER_SEC);
        assert_eq!(d.to_string(), "1h30m");
        assert_eq!("250ms".parse::<Duration>().unwrap().to_string(), "250ms");
        assert_eq!("7d".parse::<Duration>().unwrap().to_string(), "1w");
        assert_eq!(
            "-5s".parse::<Duration>().unwrap().as_nanos(),
            -5 * NANOS_PER_SEC
        );
        assert_eq!("PT5M".parse::<Duration>().unwrap().to_string(), "5m");
        assert_eq!("P1DT2H".parse::<Duration>().unwrap().to_string(), "1d2h");
        assert_eq!("PT0.5S".parse::<Duration>().unwrap().to_string(), "500ms");
        assert_eq!(Duration(0).to_string(), "0ns");
        for bad in ["", "5", "5x", "P1Y", "PT", "h"] {
            assert!(bad.parse::<Duration>().is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn datetime_plus_duration() {
        let dt: Datetime = "2026-11-07T00:00:00Z".parse().unwrap();
        let later = dt.checked_add("1d".parse().unwrap()).unwrap();
        assert_eq!(later.to_string(), "2026-11-08T00:00:00Z");
    }
}
