//! Each expectation measured on mongod 8.2.11 (2026-10-06) with
//! `$toDate: <string>` -- the full error text, or the resulting instant.

use super::{millis, mongo_parse, update_ts};

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
