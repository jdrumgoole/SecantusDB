//! Certificate revocation lists, checked as OpenSSL checks them for
//! PostgreSQL's `ssl_crl_file`.
//!
//! PostgreSQL sets `X509_V_FLAG_CRL_CHECK | X509_V_FLAG_CRL_CHECK_ALL`, so
//! every certificate in a client's chain, short of the trust anchor, needs a
//! list from its issuer. Measured against PostgreSQL 15.19:
//!
//! - a certificate a list names fails the handshake, "certificate revoked";
//! - one whose issuer has only a list past its next update fails,
//!   "certificate expired", whether or not the list names it;
//! - one whose issuer has no list at all fails, "unknown ca";
//! - one whose issuer's list is dated in the future fails, "bad
//!   certificate", and one whose issuer's list is not signed by the issuer
//!   fails, "decrypt error", even when a second list for that issuer is
//!   sound: OpenSSL picks one list per issuer and does not try another.
//!
//! rustls can make this check itself, but it reads only a list that carries
//! a CRL number extension. `openssl ca -gencrl` writes a list with no
//! extensions unless `crl_extensions` is configured, and OpenSSL, and so
//! PostgreSQL, reads that. Hence a reader here.
//!
//! A list is trusted only once its signature verifies under the key of a
//! certificate whose subject is the list's issuer: a CA from the CA file, or
//! an intermediate the client sent (which rustls has by then verified).
//!
//! A list's issuing distribution point (`2.5.29.28`) scopes it as OpenSSL
//! scopes it (`crl_crldp_check`), PostgreSQL setting no
//! `X509_V_FLAG_EXTENDED_CRL_SUPPORT`, measured on 15.19:
//!
//! - a list for some revocation reasons only, or an indirect one, is never
//!   used: the certificate's issuer has no list ("unknown ca");
//! - `onlyContainsUserCerts` / `onlyContainsCACerts` keep a list to the
//!   one kind of certificate, and a distribution point name keeps it to a
//!   certificate whose CRL distribution points name the same point; a
//!   certificate the chosen list does not cover fails, "certificate
//!   unknown" (`X509_V_ERR_DIFFERENT_CRL_SCOPE`);
//! - a delta list (`2.5.29.27`) is read as a full one.
//!
//! Any other critical extension is refused when the file is read, as
//! OpenSSL refuses a critical extension it does not handle.

use std::collections::HashSet;

use tokio_rustls::rustls::crypto::WebPkiSupportedAlgorithms;

use crate::x509::{self, expect, read, INTEGER, OID, SEQUENCE};

const BIT_STRING: u8 = 0x03;
const BOOLEAN: u8 = 0x01;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
/// `[0] EXPLICIT`, a list's extensions.
const LIST_EXTENSIONS: u8 = 0xa0;

/// One revocation list.
#[derive(Debug)]
pub(crate) struct RevocationList {
    /// The issuer Name's contents.
    issuer: Vec<u8>,
    /// The signed part, whole.
    signed: Vec<u8>,
    /// The signature AlgorithmIdentifier's contents.
    signature_algorithm: Vec<u8>,
    signature: Vec<u8>,
    /// Unix seconds.
    this_update: i64,
    next_update: Option<i64>,
    /// The serial numbers it names: each an INTEGER's contents.
    revoked: HashSet<Vec<u8>>,
    /// What its issuing distribution point keeps it to.
    scope: Scope,
}

/// An issuing distribution point's scope.
#[derive(Debug, Default)]
struct Scope {
    /// A list for some reasons only, or an indirect one: never used.
    unusable: bool,
    only_user: bool,
    only_ca: bool,
    only_attribute: bool,
    /// The distribution point's full name: each GeneralName, whole.
    points: Option<Vec<Vec<u8>>>,
}

const IDP: &[u8] = &[0x55, 0x1d, 0x1c];
const DELTA_CRL_INDICATOR: &[u8] = &[0x55, 0x1d, 0x1b];
const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const CRL_DISTRIBUTION_POINTS: &[u8] = &[0x55, 0x1d, 0x1f];

/// A DistributionPointName's full name, as its GeneralNames, whole; `None`
/// for a name relative to the issuer, which is not read here.
fn full_name(point: &[u8]) -> Option<Vec<Vec<u8>>> {
    let (name, _) = expect(point, 0xa0)?;
    let mut names = Vec::new();
    let mut rest = name.body;
    while let Some((general, after)) = read(rest) {
        names.push(general.raw.to_vec());
        rest = after;
    }
    Some(names)
}

fn idp_scope(value: &[u8]) -> Result<Scope, String> {
    let bad = || "its issuing distribution point is not well-formed".to_string();
    let (idp, _) = expect(value, SEQUENCE).ok_or_else(bad)?;
    let mut scope = Scope::default();
    let mut rest = idp.body;
    while let Some((field, after)) = read(rest) {
        rest = after;
        let flag = field.body != [0];
        match field.tag {
            0xa0 => {
                scope.points = Some(full_name(field.body).ok_or_else(|| {
                    "its issuing distribution point is named relative to its issuer, \
                     which is not supported"
                        .to_string()
                })?)
            }
            0x81 => scope.only_user = flag,
            0x82 => scope.only_ca = flag,
            0x83 => scope.unusable = true,
            0x84 => scope.unusable |= flag,
            0x85 => scope.only_attribute = flag,
            _ => return Err(bad()),
        }
    }
    Ok(scope)
}

impl Scope {
    /// Does a list of this scope cover the certificate with these
    /// extensions (`crl_crldp_check`)?
    fn covers(&self, extensions: &[u8]) -> bool {
        if self.only_attribute {
            return false;
        }
        let is_ca = x509::extension(extensions, BASIC_CONSTRAINTS)
            .and_then(|(_, v)| expect(v, SEQUENCE))
            .and_then(|(seq, _)| expect(seq.body, 0x01))
            .is_some_and(|(flag, _)| flag.body != [0]);
        if (is_ca && self.only_user) || (!is_ca && self.only_ca) {
            return false;
        }
        // A distribution point of the certificate's with no CRL issuer of
        // its own, and a name the list's point shares (or no names).
        if let Some((_, value)) = x509::extension(extensions, CRL_DISTRIBUTION_POINTS) {
            if let Some((points, _)) = expect(value, SEQUENCE) {
                let mut rest = points.body;
                while let Some((point, after)) = expect(rest, SEQUENCE) {
                    rest = after;
                    let mut name = None;
                    let mut crl_issuer = false;
                    let mut fields = point.body;
                    while let Some((field, more)) = read(fields) {
                        fields = more;
                        match field.tag {
                            0xa0 => name = Some(full_name(field.body)),
                            0xa2 => crl_issuer = true,
                            _ => {}
                        }
                    }
                    if crl_issuer {
                        continue;
                    }
                    let shares = match (&self.points, name) {
                        (None, _) | (_, None) => true,
                        (Some(ours), Some(Some(theirs))) => theirs.iter().any(|n| ours.contains(n)),
                        (Some(_), Some(None)) => false,
                    };
                    if shares {
                        return true;
                    }
                }
            }
        }
        self.points.is_none()
    }
}

/// Why a certificate did not pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// A list names it.
    Revoked,
    /// Its issuer's list is past its next update.
    ListExpired,
    /// Its issuer's list is dated in the future.
    ListNotYetValid,
    /// Its issuer has no list.
    NoList,
    /// Its issuer's list does not carry the issuer's signature.
    BadSignature,
    /// The certificate could not be read.
    Unreadable,
    /// The list chosen for its issuer does not cover it
    /// (`X509_V_ERR_DIFFERENT_CRL_SCOPE`).
    DifferentScope,
}

impl RevocationList {
    /// Read one DER list. The error says what was wrong with it.
    pub(crate) fn parse(der: &[u8]) -> Result<RevocationList, String> {
        let malformed = || "it is not a well-formed revocation list".to_string();
        let (list, _) = expect(der, SEQUENCE).ok_or_else(malformed)?;
        let (signed, rest) = expect(list.body, SEQUENCE).ok_or_else(malformed)?;
        let (algorithm, rest) = expect(rest, SEQUENCE).ok_or_else(malformed)?;
        let (signature, _) = expect(rest, BIT_STRING).ok_or_else(malformed)?;
        // A signature is a whole number of octets: no unused bits.
        let signature = signature
            .body
            .strip_prefix(&[0])
            .ok_or_else(malformed)?
            .to_vec();

        let mut fields = signed.body;
        if let Some((_, rest)) = expect(fields, INTEGER) {
            fields = rest; // version
        }
        let (_, fields) = expect(fields, SEQUENCE).ok_or_else(malformed)?; // signature
        let (issuer, fields) = expect(fields, SEQUENCE).ok_or_else(malformed)?;
        let (this_update, mut fields) = time(fields).ok_or_else(malformed)?;
        let mut next_update = None;
        if let Some((at, rest)) = time(fields) {
            next_update = Some(at);
            fields = rest;
        }
        let mut revoked = HashSet::new();
        if let Some((entries, rest)) = expect(fields, SEQUENCE) {
            fields = rest;
            let mut entries = entries.body;
            while !entries.is_empty() {
                let (entry, rest) = expect(entries, SEQUENCE).ok_or_else(malformed)?;
                entries = rest;
                let (serial, after) = expect(entry.body, INTEGER).ok_or_else(malformed)?;
                let (_, after) = time(after).ok_or_else(malformed)?;
                if let Some((extensions, _)) = expect(after, SEQUENCE) {
                    refuse_critical(extensions.body, "an entry", &[])?;
                }
                revoked.insert(serial.body.to_vec());
            }
        }
        let mut scope = Scope::default();
        if let Some((extensions, _)) = expect(fields, LIST_EXTENSIONS) {
            let (extensions, _) = expect(extensions.body, SEQUENCE).ok_or_else(malformed)?;
            if let Some((_, value)) = x509::extension(extensions.body, IDP) {
                scope = idp_scope(value)?;
            }
            refuse_critical(extensions.body, "it", &[IDP, DELTA_CRL_INDICATOR])?;
        }
        Ok(RevocationList {
            issuer: issuer.body.to_vec(),
            signed: signed.raw.to_vec(),
            signature_algorithm: algorithm.body.to_vec(),
            signature,
            this_update,
            next_update,
            revoked,
            scope,
        })
    }

    fn current(&self, now: i64) -> bool {
        self.this_update <= now && self.next_update.is_none_or(|next| now <= next)
    }

    /// Does this list carry the signature of the holder of `certificate`?
    fn signed_by(&self, certificate: &[u8], algorithms: &WebPkiSupportedAlgorithms) -> bool {
        let Some(issuer) = x509::certificate(certificate) else {
            return false;
        };
        let Some((key_algorithm, rest)) = expect(issuer.public_key_info, SEQUENCE) else {
            return false;
        };
        let Some(key) = expect(rest, BIT_STRING).and_then(|(key, _)| key.body.strip_prefix(&[0]))
        else {
            return false;
        };
        algorithms.all.iter().any(|algorithm| {
            let signature_id = algorithm.signature_alg_id();
            let key_id = algorithm.public_key_alg_id();
            signature_id.as_ref() == self.signature_algorithm.as_slice()
                && key_id.as_ref() == key_algorithm.body
                && algorithm
                    .verify_signature(key, &self.signed, &self.signature)
                    .is_ok()
        })
    }
}

/// A critical extension is one a reader must understand or refuse the whole
/// object. None is understood here.
fn refuse_critical(mut extensions: &[u8], whose: &str, handled: &[&[u8]]) -> Result<(), String> {
    while !extensions.is_empty() {
        let malformed = || "it is not a well-formed revocation list".to_string();
        let (extension, rest) = expect(extensions, SEQUENCE).ok_or_else(malformed)?;
        extensions = rest;
        let (oid, after) = expect(extension.body, OID).ok_or_else(malformed)?;
        if handled.contains(&oid.body) {
            continue;
        }
        if let Some((critical, _)) = expect(after, BOOLEAN) {
            if critical.body != [0] {
                return Err(format!(
                    "{whose} carries the critical extension {}, which is not supported",
                    x509::oid_text(oid.body).unwrap_or_else(|| "?".to_string())
                ));
            }
        }
    }
    Ok(())
}

/// A UTCTime or GeneralizedTime at the head of `input`, in Unix seconds.
/// Both must be in UTC with seconds, as DER has them.
fn time(input: &[u8]) -> Option<(i64, &[u8])> {
    let (value, rest) = read(input)?;
    let text = std::str::from_utf8(value.body).ok()?;
    let digits = text.strip_suffix('Z')?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (year, tail) = match (value.tag, digits.len()) {
        (UTC_TIME, 12) => {
            let yy: i64 = digits[..2].parse().ok()?;
            // RFC 5280: 00-49 is 20xx, 50-99 is 19xx.
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, &digits[2..])
        }
        (GENERALIZED_TIME, 14) => (digits[..4].parse().ok()?, &digits[4..]),
        _ => return None,
    };
    let part = |at: usize| tail[at..at + 2].parse::<i64>().ok();
    let (month, day, hour, minute, second) = (part(0)?, part(2)?, part(4)?, part(6)?, part(8)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second, rest))
}

/// The lists a server checks client certificates against, and the CAs that
/// may have signed them.
#[derive(Debug)]
pub(crate) struct Revocation {
    lists: Vec<RevocationList>,
    /// The CA file's certificates, DER.
    authorities: Vec<Vec<u8>>,
    algorithms: WebPkiSupportedAlgorithms,
}

/// A chain longer than this is not walked further. rustls has verified the
/// chain already; this bounds a loop of certificates that issue one another.
const MAX_CHAIN: usize = 16;

impl Revocation {
    pub(crate) fn new(
        lists: Vec<RevocationList>,
        authorities: Vec<Vec<u8>>,
        algorithms: WebPkiSupportedAlgorithms,
    ) -> Self {
        Revocation {
            lists,
            authorities,
            algorithms,
        }
    }

    /// Check a client's certificate, and each intermediate between it and a
    /// CA in the CA file, at `now` (Unix seconds).
    pub(crate) fn check(
        &self,
        end_entity: &[u8],
        intermediates: &[&[u8]],
        now: i64,
    ) -> Result<(), Refused> {
        let mut current = end_entity;
        for _ in 0..MAX_CHAIN {
            let certificate = x509::certificate(current).ok_or(Refused::Unreadable)?;
            // A trust anchor is not checked: nothing above it issues a list
            // for it.
            if certificate.subject == certificate.issuer {
                return Ok(());
            }
            let issued_by = |candidate: &&[u8]| {
                x509::certificate(candidate).is_some_and(|c| c.subject == certificate.issuer)
            };
            let above: Vec<&[u8]> = intermediates.iter().copied().filter(issued_by).collect();
            let issuers: Vec<&[u8]> = self
                .authorities
                .iter()
                .map(Vec::as_slice)
                .filter(issued_by)
                .chain(above.iter().copied())
                .collect();
            self.check_one(&certificate, &issuers, now)?;
            match above.first() {
                Some(next) => current = next,
                None => return Ok(()),
            }
        }
        Err(Refused::Unreadable)
    }

    fn check_one(
        &self,
        certificate: &x509::Certificate<'_>,
        issuers: &[&[u8]],
        now: i64,
    ) -> Result<(), Refused> {
        let (serial, issuer) = (certificate.serial, certificate.issuer);
        // A list for some reasons only, or an indirect one, is never a
        // candidate.
        let from_issuer: Vec<&RevocationList> = self
            .lists
            .iter()
            .filter(|list| list.issuer == issuer && !list.scope.unusable)
            .collect();
        if from_issuer.is_empty() {
            return Err(Refused::NoList);
        }
        // OpenSSL settles on ONE list per issuer and holds the certificate
        // to it: a list that is in date over one that is not, then the
        // newer, then the first in the file. So a second list never rescues
        // a certificate from the one that was chosen.
        // A list that covers the certificate is preferred to one that does
        // not (`CRL_SCORE_SCOPE` outranks `CRL_SCORE_TIME`).
        let covers = |list: &RevocationList| list.scope.covers(certificate.extensions);
        let mut chosen = from_issuer[0];
        for list in &from_issuer[1..] {
            if (covers(list), list.current(now), list.this_update)
                > (covers(chosen), chosen.current(now), chosen.this_update)
            {
                chosen = list;
            }
        }
        // Its scope, its dates, then its signature, then what it names:
        // OpenSSL's order, and so which failure a client is told about.
        if !covers(chosen) {
            return Err(Refused::DifferentScope);
        }
        if chosen.this_update > now {
            return Err(Refused::ListNotYetValid);
        }
        if !chosen.current(now) {
            return Err(Refused::ListExpired);
        }
        let genuine = issuers
            .iter()
            .any(|issuer| chosen.signed_by(issuer, &self.algorithms));
        if !genuine {
            return Err(Refused::BadSignature);
        }
        if chosen.revoked.contains(serial) {
            return Err(Refused::Revoked);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if body.len() < 0x80 {
            out.push(body.len() as u8);
        } else {
            out.extend_from_slice(&[0x82, (body.len() >> 8) as u8, body.len() as u8]);
        }
        out.extend_from_slice(body);
        out
    }

    fn list(
        version_2: bool,
        entries: &[&[u8]],
        extensions: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Vec<u8> {
        let algorithm = tlv(SEQUENCE, &tlv(OID, &[0x2a, 0x03]));
        let mut signed = Vec::new();
        if version_2 {
            signed.extend(tlv(INTEGER, &[1]));
        }
        signed.extend(&algorithm);
        signed.extend(tlv(SEQUENCE, b"issuer"));
        signed.extend(tlv(UTC_TIME, b"260101000000Z"));
        if let Some(next) = next {
            signed.extend(tlv(GENERALIZED_TIME, next));
        }
        if !entries.is_empty() {
            let body: Vec<u8> = entries
                .iter()
                .flat_map(|serial| {
                    tlv(
                        SEQUENCE,
                        &[tlv(INTEGER, serial), tlv(UTC_TIME, b"260102000000Z")].concat(),
                    )
                })
                .collect();
            signed.extend(tlv(SEQUENCE, &body));
        }
        if let Some(extensions) = extensions {
            signed.extend(tlv(LIST_EXTENSIONS, &tlv(SEQUENCE, extensions)));
        }
        tlv(
            SEQUENCE,
            &[
                tlv(SEQUENCE, &signed),
                algorithm,
                tlv(BIT_STRING, &[0, 9, 9]),
            ]
            .concat(),
        )
    }

    #[test]
    fn a_time_is_read_in_both_forms() {
        let utc = tlv(UTC_TIME, b"700101000000Z");
        assert_eq!(time(&utc).map(|(at, _)| at), Some(0));
        let general = tlv(GENERALIZED_TIME, b"20261011120000Z");
        assert_eq!(time(&general).map(|(at, _)| at), Some(1_791_720_000));
        let utc_2049 = tlv(UTC_TIME, b"490101000000Z");
        assert_eq!(time(&utc_2049).map(|(at, _)| at), Some(2_493_072_000));
        for bad in [
            &b"7001010000Z"[..],
            b"700101000000",
            b"70010100000aZ",
            b"701301000000Z",
        ] {
            assert!(time(&tlv(UTC_TIME, bad)).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_list_with_no_extensions_is_read() {
        // Version 1: no version field, no extensions, no next update.
        let parsed = RevocationList::parse(&list(false, &[&[5], &[0x01, 0x00]], None, None))
            .expect("a version 1 list");
        assert_eq!(parsed.issuer, b"issuer");
        assert!(parsed.revoked.contains(&[5][..]) && parsed.revoked.contains(&[1, 0][..]));
        assert_eq!(parsed.next_update, None);
        assert_eq!(parsed.signature, [9, 9]);
        assert!(parsed.current(i64::MAX), "no next update never expires");
        assert!(!parsed.current(0), "not before its own date");
        // Version 2, nobody revoked, a next update.
        let parsed = RevocationList::parse(&list(true, &[], Some(&[]), Some(b"20270101000000Z")))
            .expect("a version 2 list");
        assert!(parsed.revoked.is_empty());
        assert!(parsed.current(1_791_720_000) && !parsed.current(1_900_000_000));
    }

    #[test]
    fn a_critical_extension_is_refused_and_another_is_not() {
        let extension = |critical: Option<u8>| {
            let mut body = tlv(OID, &[0x2a, 0x03, 0x04]);
            if let Some(flag) = critical {
                body.extend(tlv(BOOLEAN, &[flag]));
            }
            body.extend(tlv(0x04, b"x"));
            tlv(SEQUENCE, &body)
        };
        assert!(RevocationList::parse(&list(true, &[], Some(&extension(None)), None)).is_ok());
        assert!(RevocationList::parse(&list(true, &[], Some(&extension(Some(0))), None)).is_ok());
        let err = RevocationList::parse(&list(true, &[], Some(&extension(Some(0xff))), None))
            .expect_err("critical");
        assert!(err.contains("critical extension 1.2.3.4"), "{err}");
    }

    #[test]
    fn truncated_input_is_refused_not_read_past() {
        let whole = list(true, &[&[5]], Some(&[]), Some(b"20270101000000Z"));
        for cut in 0..whole.len() {
            assert!(
                RevocationList::parse(&whole[..cut]).is_err(),
                "cut at {cut}"
            );
        }
    }
}
