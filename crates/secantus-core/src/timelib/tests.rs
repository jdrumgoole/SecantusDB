//! Each expectation measured on mongod 8.2.11 (2026-10-06) with
//! `$toDate: <string>` -- the full error text, or the resulting instant.

use super::{millis, mongo_parse, mongo_parse_format, update_ts, validate_format};

fn parse_ms(s: &str) -> Result<i128, String> {
    let mut t = mongo_parse(s)?;
    update_ts(&mut t);
    Ok(millis(&t).expect("in range") as i128)
}

/// Milliseconds since the epoch of a UTC wall-clock time.
fn at(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i128 {
    let days = {
        // days_from_civil
        let y = y - i64::from(mo <= 2);
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    };
    ((days * 86_400 + h * 3600 + mi * 60 + s) as i128) * 1000
}

#[test]
fn error_text_matches_mongod() {
    let err = |s: &str| parse_ms(s).unwrap_err();
    assert_eq!(err("abc"), "Error parsing date string 'abc'; 0: passing a time zone identifier as part of the string is not allowed 'a'");
    assert_eq!(
        err(""),
        "Error parsing date string ''; 0: Empty string '\0'"
    );
    assert_eq!(
        err("2024-13-01"),
        "Error parsing date string '2024-13-01'; 6: Unexpected character '3'"
    );
    assert_eq!(
        err("2024-02-30"),
        "Error parsing date string '2024-02-30'; 11: The parsed date was invalid '\0'"
    );
    assert_eq!(
        err("2024-01-01T25:00"),
        "Error parsing date string '2024-01-01T25:00'; 12: Double time specification '5'"
    );
    assert_eq!(err("hello world"), "Error parsing date string 'hello world'; 0: passing a time zone identifier as part of the string is not allowed 'h'; 6: Double timezone specification 'w'");
    assert_eq!(err("abc def ghi"), "Error parsing date string 'abc def ghi'; 0: passing a time zone identifier as part of the string is not allowed 'a'; 8: Double timezone specification 'g'; 4: Double timezone specification 'd'");
    assert_eq!(err("2024-01-01T00:00:00+25:00"), "Error parsing date string '2024-01-01T00:00:00+25:00'; 22: Unexpected character ':'; 23: Unexpected character '0'; 24: Unexpected character '0'");
    assert_eq!(err("@@@"), "Error parsing date string '@@@'; 0: Unexpected character '@'; 1: Unexpected character '@'; 2: Unexpected character '@'");
    assert_eq!(
        err("x"),
        r#"an incomplete date/time string has been found, with elements missing: "x""#
    );
    assert_eq!(
        err("12:00"),
        r#"an incomplete date/time string has been found, with elements missing: "12:00""#
    );
    assert_eq!(
        err("2024-01-01 23:59:60"),
        "Error parsing date string '2024-01-01 23:59:60'; 20: The parsed time was invalid '\0'"
    );
}

#[test]
fn values_match_mongod() {
    assert_eq!(
        parse_ms("2024-01-01 10:00 PM"),
        Ok(at(2024, 1, 1, 22, 0, 0))
    );
    assert_eq!(parse_ms("2024-01-01 12:00 AM"), Ok(at(2024, 1, 1, 0, 0, 0)));
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 UTC"),
        Ok(at(2024, 1, 1, 22, 30, 0))
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 EST"),
        Ok(at(2024, 1, 2, 3, 30, 0))
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 IST"),
        Ok(at(2024, 1, 1, 20, 30, 0))
    );
    assert_eq!(parse_ms("2024-01-01T"), Ok(at(2024, 1, 1, 7, 0, 0)));
    assert_eq!(
        parse_ms("Tue, 01 Jan 2024 10:00:00 GMT"),
        Ok(at(2024, 1, 2, 10, 0, 0))
    );
    assert_eq!(parse_ms("Jan 2024"), Ok(at(2024, 1, 1, 0, 0, 0)));
    assert_eq!(parse_ms("2024-W01-1"), Ok(at(2024, 1, 1, 0, 0, 0)));
    assert_eq!(
        parse_ms("2024-01-01T10:20:30.123Z"),
        Ok(at(2024, 1, 1, 10, 20, 30) + 123)
    );
}

/// The cases the hand-written parser this port replaced was tested on (PR
/// #1759), each measured on mongod 8.2.11, now run against the port.
#[test]
fn cases_carried_over_from_the_old_parser() {
    assert_eq!(
        parse_ms("2024-01-01 10:00 PM"),
        Ok(at(2024, 1, 1, 22, 0, 0)),
        "2024-01-01 10:00 PM"
    );
    assert_eq!(
        parse_ms("2024-01-01 10pm"),
        Ok(at(2024, 1, 1, 22, 0, 0)),
        "2024-01-01 10pm"
    );
    assert_eq!(
        parse_ms("2024-01-01 10 pm"),
        Ok(at(2024, 1, 1, 22, 0, 0)),
        "2024-01-01 10 pm"
    );
    assert_eq!(
        parse_ms("2024-01-01 10:00:00 am"),
        Ok(at(2024, 1, 1, 10, 0, 0)),
        "2024-01-01 10:00:00 am"
    );
    assert_eq!(
        parse_ms("2024-01-01 12:00 AM"),
        Ok(at(2024, 1, 1, 0, 0, 0)),
        "2024-01-01 12:00 AM"
    );
    assert_eq!(
        parse_ms("2024-01-01 12:00 PM"),
        Ok(at(2024, 1, 1, 12, 0, 0)),
        "2024-01-01 12:00 PM"
    );
    assert_eq!(
        parse_ms("2024-01-01 10:30 p.m."),
        Ok(at(2024, 1, 1, 22, 30, 0)),
        "2024-01-01 10:30 p.m."
    );
    assert_eq!(
        parse_ms("Jan 1 2024 10:00 PM"),
        Ok(at(2024, 1, 1, 22, 0, 0)),
        "Jan 1 2024 10:00 PM"
    );
    assert_eq!(
        parse_ms("10:00 PM 2024-01-01"),
        Ok(at(2024, 1, 1, 22, 0, 0)),
        "10:00 PM 2024-01-01"
    );
    assert_eq!(
        parse_ms("2024-01-01 10:00 PM +02:00"),
        Ok(at(2024, 1, 1, 20, 0, 0)),
        "2024-01-01 10:00 PM +02:00"
    );
    assert_eq!(
        parse_ms("2024-01-01 10:00 P"),
        Ok(at(2024, 1, 1, 13, 0, 0)),
        "2024-01-01 10:00 P"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 UTC"),
        Ok(at(2024, 1, 1, 22, 30, 0)),
        "2024-01-01 22:30:00 UTC"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 utc"),
        Ok(at(2024, 1, 1, 22, 30, 0)),
        "2024-01-01 22:30:00 utc"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 GMT"),
        Ok(at(2024, 1, 1, 22, 30, 0)),
        "2024-01-01 22:30:00 GMT"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 EST"),
        Ok(at(2024, 1, 2, 3, 30, 0)),
        "2024-01-01 22:30:00 EST"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 PST"),
        Ok(at(2024, 1, 2, 6, 30, 0)),
        "2024-01-01 22:30:00 PST"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 IST"),
        Ok(at(2024, 1, 1, 20, 30, 0)),
        "2024-01-01 22:30:00 IST"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 NDT"),
        Ok(at(2024, 1, 2, 1, 0, 52)),
        "2024-01-01 22:30:00 NDT"
    );
    assert!(
        parse_ms("2024-01-01 22:30:00 SGT").is_err(),
        "2024-01-01 22:30:00 SGT"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 +02:00"),
        Ok(at(2024, 1, 1, 20, 30, 0)),
        "2024-01-01 22:30:00 +02:00"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00+0200"),
        Ok(at(2024, 1, 1, 20, 30, 0)),
        "2024-01-01 22:30:00+0200"
    );
    assert_eq!(
        parse_ms("2024-01-01 22:30:00 -05"),
        Ok(at(2024, 1, 2, 3, 30, 0)),
        "2024-01-01 22:30:00 -05"
    );
    assert_eq!(
        parse_ms("22:00 2024-01-01"),
        Ok(at(2024, 1, 1, 22, 0, 0)),
        "22:00 2024-01-01"
    );
    assert_eq!(
        parse_ms("Jan 1 2024 22:00:00"),
        Ok(at(2024, 1, 1, 22, 0, 0)),
        "Jan 1 2024 22:00:00"
    );
    assert_eq!(
        parse_ms("22:00:00 2024-01-01"),
        Ok(at(2024, 1, 1, 22, 0, 0)),
        "22:00:00 2024-01-01"
    );
    assert_eq!(
        parse_ms("Mon, 01 Jan 2024 10:00:00 GMT"),
        Ok(at(2024, 1, 1, 10, 0, 0)),
        "Mon, 01 Jan 2024 10:00:00 GMT"
    );
    assert_eq!(
        parse_ms("Tue, 01 Jan 2024 10:00:00 GMT"),
        Ok(at(2024, 1, 2, 10, 0, 0)),
        "Tue, 01 Jan 2024 10:00:00 GMT"
    );
    assert_eq!(
        parse_ms("Sun, 01 Jan 2024"),
        Ok(at(2024, 1, 7, 0, 0, 0)),
        "Sun, 01 Jan 2024"
    );
    assert_eq!(
        parse_ms("Tue Jan 1 10:00:00 2024"),
        Ok(at(2024, 1, 2, 10, 0, 0)),
        "Tue Jan 1 10:00:00 2024"
    );
    assert_eq!(
        parse_ms("Jan 2024"),
        Ok(at(2024, 1, 1, 0, 0, 0)),
        "Jan 2024"
    );
    assert_eq!(
        parse_ms("2024 Jan"),
        Ok(at(2024, 1, 1, 0, 0, 0)),
        "2024 Jan"
    );
    assert_eq!(
        parse_ms("Sept 2024"),
        Ok(at(2024, 9, 1, 0, 0, 0)),
        "Sept 2024"
    );
    assert_eq!(
        parse_ms("Jan 1 10:00:00 2024"),
        Ok(at(2024, 1, 1, 10, 0, 0)),
        "Jan 1 10:00:00 2024"
    );
    assert!(parse_ms("Jan, 2024").is_err(), "Jan, 2024");
    assert!(
        parse_ms("2024-01-01 23:59:60").is_err(),
        "2024-01-01 23:59:60"
    );
    assert!(
        parse_ms("2024-01-01T23:59:60").is_err(),
        "2024-01-01T23:59:60"
    );
    assert!(
        parse_ms("2024-01-01 13:00 PM").is_err(),
        "2024-01-01 13:00 PM"
    );
    assert!(
        parse_ms("2024-01-01 0:30 am").is_err(),
        "2024-01-01 0:30 am"
    );
    assert!(
        parse_ms("2024-01-01 10:00:30.5 pm").is_err(),
        "2024-01-01 10:00:30.5 pm"
    );
    assert!(
        parse_ms("2024-01-01T10:00 PM").is_err(),
        "2024-01-01T10:00 PM"
    );
    assert!(parse_ms("10:00 PM").is_err(), "10:00 PM");
}

fn format_ms(s: &str, f: &str) -> Result<i128, String> {
    let mut t = mongo_parse_format(s, f)?;
    update_ts(&mut t);
    Ok(millis(&t).expect("in range") as i128)
}

/// `$dateFromString` with a `format`, measured on mongod 8.2.11 (2026-10-06).
#[test]
fn format_values_match_mongod() {
    let ok = |s: &str, f: &str| format_ms(s, f).unwrap();
    assert_eq!(ok("2024-01-15", "%Y-%m-%d"), at(2024, 1, 15, 0, 0, 0));
    assert_eq!(
        ok("2024-01-15T10:30:45.1", "%Y-%m-%dT%H:%M:%S.%L"),
        at(2024, 1, 15, 10, 30, 45) + 100
    );
    // %j is zero-based: day 15 is the 16th.
    assert_eq!(ok("2024-015", "%Y-%j"), at(2024, 1, 16, 0, 0, 0));
    assert_eq!(ok("2023-366", "%Y-%j"), at(2024, 1, 2, 0, 0, 0));
    assert_eq!(ok("2024-999", "%Y-%j"), at(2026, 9, 26, 0, 0, 0));
    assert_eq!(ok("2024-W03-1", "%G-W%V-%u"), at(2024, 1, 15, 0, 0, 0));
    assert_eq!(ok("2020-W53-7", "%G-W%V-%u"), at(2021, 1, 3, 0, 0, 0));
    assert_eq!(ok("2024", "%G"), at(2024, 1, 1, 0, 0, 0));
    assert_eq!(
        ok("2024-01-15 +0530", "%Y-%m-%d %z"),
        at(2024, 1, 14, 18, 30, 0)
    );
    assert_eq!(
        ok("2024-01-15 10:30 EST", "%Y-%m-%d %H:%M %z"),
        at(2024, 1, 15, 15, 30, 0)
    );
    // %Z is an offset in MINUTES.
    assert_eq!(
        ok("2024-01-15 10:30 -90", "%Y-%m-%d %H:%M %Z"),
        at(2024, 1, 15, 12, 0, 0)
    );
    assert_eq!(ok("15 January 2024", "%d %B %Y"), at(2024, 1, 15, 0, 0, 0));
    assert_eq!(ok("24-01-15", "%Y-%m-%d"), at(24, 1, 15, 0, 0, 0));
    assert_eq!(
        ok("2024-01-15 1:05", "%Y-%m-%d %H:%M"),
        at(2024, 1, 15, 1, 5, 0)
    );
}

#[test]
fn format_error_text_matches_mongod() {
    let err = |s: &str, f: &str| format_ms(s, f).unwrap_err();
    assert_eq!(
        err("2024", "%Y"),
        r#"an incomplete date/time string has been found, with elements missing: "2024""#
    );
    assert_eq!(
        err("2024-13-01", "%Y-%m-%d"),
        "Error parsing date string '2024-13-01'; 10: The parsed date was invalid '\0'"
    );
    assert_eq!(
        err("2024-01-15", "%Y/%m/%d"),
        "Error parsing date string '2024-01-15'; 4: Format literal not found '-'; 7: Format literal not found '-'"
    );
    assert_eq!(
        err("2024-01", "%Y-%m-%d"),
        "Error parsing date string '2024-01'; 7: Not enough data available to satisfy format '\0'"
    );
    assert_eq!(
        err("abc", "%Y"),
        "Error parsing date string 'abc'; 0: Unexpected data found. 'a'; 0: A four digit year could not be found 'a'"
    );
    assert_eq!(
        err("2024-01-15 EST", "%Y-%m-%d %Z"),
        "Error parsing date string '2024-01-15 EST'; 11: Invalid timezone offset in minutes 'E'; 11: Trailing data 'E'"
    );
    assert_eq!(
        err("2024-01-15 Europe/Dublin", "%Y-%m-%d %z"),
        "Error parsing date string '2024-01-15 Europe/Dublin'; 11: passing a time zone identifier as part of the string is not allowed 'E'"
    );
    assert_eq!(
        err("2024 2024-W01-1", "%Y %G-W%V-%u"),
        "Error parsing date string '2024 2024-W01-1'; 15: Mixing of ISO dates with natural dates is not allowed '\0'; 15: Mixing of ISO dates with natural dates is not allowed '\0'"
    );
    assert_eq!(
        err("015-2024", "%j-%Y"),
        "Error parsing date string '015-2024'; 0: A 'day of year' can only come after a year has been found '0'"
    );
}

#[test]
fn format_validation_matches_mongod() {
    assert_eq!(
        validate_format("%Y-%m-%d %H:%M:%S.%L%z%Z%G%V%u%j%b%B%%"),
        Ok(())
    );
    assert_eq!(
        validate_format("%Y-%m-%d%"),
        Err((18535, "Unmatched '%' at end of format string".into()))
    );
    assert_eq!(
        validate_format("%A"),
        Err((
            18536,
            "Invalid format character '%A' in format string".into()
        ))
    );
}
