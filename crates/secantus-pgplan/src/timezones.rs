//! `pg_timezone_names` and `pg_timezone_abbrevs`: the time zones and the
//! abbreviations a timestamp's input accepts.
//!
//! The names are PostgreSQL 15.19's own list (`pg_timezone_names.txt`, the
//! zones its tz database holds); each one's abbreviation, offset and DST
//! flag are the zone's at the current moment, from `chrono-tz`, as
//! `pg_timezone_names` computes them from `now()`. The abbreviations are
//! the `Default` set PostgreSQL loads (`pg_timezone_abbrevs.tsv`), a fixed
//! table.
//!
//! The rules are `chrono-tz`'s tz database, PostgreSQL's its own compiled
//! one, so a zone whose rules the two versions disagree on answers by
//! `chrono-tz`'s: measured 2026-10-15, `Africa/Casablanca` and
//! `Africa/El_Aaiun` (`+01` here, `+00` on 15.19), and no other of the 598.

use super::*;
use chrono::{DateTime, TimeZone};
use chrono_tz::{OffsetComponents, OffsetName, Tz};

const NAMES: &str = include_str!("pg_timezone_names.txt");
const ABBREVS: &str = include_str!("pg_timezone_abbrevs.tsv");

fn seconds_interval(secs: i64) -> Bson {
    Interval {
        months: 0,
        days: 0,
        micros: secs * 1_000_000,
    }
    .to_bson()
}

/// `(name, abbrev, utc_offset, is_dst)` for each zone, at this moment.
pub fn names_rows() -> Vec<(String, String, Bson, bool)> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let now = DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .naive_utc();
    NAMES
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|name| {
            // `Factory`, zic's placeholder zone, which `chrono-tz` leaves out:
            // UTC, named `-00`.
            if name == "Factory" {
                return Some((
                    name.to_string(),
                    "-00".to_string(),
                    seconds_interval(0),
                    false,
                ));
            }
            let tz: Tz = name.parse().ok()?;
            let offset = tz.offset_from_utc_datetime(&now);
            let total = offset.base_utc_offset() + offset.dst_offset();
            // A zone with no letter abbreviation is named by its offset,
            // as `zic` writes it: `-03`, `+0545`.
            let abbrev = offset.abbreviation().map_or_else(
                || {
                    let secs = total.num_seconds();
                    let (sign, a) = if secs < 0 { ('-', -secs) } else { ('+', secs) };
                    let (h, m, s) = (a / 3600, a % 3600 / 60, a % 60);
                    match (m, s) {
                        (0, 0) => format!("{sign}{h:02}"),
                        (_, 0) => format!("{sign}{h:02}{m:02}"),
                        _ => format!("{sign}{h:02}{m:02}{s:02}"),
                    }
                },
                str::to_string,
            );
            Some((
                name.to_string(),
                abbrev,
                seconds_interval(total.num_seconds()),
                offset.dst_offset().num_seconds() != 0,
            ))
        })
        .collect()
}

/// `(abbrev, utc_offset, is_dst)` for each abbreviation.
pub fn abbrevs_rows() -> Vec<(String, Bson, bool)> {
    ABBREVS
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut parts = l.split('\t');
            let abbrev = parts.next()?;
            let secs: i64 = parts.next()?.parse().ok()?;
            let dst = parts.next()? == "t";
            Some((abbrev.to_string(), seconds_interval(secs), dst))
        })
        .collect()
}
