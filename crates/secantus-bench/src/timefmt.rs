//! Minimal UTC timestamp formatting.
//!
//! The harness needs exactly one thing from a calendar — a sortable run id like
//! `20260821T140311Z` for the results directory. That is not worth a `chrono`
//! dependency in a workspace this dep-light, so the civil-date conversion below
//! (Howard Hinnant's `civil_from_days`) is inlined and unit-tested.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `(year, month, day)` for a count of days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYYmmddTHHMMSSZ` — sortable, filename-safe, unambiguous.
pub fn run_id(epoch_secs: f64) -> String {
    let secs = epoch_secs as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// `YYYY-mm-ddTHH:MM:SSZ` — the same instant, for report metadata.
pub fn iso8601(epoch_secs: f64) -> String {
    let id = run_id(epoch_secs);
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        &id[0..4],
        &id[4..6],
        &id[6..8],
        &id[9..11],
        &id[11..13],
        &id[13..15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_the_first_of_january_1970() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(run_id(0.0), "19700101T000000Z");
    }

    #[test]
    fn known_instants_round_trip() {
        // 2026-08-21T14:03:11Z == 1787320991
        assert_eq!(run_id(1_787_320_991.0), "20260821T140311Z");
        assert_eq!(iso8601(1_787_320_991.0), "2026-08-21T14:03:11Z");
    }

    #[test]
    fn leap_day_is_handled() {
        // 2024-02-29T00:00:00Z == 1709164800
        assert_eq!(run_id(1_709_164_800.0), "20240229T000000Z");
    }

    #[test]
    fn run_ids_sort_chronologically() {
        let a = run_id(1_700_000_000.0);
        let b = run_id(1_800_000_000.0);
        assert!(a < b, "{a} should sort before {b}");
    }
}
