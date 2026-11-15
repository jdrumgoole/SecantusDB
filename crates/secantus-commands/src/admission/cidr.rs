//! mongod's CIDR range lists (`CIDRRangeListParameters`), the value of
//! `ingressConnectionEstablishmentRateLimiterBypass` (RM-550).
//!
//! A port of mongod 8.2.11's `CIDR` (`util/net/cidr.cpp`) and the IDL
//! struct `{ranges: [string]}` it parses the parameter into: each range is
//! a CIDR when it parses as one, stored in its canonical form (`127.0.0.1`
//! reads back as `127.0.0.1/32`, `0:0::1` as `::1/128`, the address kept
//! unmasked: `10.1.2.3/8` stays `10.1.2.3/8`), and otherwise kept as the
//! string it was (mongod's `std::variant<CIDR, std::string>`, the string
//! naming a Unix domain socket path), so `{ranges: ["bogus"]}` is accepted
//! and reads back as given. Measured against mongod 8.2.11, 2026-11-15.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use bson::{doc, Bson, Document};

use crate::CommandError;

/// The parameter's name (`transport_options.idl`).
pub const BYPASS: &str = "ingressConnectionEstablishmentRateLimiterBypass";

/// The IDL struct's name, which its parse errors print.
const STRUCT: &str = "CIDRRangeListParameters";

/// One entry of a range list: a CIDR, or a string that is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Range {
    Cidr(IpAddr, u8),
    Other(String),
}

impl Range {
    /// mongod's `CIDR::parse`, falling back to the string.
    pub fn parse(s: &str) -> Range {
        parse_cidr(s).map_or_else(
            || Range::Other(s.to_string()),
            |(ip, len)| Range::Cidr(ip, len),
        )
    }

    /// The form `getParameter` reports (`CIDR::toString`).
    pub fn to_string_form(&self) -> String {
        match self {
            Range::Cidr(ip, len) => format!("{ip}/{len}"),
            Range::Other(s) => s.clone(),
        }
    }

    /// Whether a client at `peer` lies in this range (mongod's
    /// `CIDR::contains`: the same family, and the first `len` bits equal).
    /// A string names a Unix domain socket, which this server does not
    /// listen on, so it matches no client.
    pub fn contains(&self, peer: IpAddr) -> bool {
        let Range::Cidr(ip, len) = self else {
            return false;
        };
        let (a, b): (Vec<u8>, Vec<u8>) = match (ip, peer) {
            (IpAddr::V4(a), IpAddr::V4(b)) => (a.octets().to_vec(), b.octets().to_vec()),
            (IpAddr::V6(a), IpAddr::V6(b)) => (a.octets().to_vec(), b.octets().to_vec()),
            _ => return false,
        };
        let len = usize::from(*len);
        let whole = len / 8;
        if a[..whole] != b[..whole] {
            return false;
        }
        let rest = len % 8;
        if rest == 0 {
            return true;
        }
        let mask = 0xffu8 << (8 - rest);
        (a[whole] & mask) == (b[whole] & mask)
    }
}

/// mongod's `CIDR::parse`: an address, optionally `/` and a prefix length
/// no longer than the address.
fn parse_cidr(s: &str) -> Option<(IpAddr, u8)> {
    let (address, len) = match s.split_once('/') {
        Some((address, len)) => (address, Some(len)),
        None => (s, None),
    };
    let ip = address
        .parse::<Ipv4Addr>()
        .map(IpAddr::V4)
        .or_else(|_| address.parse::<Ipv6Addr>().map(IpAddr::V6))
        .ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let len = match len {
        None => max,
        Some(len) if !len.is_empty() && len.bytes().all(|b| b.is_ascii_digit()) => {
            len.parse::<u8>().ok().filter(|n| *n <= max)?
        }
        Some(_) => return None,
    };
    Some((ip, len))
}

/// Parse a `setParameter` (or startup) value into the ranges, or refuse it
/// with mongod's code and words.
pub fn parse(value: &Bson) -> Result<Vec<Range>, CommandError> {
    let Bson::Document(doc) = value else {
        return Err(CommandError::new(
            10065,
            "Location10065",
            format!("invalid parameter: expected an object ({BYPASS})"),
        ));
    };
    let mut ranges = None;
    for (key, field) in doc {
        if key != "ranges" {
            return Err(CommandError::new(
                40415,
                "IDLUnknownField",
                format!("BSON field '{STRUCT}.{key}' is an unknown field."),
            ));
        }
        let Bson::Array(items) = field else {
            return Err(type_mismatch(&format!("{STRUCT}.ranges"), field, "array"));
        };
        let mut parsed = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let Bson::String(s) = item else {
                return Err(type_mismatch(
                    &format!("{STRUCT}.ranges.{i}"),
                    item,
                    "string",
                ));
            };
            parsed.push(Range::parse(s));
        }
        ranges = Some(parsed);
    }
    ranges.ok_or_else(|| {
        CommandError::new(
            40414,
            "IDLFailedToParse",
            format!("BSON field '{STRUCT}.ranges' is missing but a required field"),
        )
    })
}

fn type_mismatch(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
            secantus_core::query::bson_type_name(value)
        ),
    )
}

/// The value `getParameter` reports for `ranges`.
pub fn to_document(ranges: &[Range]) -> Document {
    let list: Vec<Bson> = ranges
        .iter()
        .map(|r| Bson::String(r.to_string_form()))
        .collect();
    doc! {"ranges": list}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(s: &str) -> String {
        Range::parse(s).to_string_form()
    }

    /// The forms mongod 8.2.11 read back (2026-11-15).
    #[test]
    fn a_range_reads_back_as_mongod_stores_it() {
        for (given, stored) in [
            ("127.0.0.1", "127.0.0.1/32"),
            ("10.0.0.0/8", "10.0.0.0/8"),
            ("10.1.2.3/8", "10.1.2.3/8"),
            ("127.0.0.1/0", "127.0.0.1/0"),
            ("::1/128", "::1/128"),
            ("0:0::1", "::1/128"),
            ("FE80::1/10", "fe80::1/10"),
            ("::ffff:127.0.0.1", "::ffff:127.0.0.1/128"),
            ("127.0.0.1/33", "127.0.0.1/33"),
            ("10.0.0.1/40", "10.0.0.1/40"),
            (" 127.0.0.1", " 127.0.0.1"),
            ("bogus", "bogus"),
            ("", ""),
            ("/tmp/mongodb-27017.sock", "/tmp/mongodb-27017.sock"),
        ] {
            assert_eq!(canonical(given), stored, "{given}");
        }
    }

    #[test]
    fn a_range_contains_the_addresses_under_its_prefix() {
        let r = Range::parse("10.1.2.3/8");
        assert!(r.contains("10.200.0.1".parse().unwrap()));
        assert!(!r.contains("11.0.0.1".parse().unwrap()));
        let r = Range::parse("192.168.1.0/23");
        assert!(r.contains("192.168.0.9".parse().unwrap()));
        assert!(!r.contains("192.168.2.1".parse().unwrap()));
        assert!(Range::parse("0.0.0.0/0").contains("8.8.8.8".parse().unwrap()));
        assert!(!Range::parse("127.0.0.1").contains("::1".parse().unwrap()));
        assert!(Range::parse("::1").contains("::1".parse().unwrap()));
        assert!(!Range::parse("bogus").contains("127.0.0.1".parse().unwrap()));
    }

    /// mongod 8.2.11's refusals, code and words (2026-11-15).
    #[test]
    fn a_malformed_list_is_refused_in_mongods_words() {
        let err = |v: Bson| {
            let e = parse(&v).unwrap_err();
            (e.code, e.errmsg)
        };
        assert_eq!(
            err(Bson::String("x".into())),
            (
                10065,
                format!("invalid parameter: expected an object ({BYPASS})")
            )
        );
        assert_eq!(err(Bson::Document(doc! {})).0, 40414);
        assert_eq!(err(Bson::Array(vec![])).0, 10065);
        assert_eq!(
            err(Bson::Document(doc! {"ranges": "a"})),
            (
                14,
                "BSON field 'CIDRRangeListParameters.ranges' is the wrong type 'string', \
                 expected type 'array'"
                    .to_string()
            )
        );
        assert_eq!(
            err(Bson::Document(doc! {"ranges": [1]})),
            (
                14,
                "BSON field 'CIDRRangeListParameters.ranges.0' is the wrong type 'int', \
                 expected type 'string'"
                    .to_string()
            )
        );
        assert_eq!(
            err(Bson::Document(doc! {"ranges": ["127.0.0.1/32"], "x": 1})),
            (
                40415,
                "BSON field 'CIDRRangeListParameters.x' is an unknown field.".to_string()
            )
        );
    }
}
