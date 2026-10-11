//! SCRAM channel binding (`SCRAM-SHA-256-PLUS`, RFC 5802 section 6) for a
//! login over TLS, with the checks PostgreSQL's `auth-scram.c` makes and the
//! errors it raises for them.
//!
//! A client that binds the exchange to the TLS channel proves it is talking
//! to the holder of the server certificate, not to something relaying the
//! exchange. The binding data is `tls-server-end-point`: a hash of that
//! certificate (see `x509::end_point_hash`).
//!
//! The exchange itself is `secantus_auth`'s. Its proof covers the client's
//! first message without the GS2 header, and the client-final message
//! whole, `c=` attribute included. So the header is read and checked here,
//! the message handed on with a plain `n,,` header, and the `c=` attribute
//! compared before the proof is.

use base64::Engine;

const PLAIN: &str = "SCRAM-SHA-256";
const PLUS: &str = "SCRAM-SHA-256-PLUS";
const END_POINT: &str = "tls-server-end-point";

/// A login refused during the exchange: PostgreSQL's SQLSTATE, message and
/// DETAIL.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub code: &'static str,
    pub message: String,
    pub detail: Option<&'static str>,
}

fn protocol(message: impl Into<String>, detail: Option<&'static str>) -> Refusal {
    Refusal {
        code: "08P01",
        message: message.into(),
        detail,
    }
}

/// The mechanisms to offer: with channel binding first, as PostgreSQL lists
/// them, when the connection is TLS and the certificate's hash is known.
pub(crate) fn mechanisms(end_point_hash: Option<&[u8]>) -> Vec<String> {
    match end_point_hash {
        Some(_) => vec![PLUS.to_string(), PLAIN.to_string()],
        None => vec![PLAIN.to_string()],
    }
}

/// A checked client-first message.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ClientFirst {
    /// The message with a plain `n,,` header, for `begin_scram_pg`.
    pub message: Vec<u8>,
    /// What the client-final message's `c=` attribute must be.
    pub expected_binding: String,
    /// Whether a mismatch there is a failed binding check or a malformed
    /// message.
    pub bound: bool,
}

/// Check the mechanism a client selected and its first message's GS2 header.
///
/// `end_point_hash` is the server certificate's hash when this connection
/// was offered channel binding, `None` when it was not (no TLS).
pub(crate) fn client_first(
    mechanism: &str,
    data: &[u8],
    end_point_hash: Option<&[u8]>,
) -> Result<ClientFirst, Refusal> {
    let bound = match mechanism {
        PLAIN => false,
        PLUS if end_point_hash.is_some() => true,
        _ => {
            return Err(protocol(
                "client selected an invalid SASL authentication mechanism",
                None,
            ))
        }
    };
    const NO_BINDING_DATA: &str = "The client selected SCRAM-SHA-256-PLUS, but the SCRAM \
         message does not include channel binding data.";
    let malformed = || protocol("malformed SCRAM message", None);
    let b64 = base64::engine::general_purpose::STANDARD;
    let (header, expected): (&[u8], String) = match data.first() {
        Some(b'n') | Some(b'y') if bound => {
            return Err(protocol("malformed SCRAM message", Some(NO_BINDING_DATA)))
        }
        Some(b'n') => (b"n,,", b64.encode("n,,")),
        // "I can bind, but you did not offer it." True of a connection that
        // was not offered it; on one that was, something between the two
        // removed the offer.
        Some(b'y') => {
            if end_point_hash.is_some() {
                return Err(Refusal {
                    code: "28000",
                    message: "SCRAM channel binding negotiation error".into(),
                    detail: Some(
                        "The client supports SCRAM channel binding but thinks the server does \
                         not.  However, this server does support channel binding.",
                    ),
                });
            }
            (b"y,,", b64.encode("y,,"))
        }
        Some(b'p') => {
            if !bound {
                return Err(protocol(
                    "malformed SCRAM message",
                    Some(
                        "The client selected SCRAM-SHA-256 without channel binding, but the \
                         SCRAM message includes channel binding data.",
                    ),
                ));
            }
            let rest = data.strip_prefix(b"p=").ok_or_else(malformed)?;
            let comma = rest.iter().position(|&b| b == b',').ok_or_else(malformed)?;
            let kind = String::from_utf8_lossy(&rest[..comma]);
            if kind != END_POINT {
                return Err(protocol(
                    format!("unsupported SCRAM channel-binding type \"{kind}\""),
                    None,
                ));
            }
            // `p=<type>,` and then the empty authorization identity's comma.
            let header = data.get(..2 + comma + 2).ok_or_else(malformed)?;
            if !header.ends_with(b",,") {
                return Err(malformed());
            }
            let mut bound_data = header.to_vec();
            bound_data.extend_from_slice(end_point_hash.unwrap_or_default());
            (header, b64.encode(bound_data))
        }
        _ => return Err(malformed()),
    };
    // The header ends with an empty authorization identity: `,,`.
    let bare = data.strip_prefix(header).ok_or_else(malformed)?;
    let mut message = b"n,,".to_vec();
    message.extend_from_slice(bare);
    Ok(ClientFirst {
        message,
        expected_binding: expected,
        bound,
    })
}

/// Check a client-final message's `c=` attribute against what its first
/// message committed to. PostgreSQL makes this check before the proof.
pub(crate) fn client_final(data: &[u8], first: &ClientFirst) -> Result<(), Refusal> {
    let sent = data
        .split(|&b| b == b',')
        .find_map(|attribute| attribute.strip_prefix(b"c="));
    if sent == Some(first.expected_binding.as_bytes()) {
        return Ok(());
    }
    Err(if first.bound {
        Refusal {
            code: "28000",
            message: "SCRAM channel binding check failed".into(),
            detail: None,
        }
    } else {
        protocol(
            "unexpected SCRAM channel-binding attribute in client-final-message",
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &[u8] = &[7u8; 32];

    #[test]
    fn binding_is_offered_only_with_a_certificate_hash() {
        assert_eq!(mechanisms(Some(HASH)), [PLUS, PLAIN]);
        assert_eq!(mechanisms(None), [PLAIN]);
    }

    #[test]
    fn a_bound_first_message_is_handed_on_without_its_header() {
        let first = client_first(PLUS, b"p=tls-server-end-point,,n=,r=abc", Some(HASH)).unwrap();
        assert_eq!(first.message, b"n,,n=,r=abc");
        assert!(first.bound);
        let mut data = b"p=tls-server-end-point,,".to_vec();
        data.extend_from_slice(HASH);
        let c = base64::engine::general_purpose::STANDARD.encode(data);
        assert_eq!(first.expected_binding, c);
        assert_eq!(
            client_final(format!("c={c},r=abcdef,p=xx").as_bytes(), &first),
            Ok(())
        );
        let wrong = client_final(b"c=biws,r=abcdef,p=xx", &first).unwrap_err();
        assert_eq!(wrong.code, "28000");
        assert_eq!(wrong.message, "SCRAM channel binding check failed");
    }

    #[test]
    fn an_unbound_exchange_must_repeat_its_own_header() {
        let n = client_first(PLAIN, b"n,,n=,r=abc", None).unwrap();
        assert_eq!(n.expected_binding, "biws");
        assert_eq!(client_final(b"c=biws,r=x,p=y", &n), Ok(()));
        let y = client_first(PLAIN, b"y,,n=,r=abc", None).unwrap();
        assert_eq!(y.expected_binding, "eSws");
        assert_eq!(y.message, b"n,,n=,r=abc");
        let crossed = client_final(b"c=eSws,r=x,p=y", &n).unwrap_err();
        assert_eq!(crossed.code, "08P01");
        assert!(client_final(b"r=x,p=y", &n).is_err());
    }

    #[test]
    fn a_client_that_thinks_binding_was_not_offered_is_refused_when_it_was() {
        let err = client_first(PLAIN, b"y,,n=,r=abc", Some(HASH)).unwrap_err();
        assert_eq!(err.code, "28000");
        assert_eq!(err.message, "SCRAM channel binding negotiation error");
    }

    #[test]
    fn the_mechanism_and_the_header_must_agree() {
        for data in [&b"n,,n=,r=a"[..], b"y,,n=,r=a"] {
            let err = client_first(PLUS, data, Some(HASH)).unwrap_err();
            assert_eq!(err.code, "08P01");
            assert!(err.detail.unwrap().contains("does not include"));
        }
        let err = client_first(PLAIN, b"p=tls-server-end-point,,n=,r=a", Some(HASH)).unwrap_err();
        assert!(err.detail.unwrap().contains("without channel binding"));
        let err = client_first(PLUS, b"p=tls-unique,,n=,r=a", Some(HASH)).unwrap_err();
        assert_eq!(
            err.message,
            "unsupported SCRAM channel-binding type \"tls-unique\""
        );
        // Not offered on this connection, so not a mechanism at all.
        let err = client_first(PLUS, b"p=tls-server-end-point,,n=,r=a", None).unwrap_err();
        assert_eq!(
            err.message,
            "client selected an invalid SASL authentication mechanism"
        );
        assert!(client_first("SCRAM-SHA-1", b"n,,n=,r=a", None).is_err());
        // No header, an unknown flag, and an authorization identity.
        for data in [&b""[..], b"x,,n=,r=a", b"n,a=me,n=,r=a"] {
            assert!(client_first(PLAIN, data, None).is_err());
        }
        // Cut short at each point of the header: refused, never read past.
        let whole = b"p=tls-server-end-point,,n=,r=a";
        for cut in 0..24 {
            assert!(
                client_first(PLUS, &whole[..cut], Some(HASH)).is_err(),
                "cut at {cut}"
            );
        }
        assert!(client_first(PLUS, &whole[..24], Some(HASH)).is_ok());
        assert!(client_first(PLUS, b"p=tls-server-end-point,a=me,n=", Some(HASH)).is_err());
    }
}
