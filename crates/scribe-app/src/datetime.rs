//! Dependency-free UTC timestamp formatting for the "Insert date/time"
//! command. We deliberately avoid pulling a calendar/timezone crate (chrono /
//! time / jiff) for one small feature — the std clock plus a pure civil-date
//! conversion gives an unambiguous UTC ISO-8601 string with zero new
//! dependencies and zero privacy surface (no network, no locale probing).
//!
//! Local-time would require a timezone database; if that is ever wanted it is
//! a deliberate dependency decision, not a silent one.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`. Falls back to the epoch string
/// if the system clock is before 1970 (it never is in practice).
pub fn now_iso8601_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso8601_utc(secs)
}

/// Format UNIX seconds (UTC) as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm (public domain) to turn
/// days-since-epoch into a proleptic-Gregorian Y/M/D without any external
/// crate. Correct for all dates the editor will ever stamp.
pub fn format_iso8601_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // civil_from_days: day 0 == 1970-01-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_epochs_format_correctly() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
        // 2001-09-09T01:46:40Z — the classic 1e9 epoch second.
        assert_eq!(format_iso8601_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap-year date: 2024-02-29T12:30:45Z.
        assert_eq!(format_iso8601_utc(1_709_209_845), "2024-02-29T12:30:45Z");
        // End-of-year boundary: 2023-12-31T23:59:59Z.
        assert_eq!(format_iso8601_utc(1_704_067_199), "2023-12-31T23:59:59Z");
    }

    #[test]
    fn epoch_day_zero_is_1970_01_01() {
        // Hinnant's `civil_from_days` contract: day 0 == 1970-01-01. This is the
        // era/epoch anchor the audit flagged as a possible off-by-one. The whole
        // first day must map to 1970-01-01 (no wrap into 1969 or 1970-01-02).
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601_utc(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(format_iso8601_utc(86_400), "1970-01-02T00:00:00Z");
    }

    #[test]
    fn century_leap_day_2000_02_29() {
        // Year 2000 IS a leap year (divisible by 400) — the case that exercises
        // the `doe/36524` and `doe/146096` century/era correction terms. An era
        // off-by-one would shift Feb 29 to Mar 1 or drop the day entirely.
        assert_eq!(format_iso8601_utc(951_827_445), "2000-02-29T12:30:45Z");
        // The day immediately after the century leap day.
        assert_eq!(format_iso8601_utc(951_868_800), "2000-03-01T00:00:00Z");
    }

    #[test]
    fn year_boundary_transition() {
        // Dec 31 -> Jan 1 across a year boundary: the `year + (month <= 2)`
        // Jan/Feb correction and month/day reconstruction must not slip a day.
        assert_eq!(format_iso8601_utc(978_307_199), "2000-12-31T23:59:59Z");
        assert_eq!(format_iso8601_utc(978_307_200), "2001-01-01T00:00:00Z");
    }

    #[test]
    fn recent_known_date() {
        // A recent date (2025-06-24) — the kind of value an audit timestamp
        // actually stamps — verified against an independent reference.
        assert_eq!(format_iso8601_utc(1_750_723_200), "2025-06-24T00:00:00Z");
    }

    #[test]
    fn now_has_iso8601_shape() {
        let s = now_iso8601_utc();
        // YYYY-MM-DDTHH:MM:SSZ == 20 chars, ends with Z, has the T separator.
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'));
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[4..5], "-");
    }
}
