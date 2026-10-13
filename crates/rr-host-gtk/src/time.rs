//! The API's times (RFC 3339, UTC) read and said, without a date library.

use std::time::{SystemTime, UNIX_EPOCH};

/// `2026-10-13T09:40:00.000Z` (or with `+00:00`, or a date alone) as seconds since 1970.
pub fn parse_rfc3339(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date, rest) = match s.find(['T', 't', ' ']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let mut secs = days_from_civil(y, m, day) as f64 * 86400.0;
    if rest.is_empty() {
        return Some(secs);
    }
    // The offset: Z, or ±HH:MM.
    let (clock, offset) = if let Some(c) = rest.strip_suffix(['Z', 'z']) {
        (c, 0.0)
    } else if let Some(i) = rest.rfind(['+', '-']) {
        let (c, o) = rest.split_at(i);
        let sign = if o.starts_with('-') { -1.0 } else { 1.0 };
        let (oh, om) = o[1..].split_once(':')?;
        (c, sign * (oh.parse::<f64>().ok()? * 3600.0 + om.parse::<f64>().ok()? * 60.0))
    } else {
        (rest, 0.0)
    };
    let mut c = clock.split(':');
    let h: f64 = c.next()?.parse().ok()?;
    let min: f64 = c.next()?.parse().ok()?;
    let sec: f64 = c.next().unwrap_or("0").parse().ok()?;
    secs += h * 3600.0 + min * 60.0 + sec - offset;
    Some(secs)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Seconds left until `expires_at`, at `now` (0 once it's past).
pub fn seconds_left(expires_at: &str, now: f64) -> Option<u64> {
    Some((parse_rfc3339(expires_at)? - now).max(0.0).ceil() as u64)
}

/// `9:05` for 545 seconds.
pub fn countdown(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// How long ago, said shortly: "just now", "3 min ago", "2 h ago", "4 days ago".
pub fn ago(ts: &str, now: f64) -> String {
    let Some(t) = parse_rfc3339(ts) else { return ts.to_owned() };
    let s = (now - t).max(0.0) as u64;
    match s {
        0..=9 => "just now".into(),
        10..=59 => format!("{s} s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

/// A timestamp as `2026-10-13 09:35:12` (UTC, as the API gives it).
pub fn short(ts: &str) -> String {
    let t = ts.replace('T', " ");
    let t = t.trim_end_matches('Z');
    match t.find('.') {
        Some(i) => t[..i].to_owned(),
        None => t.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_docs_times() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_rfc3339("2026-10-13T09:40:00.000Z"), Some(1_791_884_400.0));
        assert_eq!(parse_rfc3339("2026-10-13T11:40:00+02:00"), Some(1_791_884_400.0));
        assert_eq!(parse_rfc3339("2026-10-12"), Some(1_791_763_200.0));
        assert_eq!(parse_rfc3339("2000-02-29T12:00:00.5Z"), Some(951_825_600.5));
        assert_eq!(parse_rfc3339("nonsense"), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn countdowns() {
        let t = parse_rfc3339("2026-10-13T09:40:00.000Z").unwrap();
        assert_eq!(seconds_left("2026-10-13T09:40:00.000Z", t - 512.0), Some(512));
        assert_eq!(seconds_left("2026-10-13T09:40:00.000Z", t + 3.0), Some(0));
        assert_eq!(countdown(545), "9:05");
        assert_eq!(countdown(0), "0:00");
        assert_eq!(ago("2026-10-13T09:40:00.000Z", t + 125.0), "2 min ago");
        assert_eq!(short("2026-10-13T09:35:12.004Z"), "2026-10-13 09:35:12");
    }
}
