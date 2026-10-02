// Wall-clock helpers. A date library would be a dependency paid on every hook for
// two formats, so the civil date conversion is written out here instead
// (Howard Hinnant's days-from-civil algorithm, UTC only).

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-02T09:15:00.123Z`, the shape `Date.prototype.toISOString` produces.
pub fn iso(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let h = rem / 3_600_000;
    let min = (rem / 60_000) % 60;
    let s = (rem / 1000) % 60;
    let milli = rem % 1000;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}.{milli:03}Z")
}

pub fn now_iso() -> String {
    iso(now_ms())
}

/// The UTC calendar day, `2026-10-02`, which names the ledger file.
pub fn day(ms: i64) -> String {
    iso(ms)[..10].to_string()
}

pub fn today() -> String {
    day(now_ms())
}

/// Parses the ISO 8601 shapes a server sends: a date, optional time, optional
/// fraction, and `Z` or a `+HH:MM` offset. Anything else is None rather than a
/// guessed date.
pub fn parse_iso(text: &str) -> Option<i64> {
    let b = text.trim().as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let slice = b.get(from..from + len)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    let y = num(0, 4)?;
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let mo = num(5, 2)? as u32;
    let d = num(8, 2)? as u32;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut ms = days_from_civil(y, mo, d) * 86_400_000;
    if b.len() == 10 {
        return Some(ms);
    }
    if !matches!(b.get(10), Some(b'T') | Some(b't') | Some(b' ')) {
        return None;
    }
    let h = num(11, 2)?;
    if b.get(13) != Some(&b':') {
        return None;
    }
    let mi = num(14, 2)?;
    let mut at = 16;
    let mut sec = 0;
    if b.get(16) == Some(&b':') {
        sec = num(17, 2)?;
        at = 19;
    }
    ms += h * 3_600_000 + mi * 60_000 + sec * 1000;
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        let digits = &text.trim()[start..at];
        let padded: String = digits.chars().chain("000".chars()).take(3).collect();
        ms += padded.parse::<i64>().ok()?;
    }
    match b.get(at) {
        None => Some(ms),
        Some(b'Z') | Some(b'z') if at + 1 == b.len() => Some(ms),
        Some(sign @ (b'+' | b'-')) => {
            let oh = num(at + 1, 2)?;
            let om = if b.get(at + 3) == Some(&b':') {
                num(at + 4, 2)?
            } else {
                num(at + 3, 2)?
            };
            let offset = (oh * 60 + om) * 60_000;
            Some(if *sign == b'+' { ms - offset } else { ms + offset })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trips() {
        let ms = 1_790_931_452_621;
        let text = iso(ms);
        assert_eq!(text.len(), 24);
        assert_eq!(parse_iso(&text), Some(ms));
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            parse_iso("2026-08-08T00:00:00.000Z").map(iso).as_deref(),
            Some("2026-08-08T00:00:00.000Z")
        );
    }

    #[test]
    fn offsets_and_dates_parse() {
        assert_eq!(
            parse_iso("2026-08-08T02:00:00+02:00"),
            parse_iso("2026-08-08T00:00:00Z")
        );
        assert_eq!(parse_iso("2026-08-08"), parse_iso("2026-08-08T00:00:00.000Z"));
        assert_eq!(parse_iso("not a date"), None);
        assert_eq!(parse_iso("2026-13-01"), None);
    }

    #[test]
    fn day_is_the_utc_date() {
        assert_eq!(day(parse_iso("2026-10-02T23:59:59.999Z").unwrap()), "2026-10-02");
    }
}
