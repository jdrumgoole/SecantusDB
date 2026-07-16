//! `secantus-auth` — the SCRAM-SHA-256 server-side mechanism (R5).
//!
//! A faithful, pure-Rust port of the SCRAM core of `src/secantus/auth.py`
//! (RFC 5802 / RFC 7677): credential derivation and the
//! `begin_scram` → `continue_scram` server handshake. The auth *command*
//! handlers (`saslStart` / `saslContinue` / `createUser` / …), user storage,
//! and per-connection auth state wire this in (R5b+).
//!
//! **Scope:** SCRAM-SHA-256 only (the modern driver default). SCRAM-SHA-1 (with
//! MongoDB's legacy MD5 prepass) and MONGODB-X509 (TLS, R5 tail) are deferred.
//!
//! **`saslprep`:** RFC 4013 SASLprep is applied to every password (mapping
//! table B.1 → nothing and C.1.2 → ASCII space, NFKC normalisation, the
//! prohibited-table + bidirectional checks). ASCII passwords are unaffected,
//! and non-ASCII passwords hash identically to a SASLprep-compliant client
//! (pymongo) and to the Python server's `auth.saslprep`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// MongoDB's default SCRAM-SHA-256 iteration count.
pub const DEFAULT_ITERATIONS: u32 = 15_000;
/// Server-nonce length in bytes (base64-encoded into the nonce string).
const NONCE_BYTES: usize = 24;
/// Salt length in bytes for SCRAM-SHA-256 (RFC 7677).
const SALT_BYTES: usize = 28;

type HmacSha256 = Hmac<Sha256>;

/// A SCRAM authentication failure. The `Display` text is internal; the command
/// layer maps it to `AuthenticationFailed` (18) on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthError(pub String);

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for AuthError {}

fn err(msg: &str) -> AuthError {
    AuthError(msg.to_string())
}

/// The stored SCRAM credentials for a user (RFC 5802 §3). The plaintext password
/// never persists — only the salt, iteration count, and derived keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredentials {
    pub iteration_count: u32,
    pub salt: Vec<u8>,
    pub stored_key: Vec<u8>,
    pub server_key: Vec<u8>,
}

/// SASLprep a password per RFC 4013 (a stringprep profile): map table B.1 to
/// nothing and C.1.2 to ASCII space, NFKC-normalise, then enforce the
/// prohibited-character tables and the bidirectional check. ASCII passwords are
/// the identity; non-ASCII passwords are normalised so they hash identically to
/// a compliant client. A prohibited character or a bidi violation is an
/// `AuthError`. Mirrors `secantus.auth.saslprep`.
fn saslprep(password: &str) -> Result<Vec<u8>, AuthError> {
    stringprep::saslprep(password)
        .map(|s| s.into_owned().into_bytes())
        .map_err(|e| err(&format!("SASLprep: {e}")))
}

/// Derive SCRAM-SHA-256 credentials from a plaintext password.
///
/// `SaltedPassword = PBKDF2(HMAC-SHA-256, SASLprep(password), salt, iterations)`,
/// `ClientKey = HMAC(SaltedPassword, "Client Key")`,
/// `StoredKey = SHA-256(ClientKey)`,
/// `ServerKey = HMAC(SaltedPassword, "Server Key")`.
///
/// `salt` / `iterations` default to a fresh 28-byte salt and 15000 when `None`.
///
/// Errors only when the password fails RFC 4013 SASLprep (a prohibited
/// character or a bidirectional-check violation).
pub fn derive_credentials(
    password: &str,
    iterations: Option<u32>,
    salt: Option<Vec<u8>>,
) -> Result<StoredCredentials, AuthError> {
    let prepped = saslprep(password)?;
    let iterations = iterations.unwrap_or(DEFAULT_ITERATIONS);
    let salt = salt.unwrap_or_else(|| {
        let bytes: [u8; SALT_BYTES] = rand::random();
        bytes.to_vec()
    });
    let mut salted = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(&prepped, &salt, iterations, &mut salted);
    let client_key = hmac(&salted, b"Client Key");
    let stored_key = Sha256::digest(&client_key).to_vec();
    let server_key = hmac(&salted, b"Server Key");
    Ok(StoredCredentials {
        iteration_count: iterations,
        salt,
        stored_key,
        server_key,
    })
}

impl StoredCredentials {
    /// Base64 of the salt / stored key / server key, for the stored user record
    /// (`{SCRAM-SHA-256: {iterationCount, salt, storedKey, serverKey}}`).
    pub fn salt_b64(&self) -> String {
        B64.encode(&self.salt)
    }
    pub fn stored_key_b64(&self) -> String {
        B64.encode(&self.stored_key)
    }
    pub fn server_key_b64(&self) -> String {
        B64.encode(&self.server_key)
    }

    /// Reconstruct credentials from the stored base64 record fields.
    pub fn from_b64(
        iteration_count: u32,
        salt_b64: &str,
        stored_key_b64: &str,
        server_key_b64: &str,
    ) -> Result<Self, AuthError> {
        Ok(StoredCredentials {
            iteration_count,
            salt: B64
                .decode(salt_b64)
                .map_err(|_| err("invalid stored salt base64"))?,
            stored_key: B64
                .decode(stored_key_b64)
                .map_err(|_| err("invalid storedKey base64"))?,
            server_key: B64
                .decode(server_key_b64)
                .map_err(|_| err("invalid serverKey base64"))?,
        })
    }
}

/// Per-conversation SCRAM state across one `saslStart` → `saslContinue`.
#[derive(Debug, Clone)]
pub struct ScramState {
    pub conversation_id: i32,
    pub db_name: String,
    pub username: String,
    pub creds: StoredCredentials,
    pub server_nonce: String,
    pub client_first_bare: Vec<u8>,
    pub server_first: Vec<u8>,
    /// Whether the looked-up user actually existed (the proof still runs when
    /// not, for constant-time-ish behaviour, then fails — matching mongod).
    pub user_exists: bool,
    pub step: u8,
}

/// Process a SCRAM client-first message → `(server-first payload, state)`.
///
/// `creds` is the looked-up credentials for `(db, user)`, or `None` if the user
/// doesn't exist — in which case fabricated credentials let the handshake finish
/// and fail only at the proof step (mongod's timing).
pub fn begin_scram(
    conversation_id: i32,
    db_name: &str,
    payload: &[u8],
    creds: Option<StoredCredentials>,
) -> Result<(Vec<u8>, ScramState), AuthError> {
    if !payload.starts_with(b"n,") {
        return Err(err("invalid SCRAM client-first: expected GS2 header 'n,,'"));
    }
    // Skip the GS2 header ("n,," — no channel binding / authzid) to the bare.
    let gs2_end = payload[2..]
        .iter()
        .position(|&b| b == b',')
        .map(|i| i + 2)
        .ok_or_else(|| err("invalid SCRAM client-first: malformed GS2 header"))?;
    let bare = payload[gs2_end + 1..].to_vec();
    let attrs = parse_attrs(&bare);
    let username = attrs.get("n").cloned().unwrap_or_default();
    let client_nonce = attrs.get("r").cloned().unwrap_or_default();
    if username.is_empty() || client_nonce.is_empty() {
        return Err(err("invalid SCRAM client-first: missing user or nonce"));
    }

    let (creds, user_exists) = match creds {
        Some(c) => (c, true),
        None => {
            // Fabricate same-shape creds so the proof step (not the lookup)
            // surfaces the failure.
            let fake: [u8; 16] = rand::random();
            // The base64 alphabet is pure ASCII, so SASLprep never fails here.
            let fake_creds = derive_credentials(&B64.encode(fake), None, None)
                .expect("base64 nonce is ASCII and saslpreps cleanly");
            (fake_creds, false)
        }
    };

    let server_nonce_bytes: [u8; NONCE_BYTES] = rand::random();
    let server_nonce = B64.encode(server_nonce_bytes);
    let combined_nonce = format!("{client_nonce}{server_nonce}");
    let salt_b64 = B64.encode(&creds.salt);
    let server_first = format!(
        "r={combined_nonce},s={salt_b64},i={}",
        creds.iteration_count
    )
    .into_bytes();

    let state = ScramState {
        conversation_id,
        db_name: db_name.to_string(),
        username,
        creds,
        server_nonce,
        client_first_bare: bare,
        server_first: server_first.clone(),
        user_exists,
        step: 1,
    };
    Ok((server_first, state))
}

/// Process a SCRAM client-final message → server-final `v=...` payload.
/// Verifies the client proof; any mismatch (wrong password, unknown user,
/// malformed input) is an `AuthError`.
pub fn continue_scram(state: &mut ScramState, payload: &[u8]) -> Result<Vec<u8>, AuthError> {
    if state.step != 1 {
        return Err(err("SCRAM conversation out of order"));
    }
    let attrs = parse_attrs(payload);
    let client_proof_b64 = attrs.get("p").cloned().unwrap_or_default();
    let received_nonce = attrs.get("r").cloned().unwrap_or_default();
    if client_proof_b64.is_empty() {
        return Err(err("invalid SCRAM client-final: missing proof"));
    }
    if !received_nonce.ends_with(&state.server_nonce) {
        return Err(err("invalid SCRAM nonce"));
    }

    // client-final-without-proof = the message minus the `p=` attribute.
    let without_proof: Vec<&[u8]> = payload
        .split(|&b| b == b',')
        .filter(|seg| !seg.starts_with(b"p="))
        .collect();
    let client_final_without_proof = without_proof.join(&b","[..]);

    let mut auth_message = state.client_first_bare.clone();
    auth_message.push(b',');
    auth_message.extend_from_slice(&state.server_first);
    auth_message.push(b',');
    auth_message.extend_from_slice(&client_final_without_proof);

    let client_signature = hmac(&state.creds.stored_key, &auth_message);
    let client_proof = B64
        .decode(client_proof_b64.as_bytes())
        .map_err(|_| err("invalid SCRAM client proof encoding"))?;
    if client_proof.len() != client_signature.len() {
        return Err(err("authentication failed"));
    }
    let received_client_key: Vec<u8> = client_proof
        .iter()
        .zip(client_signature.iter())
        .map(|(a, b)| a ^ b)
        .collect();
    let expected_stored = Sha256::digest(&received_client_key);
    if !constant_time_eq(expected_stored.as_slice(), &state.creds.stored_key) || !state.user_exists
    {
        return Err(err("authentication failed"));
    }

    let server_signature = hmac(&state.creds.server_key, &auth_message);
    state.step = 2;
    Ok(format!("v={}", B64.encode(server_signature)).into_bytes())
}

/// HMAC-SHA-256(key, msg).
fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// Constant-time byte-slice equality (guards the proof check against timing
/// oracles even when the user doesn't exist). Delegates to the `subtle` crate
/// so both the length comparison and the content comparison are constant-time.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// Peek the username (`n=`) out of a SCRAM client-first payload, so the caller
/// can look up the user's credentials before `begin_scram`. Returns `None` if
/// the payload is malformed or carries no username.
pub fn peek_username(payload: &[u8]) -> Option<String> {
    if !payload.starts_with(b"n,") {
        return None;
    }
    let gs2_end = payload[2..]
        .iter()
        .position(|&b| b == b',')
        .map(|i| i + 2)?;
    let bare = &payload[gs2_end + 1..];
    parse_attrs(bare).get("n").cloned()
}

/// Parse a SCRAM `key=value,key=value` payload into a map.
fn parse_attrs(payload: &[u8]) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for chunk in payload.split(|&b| b == b',') {
        if chunk.is_empty() {
            continue;
        }
        let mut it = chunk.splitn(2, |&b| b == b'=');
        let key = it.next().unwrap_or(&[]);
        let val = it.next().unwrap_or(&[]);
        if !key.is_empty() {
            out.insert(
                String::from_utf8_lossy(key).into_owned(),
                String::from_utf8_lossy(val).into_owned(),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal SCRAM-SHA-256 *client*, to drive the server side end-to-end.
    fn client_final(password: &str, server_first: &[u8], client_first_bare: &[u8]) -> Vec<u8> {
        let attrs = parse_attrs(server_first);
        let combined_nonce = attrs.get("r").unwrap().clone();
        let salt = B64.decode(attrs.get("s").unwrap()).unwrap();
        let iters: u32 = attrs.get("i").unwrap().parse().unwrap();

        let mut salted = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iters, &mut salted);
        let client_key = hmac(&salted, b"Client Key");
        let stored_key = Sha256::digest(&client_key);

        let without_proof = format!("c=biws,r={combined_nonce}");
        let mut auth_message = client_first_bare.to_vec();
        auth_message.push(b',');
        auth_message.extend_from_slice(server_first);
        auth_message.push(b',');
        auth_message.extend_from_slice(without_proof.as_bytes());

        let client_sig = hmac(&stored_key, &auth_message);
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_sig.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        format!("{without_proof},p={}", B64.encode(proof)).into_bytes()
    }

    fn client_first(user: &str, nonce: &str) -> (Vec<u8>, Vec<u8>) {
        let bare = format!("n={user},r={nonce}").into_bytes();
        let full = format!("n,,n={user},r={nonce}").into_bytes();
        (full, bare)
    }

    #[test]
    fn full_scram_roundtrip_succeeds() {
        let creds = derive_credentials("s3cr3t", None, None).unwrap();
        let (first, bare) = client_first("alice", "clientNonceAAA");
        let (server_first, mut state) =
            begin_scram(1, "admin", &first, Some(creds.clone())).unwrap();
        assert!(state.user_exists);
        let cf = client_final("s3cr3t", &server_first, &bare);
        let server_final = continue_scram(&mut state, &cf).unwrap();
        assert!(server_final.starts_with(b"v="));
        assert_eq!(state.step, 2);

        // The server-final signature must verify against the server key.
        let v = String::from_utf8(server_final).unwrap();
        let got = B64.decode(&v[2..]).unwrap();
        let attrs = parse_attrs(&server_first);
        let without_proof = format!("c=biws,r={}", attrs.get("r").unwrap());
        let mut auth_message = bare.clone();
        auth_message.push(b',');
        auth_message.extend_from_slice(&server_first);
        auth_message.push(b',');
        auth_message.extend_from_slice(without_proof.as_bytes());
        assert_eq!(got, hmac(&creds.server_key, &auth_message));
    }

    #[test]
    fn wrong_password_fails() {
        let creds = derive_credentials("right", None, None).unwrap();
        let (first, bare) = client_first("alice", "nonceBBB");
        let (server_first, mut state) = begin_scram(1, "admin", &first, Some(creds)).unwrap();
        let cf = client_final("wrong", &server_first, &bare);
        assert_eq!(
            continue_scram(&mut state, &cf),
            Err(AuthError("authentication failed".into()))
        );
    }

    #[test]
    fn unknown_user_fails_at_proof() {
        // creds=None ⇒ fabricated; the handshake runs but the proof fails.
        let (first, bare) = client_first("ghost", "nonceCCC");
        let (server_first, mut state) = begin_scram(1, "admin", &first, None).unwrap();
        assert!(!state.user_exists);
        let cf = client_final("anything", &server_first, &bare);
        assert_eq!(
            continue_scram(&mut state, &cf),
            Err(AuthError("authentication failed".into()))
        );
    }

    #[test]
    fn derive_is_deterministic_for_fixed_salt() {
        let salt = vec![7u8; SALT_BYTES];
        let a = derive_credentials("pw", Some(1000), Some(salt.clone())).unwrap();
        let b = derive_credentials("pw", Some(1000), Some(salt)).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.iteration_count, 1000);
        assert_eq!(a.stored_key.len(), 32);
    }

    #[test]
    fn saslprep_maps_and_normalises() {
        // ASCII is the identity.
        assert_eq!(saslprep("s3cr3t").unwrap(), b"s3cr3t".to_vec());
        // C.1.2 non-ASCII space (U+00A0 NO-BREAK SPACE) maps to ASCII space.
        assert_eq!(saslprep("a\u{00A0}b").unwrap(), b"a b".to_vec());
        // NFKC normalisation: a full-width digit folds to its ASCII form.
        assert_eq!(saslprep("\u{FF11}").unwrap(), b"1".to_vec());
        // B.1 (commonly mapped to nothing): U+00AD SOFT HYPHEN is deleted.
        assert_eq!(saslprep("a\u{00AD}b").unwrap(), b"ab".to_vec());
    }

    #[test]
    fn saslprep_rejects_prohibited() {
        // A prohibited control character (RFC 4013 §2.3 → table C.2.1).
        assert!(saslprep("a\u{0007}b").is_err());
        // The failure propagates through credential derivation.
        assert!(derive_credentials("a\u{0007}b", None, None).is_err());
    }

    #[test]
    fn malformed_client_first_rejected() {
        assert!(begin_scram(1, "admin", b"garbage", None).is_err());
        // missing nonce
        assert!(begin_scram(1, "admin", b"n,,n=alice", None).is_err());
    }

    #[test]
    fn nonce_mismatch_rejected() {
        let creds = derive_credentials("pw", None, None).unwrap();
        let (first, _bare) = client_first("alice", "nonceDDD");
        let (_sf, mut state) = begin_scram(1, "admin", &first, Some(creds)).unwrap();
        // client-final with a nonce that doesn't end in the server nonce
        let bad = b"c=biws,r=totally-different,p=AAAA";
        assert!(continue_scram(&mut state, bad).is_err());
    }
}
