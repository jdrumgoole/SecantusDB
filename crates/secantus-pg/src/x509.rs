//! The few facts this server reads out of an X.509 certificate: the hash its
//! signature uses (for SCRAM channel binding), and a client certificate's
//! common name, names and serial number (for `cert` authentication and
//! `pg_stat_ssl`).
//!
//! A reader over DER, not a validator. rustls has already verified a client
//! certificate against the CA before any of this runs, and the server's own
//! certificate comes from the operator. Every function returns `None` on
//! input it cannot follow; none of them panics or reads past the slice.

use sha2::Digest;

/// One DER element: its tag, its contents, and the whole encoding.
#[derive(Clone, Copy)]
struct Tlv<'a> {
    tag: u8,
    body: &'a [u8],
    raw: &'a [u8],
}

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const OID: u8 = 0x06;
/// `[0] EXPLICIT`, the optional version at the head of a TBSCertificate.
const VERSION: u8 = 0xa0;

/// The element at the head of `input`, and what follows it.
fn read(input: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let (&tag, after_tag) = input.split_first()?;
    // A tag number past 30 takes more octets; no field read here has one.
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, after_len) = after_tag.split_first()?;
    let (len, header) = if first < 0x80 {
        (usize::from(first), 2)
    } else {
        let count = usize::from(first & 0x7f);
        // 0 is the indefinite form, which DER forbids.
        if count == 0 || count > 4 || after_len.len() < count {
            return None;
        }
        let len = after_len[..count]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        (len, 2 + count)
    };
    let end = header.checked_add(len)?;
    if input.len() < end {
        return None;
    }
    let tlv = Tlv {
        tag,
        body: &input[header..end],
        raw: &input[..end],
    };
    Some((tlv, &input[end..]))
}

/// The next element of `input`, which must carry `tag`.
fn expect(input: &[u8], tag: u8) -> Option<(Tlv<'_>, &[u8])> {
    let (tlv, rest) = read(input)?;
    (tlv.tag == tag).then_some((tlv, rest))
}

/// An OBJECT IDENTIFIER's contents as dotted decimal.
fn oid_text(body: &[u8]) -> Option<String> {
    let mut arcs: Vec<u64> = Vec::new();
    let mut value: u64 = 0;
    let mut open = false;
    for &b in body {
        value = value.checked_mul(128)?.checked_add(u64::from(b & 0x7f))?;
        open = b & 0x80 != 0;
        if !open {
            if arcs.is_empty() {
                let first = (value / 40).min(2);
                arcs.push(first);
                arcs.push(value - first * 40);
            } else {
                arcs.push(value);
            }
            value = 0;
        }
    }
    if open || arcs.is_empty() {
        return None;
    }
    Some(
        arcs.iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

/// The parts of a certificate read here.
struct Certificate<'a> {
    serial: &'a [u8],
    issuer: &'a [u8],
    subject: &'a [u8],
    signature_algorithm: String,
}

fn certificate(der: &[u8]) -> Option<Certificate<'_>> {
    let (cert, _) = expect(der, SEQUENCE)?;
    let (tbs, after_tbs) = expect(cert.body, SEQUENCE)?;
    let (algorithm, _) = expect(after_tbs, SEQUENCE)?;
    let (algorithm_oid, _) = expect(algorithm.body, OID)?;

    let mut fields = tbs.body;
    if let Some((_, rest)) = expect(fields, VERSION) {
        fields = rest;
    }
    let (serial, fields) = expect(fields, INTEGER)?;
    let (_, fields) = expect(fields, SEQUENCE)?; // signature
    let (issuer, fields) = expect(fields, SEQUENCE)?;
    let (_, fields) = expect(fields, SEQUENCE)?; // validity
    let (subject, _) = expect(fields, SEQUENCE)?;
    Some(Certificate {
        serial: serial.body,
        issuer: issuer.body,
        subject: subject.body,
        signature_algorithm: oid_text(algorithm_oid.body)?,
    })
}

/// `tls-server-end-point` (RFC 5929) for a server certificate: the
/// certificate hashed with its signature's own hash, or with SHA-256 where
/// that hash is MD5 or SHA-1.
///
/// `None` for a signature whose hash is not named by its algorithm (RSA-PSS,
/// Ed25519) or is not one of these; the server then offers no channel
/// binding.
pub(crate) fn end_point_hash(der: &[u8]) -> Option<Vec<u8>> {
    let cert = certificate(der)?;
    match cert.signature_algorithm.as_str() {
        // md5WithRSA, sha1WithRSA, sha256WithRSA; ecdsa-with-SHA1, -SHA256.
        "1.2.840.113549.1.1.4"
        | "1.2.840.113549.1.1.5"
        | "1.2.840.113549.1.1.11"
        | "1.2.840.10045.4.1"
        | "1.2.840.10045.4.3.2" => Some(sha2::Sha256::digest(der).to_vec()),
        // sha384WithRSA, ecdsa-with-SHA384.
        "1.2.840.113549.1.1.12" | "1.2.840.10045.4.3.3" => Some(sha2::Sha384::digest(der).to_vec()),
        // sha512WithRSA, ecdsa-with-SHA512.
        "1.2.840.113549.1.1.13" | "1.2.840.10045.4.3.4" => Some(sha2::Sha512::digest(der).to_vec()),
        _ => None,
    }
}

/// What a client certificate says about its holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Peer {
    /// The subject's first common name, which `cert` authentication and
    /// `clientcert=verify-full` compare with the user name.
    pub common_name: Option<String>,
    /// The subject, as `pg_stat_ssl.client_dn` prints it.
    pub subject: String,
    /// The issuer, as `pg_stat_ssl.issuer_dn` prints it.
    pub issuer: String,
    /// The serial number in decimal.
    pub serial: String,
}

pub(crate) fn peer(der: &[u8]) -> Option<Peer> {
    let cert = certificate(der)?;
    let subject = name(cert.subject)?;
    let issuer = name(cert.issuer)?;
    Some(Peer {
        common_name: subject
            .iter()
            .find(|(oid, _)| oid == COMMON_NAME)
            .and_then(|(_, value)| match value {
                Value::Text(text) => Some(text.clone()),
                Value::Other(_) => None,
            }),
        subject: one_line(&subject),
        issuer: one_line(&issuer),
        serial: integer_text(cert.serial),
    })
}

const COMMON_NAME: &str = "2.5.4.3";

enum Value {
    Text(String),
    /// Not a string type: its whole DER encoding.
    Other(Vec<u8>),
}

/// A Name's attributes in order, a multi-valued RDN's one after another.
fn name(mut rdns: &[u8]) -> Option<Vec<(String, Value)>> {
    let mut out = Vec::new();
    while !rdns.is_empty() {
        let (rdn, rest) = expect(rdns, SET)?;
        rdns = rest;
        let mut attributes = rdn.body;
        while !attributes.is_empty() {
            let (attribute, rest) = expect(attributes, SEQUENCE)?;
            attributes = rest;
            let (oid, after_oid) = expect(attribute.body, OID)?;
            let (value, _) = read(after_oid)?;
            out.push((oid_text(oid.body)?, string_value(value)));
        }
    }
    Some(out)
}

fn string_value(value: Tlv<'_>) -> Value {
    let text = match value.tag {
        // UTF8String, NumericString, PrintableString, IA5String, VisibleString.
        0x0c | 0x12 | 0x13 | 0x16 | 0x1a => String::from_utf8(value.body.to_vec()).ok(),
        // TeletexString, which OpenSSL reads as Latin-1.
        0x14 => Some(value.body.iter().map(|&b| char::from(b)).collect()),
        // BMPString: UTF-16, big-endian.
        0x1e => {
            let (pairs, rest) = value.body.as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|c| u16::from_be_bytes(*c)).collect();
            rest.is_empty()
                .then(|| String::from_utf16(&units).ok())
                .flatten()
        }
        // UniversalString: UTF-32, big-endian.
        0x1c => {
            let (quads, rest) = value.body.as_chunks::<4>();
            let text: Option<String> = quads
                .iter()
                .map(|c| char::from_u32(u32::from_be_bytes(*c)))
                .collect();
            text.filter(|_| rest.is_empty())
        }
        _ => None,
    };
    match text {
        Some(text) => Value::Text(text),
        None => Value::Other(value.raw.to_vec()),
    }
}

/// OpenSSL's short name for an attribute type, which is what PostgreSQL
/// prints. An attribute outside this table prints as `UNDEF`, where OpenSSL
/// knows more names than these.
fn short_name(oid: &str) -> &'static str {
    match oid {
        "2.5.4.3" => "CN",
        "2.5.4.4" => "SN",
        "2.5.4.5" => "serialNumber",
        "2.5.4.6" => "C",
        "2.5.4.7" => "L",
        "2.5.4.8" => "ST",
        "2.5.4.9" => "street",
        "2.5.4.10" => "O",
        "2.5.4.11" => "OU",
        "2.5.4.12" => "title",
        "2.5.4.13" => "description",
        "2.5.4.17" => "postalCode",
        "2.5.4.42" => "GN",
        "2.5.4.43" => "initials",
        "2.5.4.46" => "dnQualifier",
        "1.2.840.113549.1.9.1" => "emailAddress",
        "0.9.2342.19200300.100.1.1" => "UID",
        "0.9.2342.19200300.100.1.25" => "DC",
        _ => "UNDEF",
    }
}

/// A name as PostgreSQL's `X509_NAME_to_cstring` prints it: `/CN=...` for
/// each attribute, the value escaped as RFC 2253 has it.
fn one_line(attributes: &[(String, Value)]) -> String {
    let mut out = String::new();
    for (oid, value) in attributes {
        out.push('/');
        out.push_str(short_name(oid));
        out.push('=');
        match value {
            Value::Text(text) => escape_into(text, &mut out),
            Value::Other(der) => {
                out.push('#');
                for b in der {
                    out.push_str(&format!("{b:02X}"));
                }
            }
        }
    }
    out
}

fn escape_into(text: &str, out: &mut String) {
    let last = text.chars().count().saturating_sub(1);
    for (i, c) in text.chars().enumerate() {
        let special = matches!(c, ',' | '+' | '"' | '\\' | '<' | '>' | ';')
            || (i == 0 && matches!(c, '#' | ' '))
            || (i == last && c == ' ');
        if special {
            out.push('\\');
            out.push(c);
        } else if (c as u32) < 0x20 || c as u32 == 0x7f {
            out.push_str(&format!("\\{:02X}", c as u32));
        } else {
            out.push(c);
        }
    }
}

/// A DER INTEGER's contents in decimal.
fn integer_text(body: &[u8]) -> String {
    let negative = body.first().is_some_and(|b| b & 0x80 != 0);
    let mut magnitude: Vec<u8> = body.to_vec();
    if negative {
        // Two's complement: invert, then add one.
        let mut carry = true;
        for b in magnitude.iter_mut().rev() {
            let (v, c) = (!*b).overflowing_add(u8::from(carry));
            *b = v;
            carry = c;
        }
    }
    let mut digits: Vec<u8> = Vec::new();
    while magnitude.iter().any(|&b| b != 0) {
        let mut remainder: u32 = 0;
        for b in magnitude.iter_mut() {
            let value = (remainder << 8) | u32::from(*b);
            *b = (value / 10) as u8;
            remainder = value % 10;
        }
        digits.push(b'0' + remainder as u8);
    }
    if digits.is_empty() {
        return "0".to_string();
    }
    if negative {
        digits.push(b'-');
    }
    digits.reverse();
    String::from_utf8(digits).expect("ASCII digits")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if body.len() < 0x80 {
            out.push(body.len() as u8);
        } else {
            out.extend([0x82, (body.len() >> 8) as u8, body.len() as u8]);
        }
        out.extend(body);
        out
    }

    #[test]
    fn an_integer_prints_in_decimal() {
        assert_eq!(integer_text(&[0x00]), "0");
        assert_eq!(integer_text(&[0x7f]), "127");
        assert_eq!(integer_text(&[0x00, 0x80]), "128");
        assert_eq!(integer_text(&[0x01, 0x00, 0x00]), "65536");
        assert_eq!(integer_text(&[0xff]), "-1");
        assert_eq!(integer_text(&[0x80]), "-128");
        // 2^64, one past what a machine word holds.
        assert_eq!(
            integer_text(&[1, 0, 0, 0, 0, 0, 0, 0, 0]),
            "18446744073709551616"
        );
    }

    #[test]
    fn an_object_identifier_prints_dotted() {
        assert_eq!(oid_text(&[0x55, 0x04, 0x03]).as_deref(), Some("2.5.4.3"));
        assert_eq!(
            oid_text(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]).as_deref(),
            Some("1.2.840.113549.1.1.11")
        );
        // The last arc is left open.
        assert_eq!(oid_text(&[0x55, 0x84]), None);
        assert_eq!(oid_text(&[]), None);
    }

    #[test]
    fn a_name_prints_as_postgresql_prints_it() {
        let attribute = |oid: &[u8], tag: u8, value: &[u8]| {
            tlv(
                SET,
                &tlv(SEQUENCE, &[tlv(OID, oid), tlv(tag, value)].concat()),
            )
        };
        let der = [
            attribute(&[0x55, 0x04, 0x0a], 0x0c, b"Acme, Inc."),
            attribute(&[0x55, 0x04, 0x03], 0x13, b" alice "),
            attribute(&[0x55, 0x04, 0x03], 0x0c, "zo\u{eb}".as_bytes()),
            attribute(&[0x2b, 0x06, 0x01], 0x0c, b"x"),
            attribute(&[0x55, 0x04, 0x0b], 0x02, &[0x05]),
        ]
        .concat();
        let attributes = name(&der).expect("a name");
        assert_eq!(
            one_line(&attributes),
            "/O=Acme\\, Inc./CN=\\ alice\\ /CN=zo\u{eb}/UNDEF=x/OU=#020105"
        );
    }

    #[test]
    fn a_real_certificate_is_read() {
        let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("generate a certificate");
        let der: &[u8] = made.cert.der();
        // rcgen signs with ECDSA over SHA-256, so that is the hash.
        assert_eq!(
            end_point_hash(der),
            Some(sha2::Sha256::digest(der).to_vec())
        );
        let peer = peer(der).expect("a certificate");
        assert_eq!(peer.common_name.as_deref(), Some("rcgen self signed cert"));
        assert_eq!(peer.subject, "/CN=rcgen self signed cert");
        assert_eq!(peer.issuer, peer.subject);
        assert!(
            peer.serial.bytes().all(|b| b.is_ascii_digit()),
            "{}",
            peer.serial
        );
        // Every prefix of it is refused, and none is read past.
        for cut in 0..der.len() {
            assert!(certificate(&der[..cut]).is_none(), "cut at {cut}");
        }
    }

    #[test]
    fn truncated_input_is_refused_not_read_past() {
        let whole = tlv(SEQUENCE, &tlv(INTEGER, &[1, 2, 3]));
        for cut in 0..whole.len() {
            assert!(read(&whole[..cut]).is_none(), "cut at {cut}");
        }
        assert!(read(&whole).is_some());
        // A length that claims more octets than a usize, and the indefinite form.
        assert!(read(&[SEQUENCE, 0x85, 1, 1, 1, 1, 1]).is_none());
        assert!(read(&[SEQUENCE, 0x80, 0, 0]).is_none());
        assert!(certificate(&whole).is_none());
        assert!(peer(&[]).is_none());
        assert!(end_point_hash(&[0x30]).is_none());
    }
}
