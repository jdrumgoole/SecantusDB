//! The timestamp of a mongod log line: `"t": {"$date": ...}` in mongod's
//! default `iso8601-local` format, the server process's local time to the
//! millisecond with its offset -- `2026-10-24T12:26:49.482+01:00` (measured
//! 8.2.11, 2026-10-24).

use chrono::TimeZone;

/// `millis` since the epoch as mongod writes a log line's time.
pub fn log_timestamp(millis: i64) -> String {
    match chrono::Local.timestamp_millis_opt(millis).single() {
        Some(local) => local.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_time_to_the_millisecond_with_its_offset() {
        let t = log_timestamp(1_792_841_284_642);
        // `YYYY-MM-DDTHH:MM:SS.mmm+HH:MM`, whatever the zone.
        assert_eq!(t.len(), 29, "{t}");
        assert_eq!(&t[10..11], "T");
        assert_eq!(&t[19..20], ".");
        assert!(matches!(&t[23..24], "+" | "-"), "{t}");
        assert_eq!(&t[26..27], ":");
    }
}
