//! Authentication command family (R5b): the SCRAM-SHA-256 handshake
//! (`saslStart` / `saslContinue`) and user management (`createUser` /
//! `dropUser` / `usersInfo`).
//!
//! A port of the SCRAM + user-management slice of `commands.py`'s auth handlers.
//! The SCRAM mechanism itself lives in `secantus-auth` (credential derivation +
//! `begin_scram` / `continue_scram`); this module wires it to per-connection
//! state ([`ConnectionAuth`]) and to the user store (the `add_user` / `get_user`
//! / `drop_user` / `list_users` storage-trait methods).
//!
//! ## Stored user record
//!
//! Identical shape to the Python server's, so both servers read the same
//! `secantus_users` table:
//!
//! ```text
//! { _id: "<db>.<user>", user, db,
//!   credentials: { "SCRAM-SHA-256": { iterationCount, salt, storedKey, serverKey } },
//!   roles: [{ role, db }, ...],
//!   mechanisms: ["SCRAM-SHA-256"] }
//! ```
//!
//! `--auth` gating + RBAC privilege checks run in the dispatcher (see
//! [`crate::rbac`] and `dispatch`'s `authorize`): under `--auth` a non-handshake
//! command requires an authenticated principal holding the command's action
//! grant. `createUser` validates each role against the built-in catalogue
//! (`RoleNotFound`), and a successful `saslContinue` loads the principal's role
//! bindings into `ConnectionAuth::effective_roles`.
//!
//! User mutation: `updateUser` (rotate password / replace roles) and
//! `dropAllUsersFromDatabase` (R5b-4). Custom user-defined roles land in
//! [`crate::roles`]; `createUser` / `updateUser` accept a custom role that
//! exists in storage. `hello` advertises `saslSupportedMechs` for a queried
//! principal (R5b-4).
//!
//! **MONGODB-X509** (R5c-2): `createUser` provisions X509-capable users
//! (`mechanisms: ["MONGODB-X509"]`, no password); `saslStart` (and the legacy
//! `authenticate` command) read the verified client cert DN
//! ([`CommandContext::peer_cert_dn`], set by the server's mTLS handshake), look
//! the user up on `$external` / `admin`, and authenticate without a password.
//!
//! **SCRAM-SHA-1** (2026-09-30): created for every user by default alongside
//! SCRAM-SHA-256, as mongod 8.2 does, and authenticated with MongoDB's legacy
//! MD5 password prepass (`secantus_auth::derive_credentials_sha1`). A user
//! without credentials for the requested mechanism is 334 at `saslStart`.

use std::sync::{Arc, Mutex};

use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};

use secantus_auth::{
    begin_scram_with, continue_scram, derive_credentials, derive_credentials_sha1,
    derive_credentials_sha1_digested, peek_username, Mechanism, ScramState,
};

use crate::umc::{self, Field};
use crate::{CommandContext, CommandError, HandlerResult};

/// The modern driver default.
const SCRAM_SHA_256: &str = "SCRAM-SHA-256";
/// The legacy SCRAM, which mongod 8.2 still creates for every user by default.
const SCRAM_SHA_1: &str = "SCRAM-SHA-1";
/// `MechanismUnavailable` -- the user has no credentials for that mechanism.
const MECHANISM_UNAVAILABLE: i32 = 334;
/// The TLS-cert-as-username mechanism (R5c-2).
const MONGODB_X509: &str = "MONGODB-X509";
/// `AuthenticationFailed`.
const AUTHENTICATION_FAILED: i32 = 18;
/// `UserNotFound`.
const USER_NOT_FOUND: i32 = 11;
/// `Location51003` — a user with that name already exists.
const USER_ALREADY_EXISTS: i32 = 51003;

/// Per-connection authentication state, owned by the server's connection loop
/// (one per socket) and shared into each request's [`CommandContext`] behind an
/// `Arc<Mutex<…>>`. Mirrors `commands.py::ConnectionAuth` (SCRAM-only subset).
#[derive(Debug, Default)]
pub struct ConnectionAuth {
    /// The in-flight SCRAM conversation, set by `saslStart` and consumed by
    /// `saslContinue`. `None` between conversations.
    pub scram: Option<ScramState>,
    /// Monotonic conversation-id source (mongod starts at 1 per connection).
    next_conversation_id: i32,
    /// Principals (`(db, username)`) authenticated on this connection.
    pub authenticated: Vec<(String, String)>,
    /// The union of role bindings (`(role, db)`) the authenticated principals
    /// hold, used by the dispatcher's RBAC check.
    pub effective_roles: Vec<(String, String)>,
    /// The driver `client` metadata document sent in the handshake (`hello`'s
    /// `client` field), surfaced by `currentOp` as `clientMetadata`.
    pub client_metadata: Option<Document>,
    /// Whether this connection has had a `hello` answered: the driver's
    /// `client` metadata may come only with the first.
    pub hello_seen: bool,
    /// The compressors the last `hello` that named any settled on, in the
    /// client's order; answered again by every `hello` that names none.
    pub compressors: Vec<String>,
    /// The address the client connected from (`whatsmyuri`), set by the
    /// accept loop.
    pub peer_address: Option<String>,
    /// What `connectionStatus` calls this connection, for as long as it lasts.
    pub client_uuid: [u8; 16],
    /// Whether the peer is a loopback address -- the precondition of mongod's
    /// LOCALHOST EXCEPTION (see `lib.rs::localhost_exception`). Set by the
    /// accept loop; `false` wherever the peer is unknown.
    pub peer_loopback: bool,
    /// The effective privileges last computed for `authenticated`, kept
    /// until a user or role changes (`rbac::effective`).
    pub(crate) privileges: Option<crate::rbac::CachedAuthz>,
}

impl ConnectionAuth {
    pub fn new() -> Self {
        ConnectionAuth {
            scram: None,
            next_conversation_id: 0,
            authenticated: Vec::new(),
            effective_roles: Vec::new(),
            client_metadata: None,
            hello_seen: false,
            compressors: Vec::new(),
            peer_address: None,
            client_uuid: rand::random(),
            peer_loopback: false,
            privileges: None,
        }
    }

    /// Mint the next conversation id (1, 2, 3, … per connection).
    pub fn new_conversation_id(&mut self) -> i32 {
        self.next_conversation_id += 1;
        self.next_conversation_id
    }

    /// Whether any principal has authenticated on this connection.
    pub fn is_authenticated(&self) -> bool {
        !self.authenticated.is_empty()
    }

    /// Merge a user record's `roles` (`[{role, db}, ...]`) into the effective
    /// role set, de-duplicating.
    pub fn add_principal_roles(&mut self, roles: &[Bson]) {
        for r in roles {
            if let Bson::Document(d) = r {
                if let (Ok(role), Ok(db)) = (d.get_str("role"), d.get_str("db")) {
                    let binding = (role.to_string(), db.to_string());
                    if !self.effective_roles.contains(&binding) {
                        self.effective_roles.push(binding);
                    }
                }
            }
        }
    }
}

/// An `AuthenticationFailed` (18) command error.
fn auth_failure(msg: impl Into<String>) -> CommandError {
    CommandError::new(AUTHENTICATION_FAILED, "AuthenticationFailed", msg)
}

/// mongod's one message for EVERY failed SCRAM step -- a wrong password, an
/// unknown user, a malformed payload -- so the reply says nothing about which
/// (measured 8.2.11, 2026-09-30). The specific reason stays internal.
const AUTHENTICATION_FAILED_MSG: &str = "Authentication failed.";

/// Extract a SCRAM payload (BSON Binary, or raw bytes) into a byte vector.
fn payload_bytes(value: Option<&Bson>) -> Vec<u8> {
    match value {
        Some(Bson::Binary(b)) => b.bytes.clone(),
        _ => Vec::new(),
    }
}

/// A SASL payload as mongod reads it: binary, or a base64 string, which it
/// decodes and answers in kind (measured 8.2.11, 2026-10-15). The flag says
/// whether the reply's payload goes back as base64.
fn sasl_payload(value: Option<&Bson>) -> Result<(Vec<u8>, bool), CommandError> {
    use base64::Engine as _;
    match value {
        Some(Bson::String(s)) => base64::engine::general_purpose::STANDARD
            .decode(s)
            .map(|bytes| (bytes, true))
            .map_err(|_| {
                CommandError::new(
                    40537,
                    "Location40537",
                    "Failed decoding SASL payload: Invalid base64 character",
                )
            }),
        other => Ok((payload_bytes(other), false)),
    }
}

/// The reply's payload, in the form the request's came in.
fn sasl_reply_payload(bytes: Vec<u8>, as_base64: bool) -> Bson {
    use base64::Engine as _;
    if as_base64 {
        Bson::String(base64::engine::general_purpose::STANDARD.encode(bytes))
    } else {
        payload_binary(bytes)
    }
}

/// Wrap a server SCRAM payload back into a generic BSON Binary (subtype 0).
fn payload_binary(bytes: Vec<u8>) -> Bson {
    Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes,
    })
}

/// The per-connection auth state, or an `InternalError` if the server didn't
/// wire one in (a server bug — auth handlers always run with connection state).
fn conn_auth(ctx: &CommandContext) -> Result<Arc<Mutex<ConnectionAuth>>, CommandError> {
    match &ctx.conn_auth {
        Some(c) => Ok(c.clone()),
        None => Err(CommandError::new(
            1,
            "InternalError",
            "connection auth state not configured",
        )),
    }
}

/// Look up a user's credentials for `mechanism`, decoding the stored record.
///
/// `Ok(None)` when the user does not exist -- `begin_scram_with` then
/// fabricates credentials and the conversation fails at the proof step, as on
/// mongod. A user who EXISTS without credentials for this mechanism is refused
/// up front with mongod's 334 (measured 8.2.11, 2026-09-30).
fn lookup_creds(
    ctx: &CommandContext,
    db: &str,
    username: &str,
    mechanism: Mechanism,
) -> Result<Option<secantus_auth::StoredCredentials>, CommandError> {
    let record = ctx
        .storage()
        .ok()
        .and_then(|s| s.get_user(db, username).ok().flatten())
        .and_then(|bytes| secantus_core::bsondoc::from_bytes(bytes.as_slice()).ok());
    let Some(record) = record else {
        return Ok(None);
    };
    let name = mechanism.name();
    let Some(sub) = record
        .get_document("credentials")
        .ok()
        .and_then(|c| c.get_document(name).ok())
    else {
        return Err(CommandError::new(
            MECHANISM_UNAVAILABLE,
            "MechanismUnavailable",
            format!(
                "Unable to use {name} based authentication for user without any {name} \
                 credentials registered"
            ),
        ));
    };
    let decoded = (|| {
        let iteration_count = sub.get_i32("iterationCount").ok()? as u32;
        secantus_auth::StoredCredentials::from_b64(
            iteration_count,
            sub.get_str("salt").ok()?,
            sub.get_str("storedKey").ok()?,
            sub.get_str("serverKey").ok()?,
        )
        .ok()
    })();
    Ok(decoded)
}

/// The `roles` array (`[{role, db}, ...]`) stored on a user record, or `None`
/// if the user / roles can't be read.
pub(crate) fn lookup_roles(ctx: &CommandContext, db: &str, username: &str) -> Option<Vec<Bson>> {
    let record_bytes = ctx.storage().ok()?.get_user(db, username).ok()??;
    let record = secantus_core::bsondoc::from_bytes(record_bytes.as_slice()).ok()?;
    record.get_array("roles").ok().cloned()
}

/// A field mongod's parser requires and types (8.2.11): absent or null is
/// 40414, another type is 14.
fn required_field(
    doc: &Document,
    command: &str,
    field: &str,
    expected: &str,
    fits: fn(&Bson) -> bool,
) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null) => Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            format!("BSON field '{command}.{field}' is missing but a required field"),
        )),
        Some(value) if fits(value) => Ok(()),
        Some(value) => Err(CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{command}.{field}' is the wrong type '{}', expected type \
                 '{expected}'",
                secantus_core::query::bson_type_name(value)
            ),
        )),
    }
}

/// `saslStart` — begin a SCRAM-SHA-1 / SCRAM-SHA-256 conversation.
pub fn sasl_start(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    required_field(doc, "saslStart", "mechanism", "string", |v| {
        matches!(v, Bson::String(_))
    })?;
    let mechanism = doc.get_str("mechanism").unwrap_or("");
    if mechanism == MONGODB_X509 {
        return sasl_start_x509(doc, ctx);
    }
    let Some(mech) = Mechanism::from_name(mechanism) else {
        return Err(CommandError::new(
            MECHANISM_UNAVAILABLE,
            "MechanismUnavailable",
            format!("Received authentication for mechanism {mechanism} which is not enabled"),
        ));
    };
    let (payload, as_base64) = sasl_payload(doc.get("payload"))?;
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    let username = peek_username(&payload).unwrap_or_default();
    let creds = lookup_creds(ctx, &db_name, &username, mech)?;

    let auth = conn_auth(ctx)?;
    let mut auth = auth
        .lock()
        .map_err(|_| CommandError::new(1, "InternalError", "connection auth state corrupted"))?;
    let conversation_id = auth.new_conversation_id();
    let (server_first, state) = begin_scram_with(conversation_id, &db_name, &payload, creds, mech)
        .map_err(|_| auth_failure(AUTHENTICATION_FAILED_MSG))?;
    auth.scram = Some(state);

    Ok(doc! {
        "conversationId": conversation_id,
        "done": false,
        "payload": sasl_reply_payload(server_first, as_base64),
        "ok": 1.0,
    })
}

/// `saslContinue` — verify the client proof and finish the conversation.
pub fn sasl_continue(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    required_field(doc, "saslContinue", "conversationId", "int", |v| {
        matches!(v, Bson::Int32(_))
    })?;
    let (payload, as_base64) = sasl_payload(doc.get("payload"))?;
    let incoming_id = doc.get_i32("conversationId").ok();

    let auth = conn_auth(ctx)?;
    let mut auth = auth
        .lock()
        .map_err(|_| CommandError::new(1, "InternalError", "connection auth state corrupted"))?;
    let Some(mut state) = auth.scram.take() else {
        return Err(CommandError::new(
            17,
            "ProtocolError",
            "No SASL session state found",
        ));
    };
    if incoming_id != Some(state.conversation_id) {
        // Restore so a spurious id doesn't silently drop the conversation.
        auth.scram = Some(state);
        return Err(auth_failure("SCRAM conversation id mismatch"));
    }
    let server_final = match continue_scram(&mut state, &payload) {
        Ok(v) => v,
        Err(_) => return Err(auth_failure(AUTHENTICATION_FAILED_MSG)),
    };
    // Successful proof: record the principal. mongod returns done=true from the
    // second server message (skipping the spec's optional third round-trip).
    let principal = (state.db_name.clone(), state.username.clone());
    if !auth.authenticated.contains(&principal) {
        auth.authenticated.push(principal);
    }
    // Capture the principal's role bindings for the dispatcher's RBAC check.
    if let Some(roles) = lookup_roles(ctx, &state.db_name, &state.username) {
        auth.add_principal_roles(&roles);
    }
    let conversation_id = state.conversation_id;

    Ok(doc! {
        "conversationId": conversation_id,
        "done": true,
        "payload": sasl_reply_payload(server_final, as_base64),
        "ok": 1.0,
    })
}

/// Best-effort username from a MONGODB-X509 SASL payload. pymongo sends an empty
/// payload and trusts the cert; some drivers send the DN as UTF-8 (sometimes
/// behind the SCRAM-style `n,,` GS2 marker).
fn x509_payload_username(payload: &[u8]) -> Option<String> {
    if payload.is_empty() {
        return None;
    }
    let bytes = payload.strip_prefix(b"n,,").unwrap_or(payload);
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Shared MONGODB-X509 verification: the connection must have presented a
/// verified client cert (mTLS); look the cert DN up as a username on
/// `$external` (falling back to `admin`), require a MONGODB-X509 credential
/// entry, then mark the connection authenticated and capture its roles. Returns
/// the `(db, dn)` the principal authenticated under.
fn verify_x509(
    ctx: &CommandContext,
    claimed: Option<String>,
) -> Result<(String, String), CommandError> {
    // No verified certificate, or one naming another user: mongod gives no
    // reason, as for every failed authentication (measured 8.2.11,
    // 2026-10-15).
    let Some(dn) = ctx.peer_cert_dn.clone() else {
        return Err(auth_failure(AUTHENTICATION_FAILED_MSG));
    };
    if let Some(claimed) = claimed {
        if !claimed.is_empty() && claimed != dn {
            return Err(auth_failure(AUTHENTICATION_FAILED_MSG));
        }
    }
    let mut db_name = if ctx.db_name.is_empty() {
        "$external".to_string()
    } else {
        ctx.db_name.clone()
    };
    let mut record = lookup_user_doc(ctx, &db_name, &dn);
    if record.is_none() && db_name == "$external" {
        // Fall back to `admin` for users created with the `--db admin` shorthand.
        if let Some(r) = lookup_user_doc(ctx, "admin", &dn) {
            db_name = "admin".to_string();
            record = Some(r);
        }
    }
    let Some(record) = record else {
        return Err(auth_failure(format!(
            "MONGODB-X509: no user found with name '{dn}' on '{db_name}'"
        )));
    };
    // mongod's record of a `$external` user says `credentials: {external:
    // true}`; records this server wrote before 2026-10-13 carry the
    // mechanism's name instead.
    let has_x509 = record
        .get_document("credentials")
        .map(|c| c.contains_key(MONGODB_X509) || c.get_bool("external").unwrap_or(false))
        .unwrap_or(false);
    if !has_x509 {
        return Err(auth_failure(format!(
            "MONGODB-X509: user '{dn}' on '{db_name}' is not configured for X509 auth"
        )));
    }
    if let Some(auth) = &ctx.conn_auth {
        let mut auth = auth.lock().map_err(|_| {
            CommandError::new(1, "InternalError", "connection auth state corrupted")
        })?;
        let principal = (db_name.clone(), dn.clone());
        if !auth.authenticated.contains(&principal) {
            auth.authenticated.push(principal);
        }
        if let Ok(roles) = record.get_array("roles") {
            auth.add_principal_roles(roles);
        }
    }
    Ok((db_name, dn))
}

/// Decode a stored user record for `(db, username)`.
fn lookup_user_doc(ctx: &CommandContext, db: &str, username: &str) -> Option<Document> {
    let bytes = ctx.storage().ok()?.get_user(db, username).ok()??;
    secantus_core::bsondoc::from_bytes(bytes.as_slice()).ok()
}

/// `saslStart` with `mechanism: "MONGODB-X509"` — one-shot cert auth (no
/// challenge/response). `done: true` on the first reply.
fn sasl_start_x509(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let claimed = x509_payload_username(&payload_bytes(doc.get("payload")));
    verify_x509(ctx, claimed)?;
    let conversation_id = match &ctx.conn_auth {
        Some(a) => a
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .new_conversation_id(),
        None => 1,
    };
    Ok(doc! {
        "conversationId": conversation_id,
        "done": true,
        "payload": payload_binary(Vec::new()),
        "ok": 1.0,
    })
}

/// Legacy `authenticate` command — pymongo / Java / Go drivers use it for
/// MONGODB-X509 (a one-shot, no challenge/response). Only X509 is supported on
/// this path (SCRAM goes through `saslStart`).
pub fn authenticate(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    required_field(doc, "authenticate", "mechanism", "string", |v| {
        matches!(v, Bson::String(_))
    })?;
    let mechanism = doc.get_str("mechanism").unwrap_or(SCRAM_SHA_256);
    if mechanism != MONGODB_X509 {
        // mongod's whole answer to a mechanism this command does not run
        // (the SCRAM mechanisms go through `saslStart`).
        return Err(auth_failure("Authentication failed."));
    }
    let claimed = doc.get_str("user").ok().map(String::from);
    let (db_name, dn) = verify_x509(ctx, claimed)?;
    Ok(doc! { "dbname": db_name, "user": dn, "ok": 1.0 })
}

/// A `BadValue` (2) command error.
fn bad_value(msg: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", msg)
}

/// `RoleNotFound`.
const ROLE_NOT_FOUND: i32 = 31;

/// The database a user command works on. mongod refuses the empty name
/// (8.2.11) rather than reading it as `admin`.
fn user_db(ctx: &CommandContext) -> Result<String, CommandError> {
    if ctx.db_name.is_empty() {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            "Invalid database name: ''",
        ));
    }
    Ok(ctx.db_name.clone())
}

/// `UserNotFound` in the wording the user commands share: `updateUser`
/// (`User x@db not found`), `dropUser` (`User 'x@db' not found`) and the
/// role grants (`Could not find user "x" for db "db"`) each have their own
/// (8.2.11).
fn user_not_found(message: String) -> CommandError {
    CommandError::new(USER_NOT_FOUND, "UserNotFound", message)
}

/// Every role named must exist on its database, else mongod's
/// `Could not find role: r@db` (31); `prefix` is what `createRole` and its
/// kin put before that.
fn roles_must_exist(
    ctx: &CommandContext,
    roles: &[(String, String)],
    prefix: &str,
) -> Result<(), CommandError> {
    for (role, db) in roles {
        if !umc::role_exists(ctx, role, db)? {
            return Err(CommandError::new(
                ROLE_NOT_FOUND,
                "RoleNotFound",
                format!("{prefix}Could not find role: {role}@{db}"),
            ));
        }
    }
    Ok(())
}

/// The SCRAM credentials `createUser` / `updateUser` build from `pwd`, in
/// mongod's order and with mongod's refusals (8.2.11): an unknown mechanism,
/// SCRAM-SHA-256 with a password the client digested, and an empty password
/// (which mongod's SASLprep refuses with its UTF-8 preflight error).
/// `requested` is the `mechanisms` field; absent means both SCRAMs.
fn scram_credentials(
    username: &str,
    pwd: &str,
    requested: Option<&[String]>,
    digest_password: bool,
) -> Result<Document, CommandError> {
    let both = [SCRAM_SHA_1.to_string(), SCRAM_SHA_256.to_string()];
    let requested = requested.unwrap_or(&both);
    if let Some(unknown) = requested.iter().find(|m| Mechanism::from_name(m).is_none()) {
        return Err(bad_value(format!("Unknown auth mechanism '{unknown}'")));
    }
    let wants_256 = requested.iter().any(|m| m == SCRAM_SHA_256);
    if wants_256 && !digest_password {
        return Err(bad_value(
            "Use of SCRAM-SHA-256 requires undigested passwords",
        ));
    }
    if wants_256 && pwd.is_empty() {
        return Err(CommandError::new(
            50687,
            "Location50687",
            "Error preflighting UTF-8 conversion: U_STRING_NOT_TERMINATED_WARNING",
        ));
    }
    let mut out = Document::new();
    for mech in [Mechanism::Sha1, Mechanism::Sha256] {
        if !requested.iter().any(|m| m == mech.name()) {
            continue;
        }
        let creds = match (mech, digest_password) {
            (Mechanism::Sha1, true) => derive_credentials_sha1(username, pwd, None, None),
            (Mechanism::Sha1, false) => derive_credentials_sha1_digested(pwd, None, None),
            (Mechanism::Sha256, _) => {
                derive_credentials(pwd, None, None).map_err(|e| bad_value(e.to_string()))?
            }
        };
        out.insert(
            mech.name(),
            doc! {
                "iterationCount": creds.iteration_count as i32,
                "salt": creds.salt_b64(),
                "storedKey": creds.stored_key_b64(),
                "serverKey": creds.server_key_b64(),
            },
        );
    }
    Ok(out)
}

/// The strings of a `mechanisms` field, deduplicated in order.
fn mechanism_names(value: &Bson) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Bson::Array(items) = value {
        for name in items.iter().filter_map(Bson::as_str) {
            if !out.iter().any(|m| m == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// A fresh `userId`: a UUID (BSON binary subtype 4), as mongod stamps on
/// every user it creates.
fn new_user_id() -> Bson {
    Bson::Binary(Binary {
        subtype: BinarySubtype::Uuid,
        bytes: bson::Uuid::new().bytes().to_vec(),
    })
}

/// Put `mechanisms` at the end of a record, from its credentials.
fn restamp_mechanisms(record: &mut Document) {
    record.remove("mechanisms");
    record.insert("mechanisms", umc::mechanisms_of(record));
}

const CREATE_USER_FIELDS: &[(&str, Field, bool)] = &[
    ("createUser", Field::Str, true),
    ("pwd", Field::Str, false),
    ("customData", Field::Obj, false),
    ("roles", Field::RoleNames, true),
    ("digestPassword", Field::SafeBool, false),
    ("authenticationRestrictions", Field::Restrictions, false),
    ("mechanisms", Field::StrArr, false),
];

/// `createUser`, checked in mongod's order (8.2.11): the fields' types, the
/// database (`local` holds no users; `$external` users have no password,
/// every other user must), the mechanisms and the password, the name, the
/// restrictions, then the roles, each of which must exist on its database.
/// The record is stored in mongod's own field order (see [`crate::umc`]).
pub fn create_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(doc, "createUser", CREATE_USER_FIELDS)?;
    let db_name = user_db(ctx)?;
    let username = doc.get_str("createUser").unwrap_or("").to_string();
    if db_name == "local" {
        return Err(bad_value("Cannot create users in the local database"));
    }
    let external = db_name == "$external";
    let pwd = umc::given(doc, "pwd").and_then(Bson::as_str);
    if external && pwd.is_some() {
        return Err(bad_value(
            "Cannot set the password for users defined on the '$external' database",
        ));
    }
    let Some(pwd) = pwd.or(if external { Some("") } else { None }) else {
        return Err(bad_value(
            "Must provide a 'pwd' field for all user documents, except those with '$external' as \
             the user's source db",
        ));
    };
    let mechanisms = umc::given(doc, "mechanisms").map(mechanism_names);
    if mechanisms.as_ref().is_some_and(|m| m.is_empty()) {
        return Err(bad_value("mechanisms field must not be empty"));
    }
    let credentials = if external {
        doc! { "external": true }
    } else {
        let digest = umc::given(doc, "digestPassword")
            .and_then(crate::util::truthy_number_or_bool)
            .unwrap_or(true);
        scram_credentials(&username, pwd, mechanisms.as_deref(), digest)?
    };
    if username.is_empty() {
        return Err(bad_value(
            "User document needs 'user' field to be non-empty",
        ));
    }
    let restrictions = umc::given(doc, "authenticationRestrictions")
        .and_then(Bson::as_array)
        .cloned();
    if let Some(r) = &restrictions {
        umc::validate_restrictions(r)?;
    }
    let roles = umc::parse_role_names(
        umc::given(doc, "roles")
            .and_then(Bson::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        &db_name,
    )?;
    roles_must_exist(ctx, &roles, "")?;

    let mut record = doc! {
        "_id": format!("{db_name}.{username}"),
        "userId": new_user_id(),
        "user": &username,
        "db": &db_name,
        "credentials": credentials,
    };
    if let Some(r) = restrictions {
        record.insert("authenticationRestrictions", r);
    }
    if let Some(custom) = umc::given(doc, "customData") {
        record.insert("customData", custom.clone());
    }
    record.insert(
        "roles",
        roles.iter().map(umc::role_name_doc).collect::<Vec<_>>(),
    );
    restamp_mechanisms(&mut record);
    let mut record_bytes = Vec::new();
    record
        .to_writer(&mut record_bytes)
        .map_err(|e| CommandError::new(1, "InternalError", format!("encode user record: {e}")))?;

    let added = ctx
        .storage()?
        .add_user(&db_name, &username, &record_bytes, false)
        .map_err(crate::util::command_error)?;
    if !added {
        return Err(CommandError::new(
            USER_ALREADY_EXISTS,
            "Location51003",
            format!("User \"{username}@{db_name}\" already exists"),
        ));
    }
    // Close the localhost exception for the rest of this server's life.
    if let Some(p) = &ctx.server_params {
        p.mark_users_seen();
    }
    Ok(doc! { "ok": 1.0 })
}

/// `dropUser` — remove a user record.
pub fn drop_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(doc, "dropUser", &[("dropUser", Field::Str, true)])?;
    let db_name = user_db(ctx)?;
    let username = doc.get_str("dropUser").unwrap_or("").to_string();
    let removed = ctx
        .storage()?
        .drop_user(&db_name, &username)
        .map_err(crate::util::command_error)?;
    if !removed {
        return Err(user_not_found(format!(
            "User '{username}@{db_name}' not found"
        )));
    }
    // Drop any active auth state for this principal on the calling connection.
    if let Some(auth) = &ctx.conn_auth {
        let mut auth = auth.lock().map_err(|_| {
            CommandError::new(1, "InternalError", "connection auth state corrupted")
        })?;
        auth.authenticated
            .retain(|(d, u)| !(d == &db_name && u == &username));
    }
    Ok(doc! { "ok": 1.0 })
}

/// `logout`: drop the users this connection authenticated as on the
/// command's database, and the roles they brought.
pub(crate) fn forget_database(ctx: &CommandContext) {
    if let Some(auth) = &ctx.conn_auth {
        if let Ok(mut auth) = auth.lock() {
            auth.authenticated.retain(|(db, _)| db != &ctx.db_name);
        }
    }
    refresh_effective_roles(ctx);
}

/// Rebuild the calling connection's effective roles from its authenticated
/// principals' current records (after a role change).
pub(crate) fn refresh_effective_roles(ctx: &CommandContext) {
    let Some(auth) = &ctx.conn_auth else { return };
    let Ok(mut auth) = auth.lock() else { return };
    let principals = auth.authenticated.clone();
    auth.effective_roles.clear();
    auth.privileges = None;
    for (db, user) in principals {
        if let Some(roles) = lookup_roles(ctx, &db, &user) {
            auth.add_principal_roles(&roles);
        }
    }
}

const UPDATE_USER_FIELDS: &[(&str, Field, bool)] = &[
    ("updateUser", Field::Str, true),
    ("pwd", Field::Str, false),
    ("customData", Field::Obj, false),
    ("roles", Field::RoleNames, false),
    ("digestPassword", Field::SafeBool, false),
    ("authenticationRestrictions", Field::Restrictions, false),
    ("mechanisms", Field::StrArr, false),
];

/// `updateUser`, in mongod's order (8.2.11): at least one field to update;
/// a new password rebuilds the credentials for the mechanisms named (both
/// SCRAMs when none are); `mechanisms` alone keeps only those credentials,
/// and must name ones the user has; the roles must exist; then the user
/// must. A field the record lacks is added after the rest, which is where
/// `usersInfo` then lists it. Other connections keep their sessions.
pub fn update_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(doc, "updateUser", UPDATE_USER_FIELDS)?;
    let db_name = user_db(ctx)?;
    let username = doc.get_str("updateUser").unwrap_or("").to_string();
    let updates = [
        "pwd",
        "customData",
        "roles",
        "authenticationRestrictions",
        "mechanisms",
    ];
    if !updates.iter().any(|f| umc::given(doc, f).is_some()) {
        return Err(bad_value(
            "Must specify at least one field to update in updateUser",
        ));
    }
    let external = db_name == "$external";
    let pwd = umc::given(doc, "pwd").and_then(Bson::as_str);
    if external && pwd.is_some() {
        return Err(bad_value(
            "Cannot set the password for users defined on the '$external' database",
        ));
    }
    let mechanisms = umc::given(doc, "mechanisms").map(mechanism_names);
    let mut new_credentials = None;
    let mut keep_only = None;
    if let Some(pwd) = pwd {
        if mechanisms.as_ref().is_some_and(|m| m.is_empty()) {
            return Err(bad_value("mechanisms field must not be empty"));
        }
        let digest = umc::given(doc, "digestPassword")
            .and_then(crate::util::truthy_number_or_bool)
            .unwrap_or(true);
        new_credentials = Some(scram_credentials(
            &username,
            pwd,
            mechanisms.as_deref(),
            digest,
        )?);
    } else if let Some(requested) = mechanisms {
        if requested.is_empty() {
            return Err(bad_value("mechanisms field must not be empty"));
        }
        let Some(record) = lookup_user_doc(ctx, &db_name, &username) else {
            return Err(user_not_found(format!(
                "Could not find user \"{username}\" for db \"{db_name}\""
            )));
        };
        let held = record
            .get_document("credentials")
            .cloned()
            .unwrap_or_default();
        if requested.iter().any(|m| !held.contains_key(m)) {
            return Err(bad_value(
                "mechanisms field must be a subset of previously set mechanisms",
            ));
        }
        if !requested.iter().any(|m| Mechanism::from_name(m).is_some()) {
            return Err(bad_value(
                "mechanisms field must contain at least one previously set known mechanism",
            ));
        }
        keep_only = Some(requested);
    }
    let restrictions = umc::given(doc, "authenticationRestrictions")
        .and_then(Bson::as_array)
        .cloned();
    if let Some(r) = &restrictions {
        umc::validate_restrictions(r)?;
    }
    let roles = match umc::given(doc, "roles").and_then(Bson::as_array) {
        Some(items) => {
            let roles = umc::parse_role_names(items, &db_name)?;
            roles_must_exist(ctx, &roles, "")?;
            Some(roles)
        }
        None => None,
    };
    let Some(mut record) = lookup_user_doc(ctx, &db_name, &username) else {
        return Err(user_not_found(format!(
            "User {username}@{db_name} not found"
        )));
    };

    if let Some(credentials) = new_credentials {
        record.insert("credentials", credentials);
    }
    if let Some(keep) = keep_only {
        let mut held = record
            .get_document("credentials")
            .cloned()
            .unwrap_or_default();
        let dropped: Vec<String> = held.keys().filter(|k| !keep.contains(k)).cloned().collect();
        for k in dropped {
            held.remove(&k);
        }
        record.insert("credentials", held);
    }
    if let Some(custom) = umc::given(doc, "customData") {
        record.insert("customData", custom.clone());
    }
    if let Some(roles) = &roles {
        record.insert(
            "roles",
            roles.iter().map(umc::role_name_doc).collect::<Vec<_>>(),
        );
    }
    if let Some(r) = restrictions {
        if r.is_empty() {
            record.remove("authenticationRestrictions");
        } else {
            record.insert("authenticationRestrictions", r);
        }
    }
    restamp_mechanisms(&mut record);
    save_user_record(ctx, &db_name, &username, &record)?;
    if roles.is_some() {
        refresh_effective_roles(ctx);
    }
    Ok(doc! { "ok": 1.0 })
}

/// Re-encode a user record and persist it (overwriting the existing one).
fn save_user_record(
    ctx: &mut CommandContext,
    db_name: &str,
    username: &str,
    record: &Document,
) -> Result<(), CommandError> {
    let mut bytes = Vec::new();
    record
        .to_writer(&mut bytes)
        .map_err(|e| CommandError::new(1, "InternalError", format!("encode user record: {e}")))?;
    ctx.storage()?
        .add_user(db_name, username, &bytes, true)
        .map_err(crate::util::command_error)?;
    Ok(())
}

/// Pull `(role, db)` out of every user that holds it, on every database:
/// what `dropRole` does to users (8.2.11).
pub(crate) fn remove_role_from_users(
    ctx: &mut CommandContext,
    role: &(String, String),
) -> Result<(), CommandError> {
    let records = ctx
        .storage()?
        .list_users(None, 0, 0)
        .map_err(crate::util::command_error)?;
    for bytes in records {
        let Ok(mut record) = secantus_core::bsondoc::from_bytes(bytes.as_slice()) else {
            continue;
        };
        let (Ok(db), Ok(user)) = (
            record.get_str("db").map(String::from),
            record.get_str("user").map(String::from),
        ) else {
            continue;
        };
        let Ok(roles) = record.get_array("roles").cloned() else {
            continue;
        };
        let kept: Vec<Bson> = roles
            .iter()
            .filter(|r| umc::role_pairs(std::slice::from_ref(r)).first() != Some(role))
            .cloned()
            .collect();
        if kept.len() != roles.len() {
            record.insert("roles", kept);
            save_user_record(ctx, &db, &user, &record)?;
        }
    }
    Ok(())
}

/// `grantRolesToUser` / `revokeRolesFromUser` share one parse and one
/// order of checks (8.2.11): a non-empty `roles`, the user, then every role
/// named must exist -- a revoke of a role the user never held is fine, of
/// one that does not exist is not.
fn change_user_roles(
    doc: &Document,
    ctx: &mut CommandContext,
    command: &str,
    grant: bool,
) -> HandlerResult {
    umc::check_fields(
        doc,
        command,
        &[
            (command, Field::Str, true),
            ("roles", Field::RoleNames, true),
        ],
    )?;
    let db_name = user_db(ctx)?;
    let username = doc.get_str(command).unwrap_or("").to_string();
    let items = doc.get_array("roles").cloned().unwrap_or_default();
    if items.is_empty() {
        return Err(bad_value(format!(
            "{command} command requires a non-empty \"roles\" array"
        )));
    }
    let changes = umc::parse_role_names(&items, &db_name)?;
    let Some(mut record) = lookup_user_doc(ctx, &db_name, &username) else {
        return Err(user_not_found(format!(
            "Could not find user \"{username}\" for db \"{db_name}\""
        )));
    };
    roles_must_exist(ctx, &changes, "")?;
    let held = record.get_array("roles").cloned().unwrap_or_default();
    let mut pairs = umc::role_pairs(&held);
    if grant {
        for pair in changes {
            if !pairs.contains(&pair) {
                pairs.push(pair);
            }
        }
    } else {
        pairs.retain(|p| !changes.contains(p));
    }
    record.insert(
        "roles",
        pairs.iter().map(umc::role_name_doc).collect::<Vec<_>>(),
    );
    save_user_record(ctx, &db_name, &username, &record)?;
    refresh_effective_roles(ctx);
    Ok(doc! { "ok": 1.0 })
}

/// `grantRolesToUser` — add roles to a user, taking effect at once on the
/// calling connection.
pub fn grant_roles_to_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    change_user_roles(doc, ctx, "grantRolesToUser", true)
}

/// `revokeRolesFromUser` — take roles from a user.
pub fn revoke_roles_from_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    change_user_roles(doc, ctx, "revokeRolesFromUser", false)
}

/// `dropAllUsersFromDatabase` — drop every user of the calling db; answers
/// `{n, ok}` in that order, as mongod does.
pub fn drop_all_users_from_database(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(
        doc,
        "dropAllUsersFromDatabase",
        &[("dropAllUsersFromDatabase", Field::Any, false)],
    )?;
    let db_name = user_db(ctx)?;
    let storage = ctx.storage()?;
    let mut removed = 0_i32;
    loop {
        let batch = storage
            .list_users(Some(&db_name), 0, 1000)
            .map_err(crate::util::command_error)?;
        let n = batch.len();
        for bytes in &batch {
            if let Ok(rec) = secantus_core::bsondoc::from_bytes(bytes.as_slice()) {
                if let Ok(user) = rec.get_str("user") {
                    if storage
                        .drop_user(&db_name, user)
                        .map_err(crate::util::command_error)?
                    {
                        removed += 1;
                    }
                }
            }
        }
        if n < 1000 {
            break;
        }
    }
    if let Some(auth) = &ctx.conn_auth {
        let mut auth = auth.lock().map_err(|_| {
            CommandError::new(1, "InternalError", "connection auth state corrupted")
        })?;
        auth.authenticated.retain(|(d, _)| d != &db_name);
    }
    refresh_effective_roles(ctx);
    Ok(doc! { "n": removed, "ok": 1.0 })
}

/// mongod's `BSONElement::trueValue`: what `{forAllDBs: v}` means.
fn true_value(value: &Bson) -> bool {
    match value {
        Bson::Null | Bson::Undefined => false,
        other => crate::util::truthy_number_or_bool(other).unwrap_or(true),
    }
}

/// The users `usersInfo` names, looked up one by one. A name that matches
/// nothing is skipped.
fn named_users(
    ctx: &CommandContext,
    arg: &Bson,
    db_name: &str,
) -> Result<Vec<Document>, CommandError> {
    let storage = ctx.storage()?;
    let items: Vec<&Bson> = match arg {
        Bson::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut out: Vec<Document> = Vec::new();
    for item in items {
        let (user, db) = match item {
            Bson::String(name) => (name.as_str(), db_name),
            Bson::Document(spec) => match spec.get_str("user") {
                Ok(u) => (u, spec.get_str("db").unwrap_or(db_name)),
                Err(_) => continue,
            },
            _ => continue,
        };
        if let Some(bytes) = storage
            .get_user(db, user)
            .map_err(crate::util::command_error)?
        {
            let record = secantus_core::bsondoc::from_bytes(bytes.as_slice()).map_err(|e| {
                CommandError::new(1, "InternalError", format!("decode user record: {e}"))
            })?;
            if !out.iter().any(|r| r.get("_id") == record.get("_id")) {
                out.push(record);
            }
        }
    }
    Ok(out)
}

const USERS_INFO_FIELDS: &[(&str, Field, bool)] = &[
    ("usersInfo", Field::Any, false),
    ("showCredentials", Field::SafeBool, false),
    ("showCustomData", Field::SafeBool, false),
    ("showPrivileges", Field::SafeBool, false),
    ("showAuthenticationRestrictions", Field::SafeBool, false),
    ("filter", Field::Obj, false),
];

/// `usersInfo` (8.2.11). Without privilege details it is a query: the
/// users asked for, filtered by `filter`, sorted by user then database,
/// each in its stored field order with `mechanisms` last. With
/// `showPrivileges` or `showAuthenticationRestrictions` it is a description
/// of each named user in the order named, which no `1`, `forAllDBs` or
/// `filter` may ask for.
pub fn users_info(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(doc, "usersInfo", USERS_INFO_FIELDS)?;
    let exact =
        crate::serverinfo::principal_argument(doc.get("usersInfo"), "UserName", "user", true)?;
    let flag = |name: &str| {
        umc::given(doc, name)
            .and_then(crate::util::truthy_number_or_bool)
            .unwrap_or(false)
    };
    let show_credentials = flag("showCredentials");
    let show_custom_data = umc::given(doc, "showCustomData")
        .and_then(crate::util::truthy_number_or_bool)
        .unwrap_or(true);
    let detail = flag("showPrivileges") || flag("showAuthenticationRestrictions");
    let filter = umc::given(doc, "filter").and_then(Bson::as_document);
    if detail && (!exact || filter.is_some()) {
        return Err(CommandError::new(
            20,
            "IllegalOperation",
            "Privilege or restriction details require exact-match usersInfo queries",
        ));
    }
    let db_name = user_db(ctx)?;
    let arg = doc.get("usersInfo").cloned().unwrap_or(Bson::Int32(1));

    if detail {
        let mut users = Vec::new();
        for record in named_users(ctx, &arg, &db_name)? {
            users.push(Bson::Document(umc::user_detail(
                ctx,
                &record,
                show_credentials,
                show_custom_data,
                flag("showAuthenticationRestrictions"),
            )?));
        }
        return Ok(doc! { "users": users, "ok": 1.0 });
    }

    let records: Vec<Document> = match &arg {
        Bson::Array(items) if items.is_empty() => {
            return Err(bad_value("$or argument must be a non-empty array"));
        }
        Bson::Document(spec) if spec.get("forAllDBs").is_some_and(true_value) => ctx
            .storage()?
            .list_users(None, 0, 0)
            .map_err(crate::util::command_error)?
            .iter()
            .filter_map(|bytes| secantus_core::bsondoc::from_bytes(bytes.as_slice()).ok())
            .collect(),
        Bson::String(_) | Bson::Document(_) | Bson::Array(_) => named_users(ctx, &arg, &db_name)?,
        _ => ctx
            .storage()?
            .list_users(Some(&db_name), 0, 0)
            .map_err(crate::util::command_error)?
            .iter()
            .filter_map(|bytes| secantus_core::bsondoc::from_bytes(bytes.as_slice()).ok())
            .collect(),
    };
    let mut users: Vec<Document> = records
        .iter()
        .map(|r| umc::user_summary(r, show_credentials, show_custom_data))
        .collect();
    if let Some(filter) = filter {
        let mut kept = Vec::with_capacity(users.len());
        for user in users {
            match secantus_core::query::matches(&user, filter, &Document::new(), None) {
                Ok(true) => kept.push(user),
                Ok(false) => {}
                Err(secantus_core::fallback::Fallback::Mongo { code, message, .. }) => {
                    return Err(CommandError::new(
                        code,
                        crate::util::error_code_name(code),
                        message,
                    ))
                }
                Err(_) => return Err(bad_value("usersInfo: filter is not supported")),
            }
        }
        users = kept;
    }
    users.sort_by(|a, b| {
        let key = |u: &Document| {
            (
                u.get_str("user").unwrap_or("").to_string(),
                u.get_str("db").unwrap_or("").to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    let users: Vec<Bson> = users.into_iter().map(Bson::Document).collect();
    Ok(doc! { "users": users, "ok": 1.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    /// A tiny in-memory user store implementing the command `Storage` trait —
    /// only the user methods are overridden; everything else uses the defaults.
    #[derive(Default)]
    struct UserStore {
        users: StdMutex<HashMap<(String, String), Vec<u8>>>,
    }

    impl crate::Storage for UserStore {
        fn insert(
            &self,
            _db: &str,
            _coll: &str,
            _docs: Vec<Vec<u8>>,
            _ordered: bool,
        ) -> Result<(usize, Vec<Document>), crate::StorageError> {
            Ok((0, Vec::new()))
        }
        fn update_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _update: &Document,
            _multi: bool,
            _upsert: bool,
        ) -> Result<crate::UpdateOutcome, crate::StorageError> {
            Ok(crate::UpdateOutcome::default())
        }
        fn delete_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _limit: usize,
        ) -> Result<usize, crate::StorageError> {
            Ok(0)
        }
        fn count_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
        ) -> Result<usize, crate::StorageError> {
            Ok(0)
        }
        fn find(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _sort: Option<&Document>,
            _hint: Option<crate::storage::RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, crate::StorageError> {
            Ok(Vec::new())
        }
        fn add_user(
            &self,
            db: &str,
            username: &str,
            record: &[u8],
            replace: bool,
        ) -> Result<bool, crate::StorageError> {
            let mut users = self.users.lock().unwrap_or_else(|e| e.into_inner());
            let key = (db.to_string(), username.to_string());
            if users.contains_key(&key) && !replace {
                return Ok(false);
            }
            users.insert(key, record.to_vec());
            Ok(true)
        }
        fn get_user(
            &self,
            db: &str,
            username: &str,
        ) -> Result<Option<Vec<u8>>, crate::StorageError> {
            Ok(self
                .users
                .lock()
                .unwrap()
                .get(&(db.to_string(), username.to_string()))
                .cloned())
        }
        fn drop_user(&self, db: &str, username: &str) -> Result<bool, crate::StorageError> {
            Ok(self
                .users
                .lock()
                .unwrap()
                .remove(&(db.to_string(), username.to_string()))
                .is_some())
        }
        fn list_users(
            &self,
            db: Option<&str>,
            _skip: usize,
            _limit: usize,
        ) -> Result<Vec<Vec<u8>>, crate::StorageError> {
            Ok(self
                .users
                .lock()
                .unwrap()
                .iter()
                .filter(|((d, _), _)| db.is_none() || db == Some(d.as_str()))
                .map(|(_, v)| v.clone())
                .collect())
        }
    }

    fn ctx_with_store() -> (CommandContext, Arc<Mutex<ConnectionAuth>>) {
        let store: Arc<dyn crate::Storage> = Arc::new(UserStore::default());
        let auth = Arc::new(Mutex::new(ConnectionAuth::new()));
        let mut ctx = CommandContext::new(1)
            .with_storage(store)
            .with_cursors(Arc::new(crate::CursorRegistry::new()))
            .with_conn_auth(auth.clone());
        ctx.db_name = "admin".to_string();
        (ctx, auth)
    }

    /// Drive the SCRAM-SHA-256 client side to produce client-first / client-final.
    fn scram_client_first(user: &str, nonce: &str) -> (Vec<u8>, Vec<u8>) {
        let bare = format!("n={user},r={nonce}").into_bytes();
        let full = format!("n,,n={user},r={nonce}").into_bytes();
        (full, bare)
    }

    /// Run the full SCRAM-SHA-256 handshake for `(user, pwd)` against `ctx`,
    /// leaving the connection authenticated (with its effective roles loaded).
    fn authenticate(ctx: &mut CommandContext, user: &str, pwd: &str) {
        let (first, bare) = scram_client_first(user, "clientNonce123");
        let start = dispatch(
            &doc! {"saslStart": 1, "mechanism": SCRAM_SHA_256, "payload": payload_binary(first), "$db": "admin"},
            ctx,
        );
        let cid = start.get_i32("conversationId").unwrap();
        let server_first = payload_bytes(start.get("payload"));
        let cf = client_final(pwd, &server_first, &bare);
        let cont = dispatch(
            &doc! {"saslContinue": 1, "conversationId": cid, "payload": payload_binary(cf), "$db": "admin"},
            ctx,
        );
        assert_eq!(cont.get_f64("ok").unwrap(), 1.0, "auth failed: {cont:?}");
    }

    /// A user's current `(role, db)` assignments, sorted, for assertions.
    fn user_role_pairs(ctx: &CommandContext, user: &str) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> =
            umc::role_pairs(&lookup_roles(ctx, "admin", user).unwrap_or_default());
        pairs.sort();
        pairs
    }

    #[test]
    fn grant_and_revoke_roles_to_user() {
        let (mut ctx, _auth) = ctx_with_store();
        let r = dispatch(
            &doc! {"createUser": "alice", "pwd": "pw",
            "roles": [{"role": "read", "db": "admin"}], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0, "{r:?}");

        // Grant readWrite — now has both.
        let g = dispatch(
            &doc! {"grantRolesToUser": "alice",
            "roles": [{"role": "readWrite", "db": "admin"}], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(g.get_f64("ok").unwrap(), 1.0, "{g:?}");
        assert_eq!(
            user_role_pairs(&ctx, "alice"),
            vec![
                ("read".to_string(), "admin".to_string()),
                ("readWrite".to_string(), "admin".to_string()),
            ]
        );

        // Granting an already-held role is idempotent (no duplicate).
        dispatch(
            &doc! {"grantRolesToUser": "alice",
            "roles": [{"role": "read", "db": "admin"}], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(user_role_pairs(&ctx, "alice").len(), 2);

        // Revoke read — only readWrite remains.
        let rv = dispatch(
            &doc! {"revokeRolesFromUser": "alice",
            "roles": [{"role": "read", "db": "admin"}], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(rv.get_f64("ok").unwrap(), 1.0, "{rv:?}");
        assert_eq!(
            user_role_pairs(&ctx, "alice"),
            vec![("readWrite".to_string(), "admin".to_string())]
        );
    }

    #[test]
    fn grant_roles_to_missing_user_is_user_not_found() {
        let (mut ctx, _auth) = ctx_with_store();
        let r = dispatch(
            &doc! {"grantRolesToUser": "ghost",
            "roles": [{"role": "read", "db": "admin"}], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(r.get_i32("code").unwrap(), USER_NOT_FOUND);
        assert_eq!(r.get_str("codeName").unwrap(), "UserNotFound");
    }

    #[test]
    fn grant_unknown_role_is_role_not_found() {
        let (mut ctx, _auth) = ctx_with_store();
        dispatch(
            &doc! {"createUser": "bob", "pwd": "pw", "roles": [], "$db": "admin"},
            &mut ctx,
        );
        let r = dispatch(
            &doc! {"grantRolesToUser": "bob",
            "roles": [{"role": "nonexistentRole", "db": "admin"}], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(r.get_str("codeName").unwrap(), "RoleNotFound");
    }

    #[test]
    fn require_auth_gates_unauthenticated_commands() {
        let (mut ctx, _auth) = ctx_with_store();
        ctx.require_auth = true;
        // Pre-auth handshake commands flow without authentication.
        assert_eq!(
            dispatch(&doc! {"ping": 1}, &mut ctx).get_f64("ok").unwrap(),
            1.0
        );
        assert_eq!(
            dispatch(&doc! {"hello": 1}, &mut ctx)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        // A data command is rejected with Unauthorized (13).
        let find = dispatch(&doc! {"find": "c", "$db": "t"}, &mut ctx);
        assert_eq!(find.get_i32("code").unwrap(), 13);
        assert_eq!(find.get_str("codeName").unwrap(), "Unauthorized");
        // An unknown command is still CommandNotFound (59), not Unauthorized.
        let unknown = dispatch(&doc! {"bogusCmd": 1, "$db": "t"}, &mut ctx);
        assert_eq!(unknown.get_i32("code").unwrap(), 59);
    }

    #[test]
    fn rbac_grants_and_denies_by_role() {
        let (mut ctx, _auth) = ctx_with_store();
        // Provision a readWrite-on-"t" user (auth off during setup).
        dispatch(
            &doc! {"createUser": "rw", "pwd": "pw", "roles": [{"role": "readWrite", "db": "t"}], "$db": "admin"},
            &mut ctx,
        );
        authenticate(&mut ctx, "rw", "pw");
        ctx.require_auth = true;

        // The unit-test `dispatch` doesn't read `$db` (the server sets db_name),
        // so we set the target db on the context directly, as the server would.
        // readWrite on "t" grants find / insert on "t" ...
        ctx.db_name = "t".to_string();
        assert_eq!(
            dispatch(&doc! {"find": "c"}, &mut ctx)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        assert_eq!(
            dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, &mut ctx)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        // ... but not on a different db ...
        ctx.db_name = "other".to_string();
        let other = dispatch(&doc! {"find": "c"}, &mut ctx);
        assert_eq!(other.get_i32("code").unwrap(), 13);
        // ... and not a cluster command (serverStatus needs clusterMonitor/root).
        ctx.db_name = "admin".to_string();
        let ss = dispatch(&doc! {"serverStatus": 1}, &mut ctx);
        assert_eq!(ss.get_i32("code").unwrap(), 13);
    }

    #[test]
    fn root_role_authorizes_cluster_commands() {
        let (mut ctx, _auth) = ctx_with_store();
        dispatch(
            &doc! {"createUser": "admin", "pwd": "pw", "roles": [{"role": "root", "db": "admin"}], "$db": "admin"},
            &mut ctx,
        );
        authenticate(&mut ctx, "admin", "pw");
        ctx.require_auth = true;
        ctx.db_name = "admin".to_string();
        assert_eq!(
            dispatch(&doc! {"serverStatus": 1}, &mut ctx)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        // root spans every db.
        ctx.db_name = "anydb".to_string();
        assert_eq!(
            dispatch(&doc! {"find": "c"}, &mut ctx)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
    }

    #[test]
    fn create_user_unknown_role_is_role_not_found() {
        let (mut ctx, _auth) = ctx_with_store();
        let reply = dispatch(
            &doc! {"createUser": "x", "pwd": "pw", "roles": ["notARole"], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 31);
        assert_eq!(reply.get_str("codeName").unwrap(), "RoleNotFound");
    }

    #[test]
    fn update_user_rotates_password_and_roles() {
        let (mut ctx, _auth) = ctx_with_store();
        dispatch(
            &doc! {"createUser": "u", "pwd": "old", "roles": ["read"], "$db": "admin"},
            &mut ctx,
        );
        // rotate password — the old one no longer authenticates, the new one does
        assert_eq!(
            dispatch(&doc! {"updateUser": "u", "pwd": "new"}, &mut ctx)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        let (first, bare) = scram_client_first("u", "nonceUpd");
        let start = dispatch(
            &doc! {"saslStart": 1, "mechanism": SCRAM_SHA_256, "payload": payload_binary(first), "$db": "admin"},
            &mut ctx,
        );
        let cid = start.get_i32("conversationId").unwrap();
        let sf = payload_bytes(start.get("payload"));
        let cont = dispatch(
            &doc! {"saslContinue": 1, "conversationId": cid, "payload": payload_binary(client_final("new", &sf, &bare)), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(
            cont.get_f64("ok").unwrap(),
            1.0,
            "new password authenticates"
        );

        // replace roles
        assert_eq!(
            dispatch(&doc! {"updateUser": "u", "roles": ["readWrite"]}, &mut ctx)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        let info = dispatch(&doc! {"usersInfo": "u"}, &mut ctx);
        let roles = info.get_array("users").unwrap()[0]
            .as_document()
            .unwrap()
            .get_array("roles")
            .unwrap()
            .clone();
        assert_eq!(
            roles[0].as_document().unwrap().get_str("role").unwrap(),
            "readWrite"
        );

        // updateUser on a missing user → UserNotFound
        let missing = dispatch(&doc! {"updateUser": "ghost", "pwd": "x"}, &mut ctx);
        assert_eq!(missing.get_i32("code").unwrap(), USER_NOT_FOUND);
        // nothing to update → BadValue
        let empty = dispatch(&doc! {"updateUser": "u"}, &mut ctx);
        assert_eq!(empty.get_i32("code").unwrap(), 2);
    }

    #[test]
    fn drop_all_users_from_database() {
        let (mut ctx, _auth) = ctx_with_store();
        for u in ["a", "b", "c"] {
            dispatch(
                &doc! {"createUser": u, "pwd": "pw", "roles": [], "$db": "admin"},
                &mut ctx,
            );
        }
        let reply = dispatch(&doc! {"dropAllUsersFromDatabase": 1}, &mut ctx);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(reply.get_i32("n").unwrap(), 3);
        assert!(dispatch(&doc! {"usersInfo": 1}, &mut ctx)
            .get_array("users")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn create_user_then_authenticate_roundtrip() {
        let (mut ctx, _auth) = ctx_with_store();
        // createUser
        let reply = dispatch(
            &doc! {
                "createUser": "alice",
                "pwd": "s3cr3t",
                "roles": ["readWrite"],
                "$db": "admin",
            },
            &mut ctx,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply:?}");

        // duplicate createUser fails
        let dup = dispatch(
            &doc! {"createUser": "alice", "pwd": "x", "roles": [], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(dup.get_i32("code").unwrap(), USER_ALREADY_EXISTS);

        // saslStart
        let (first, bare) = scram_client_first("alice", "clientNonceXYZ");
        let start = dispatch(
            &doc! {
                "saslStart": 1,
                "mechanism": SCRAM_SHA_256,
                "payload": payload_binary(first),
                "$db": "admin",
            },
            &mut ctx,
        );
        assert_eq!(start.get_f64("ok").unwrap(), 1.0, "{start:?}");
        assert!(!start.get_bool("done").unwrap());
        let cid = start.get_i32("conversationId").unwrap();
        let server_first = payload_bytes(start.get("payload"));

        // client-final (compute the proof exactly as the secantus-auth tests do)
        let cf = client_final("s3cr3t", &server_first, &bare);
        let cont = dispatch(
            &doc! {
                "saslContinue": 1,
                "conversationId": cid,
                "payload": payload_binary(cf),
                "$db": "admin",
            },
            &mut ctx,
        );
        assert_eq!(cont.get_f64("ok").unwrap(), 1.0, "{cont:?}");
        assert!(cont.get_bool("done").unwrap());
    }

    #[test]
    fn wrong_password_fails_auth() {
        let (mut ctx, _auth) = ctx_with_store();
        dispatch(
            &doc! {"createUser": "bob", "pwd": "right", "roles": [], "$db": "admin"},
            &mut ctx,
        );
        let (first, bare) = scram_client_first("bob", "nonceWrong");
        let start = dispatch(
            &doc! {"saslStart": 1, "mechanism": SCRAM_SHA_256, "payload": payload_binary(first), "$db": "admin"},
            &mut ctx,
        );
        let cid = start.get_i32("conversationId").unwrap();
        let server_first = payload_bytes(start.get("payload"));
        let cf = client_final("wrong", &server_first, &bare);
        let cont = dispatch(
            &doc! {"saslContinue": 1, "conversationId": cid, "payload": payload_binary(cf), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(cont.get_i32("code").unwrap(), AUTHENTICATION_FAILED);
    }

    #[test]
    fn unknown_user_fails_at_proof() {
        let (mut ctx, _auth) = ctx_with_store();
        let (first, bare) = scram_client_first("ghost", "nonceGhost");
        let start = dispatch(
            &doc! {"saslStart": 1, "mechanism": SCRAM_SHA_256, "payload": payload_binary(first), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(start.get_f64("ok").unwrap(), 1.0);
        let cid = start.get_i32("conversationId").unwrap();
        let server_first = payload_bytes(start.get("payload"));
        let cf = client_final("anything", &server_first, &bare);
        let cont = dispatch(
            &doc! {"saslContinue": 1, "conversationId": cid, "payload": payload_binary(cf), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(cont.get_i32("code").unwrap(), AUTHENTICATION_FAILED);
    }

    #[test]
    fn unsupported_mechanism_rejected() {
        let (mut ctx, _auth) = ctx_with_store();
        let reply = dispatch(
            &doc! {"saslStart": 1, "mechanism": "PLAIN", "payload": payload_binary(b"n,,n=x,r=y".to_vec()), "$db": "admin"},
            &mut ctx,
        );
        // mongod 8.2.11: 334, naming the mechanism.
        assert_eq!(reply.get_i32("code").unwrap(), MECHANISM_UNAVAILABLE);
        assert_eq!(
            reply.get_str("errmsg").unwrap(),
            "Received authentication for mechanism PLAIN which is not enabled"
        );
    }

    /// mongod 8.2.11 (2026-09-30): a user gets BOTH SCRAMs by default, `hello`
    /// lists the user's own, and a mechanism the user lacks is 334 at saslStart.
    #[test]
    fn scram_sha1_is_created_by_default_and_refused_when_absent() {
        let (mut ctx, _auth) = ctx_with_store();
        for (user, mechs) in [("both", None), ("only256", Some(vec!["SCRAM-SHA-256"]))] {
            let mut cmd = doc! {"createUser": user, "pwd": "pw", "roles": [], "$db": "admin"};
            if let Some(m) = mechs {
                cmd.insert("mechanisms", m);
            }
            assert_eq!(dispatch(&cmd, &mut ctx).get_f64("ok").unwrap(), 1.0);
        }
        let hello = |ctx: &mut CommandContext, who: &str| {
            dispatch(
                &doc! {"hello": 1, "saslSupportedMechs": who, "$db": "admin"},
                ctx,
            )
        };
        assert_eq!(
            hello(&mut ctx, "admin.both")
                .get_array("saslSupportedMechs")
                .unwrap(),
            &vec![
                Bson::String("SCRAM-SHA-1".into()),
                Bson::String("SCRAM-SHA-256".into())
            ]
        );
        assert!(hello(&mut ctx, "admin.nobody")
            .get("saslSupportedMechs")
            .is_none());
        let refused = dispatch(
            &doc! {"saslStart": 1, "mechanism": "SCRAM-SHA-1",
            "payload": payload_binary(b"n,,n=only256,r=abc".to_vec()), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(refused.get_i32("code").unwrap(), 334);
        assert_eq!(
            refused.get_str("errmsg").unwrap(),
            "Unable to use SCRAM-SHA-1 based authentication for user without any SCRAM-SHA-1 \
             credentials registered"
        );
        let started = dispatch(
            &doc! {"saslStart": 1, "mechanism": "SCRAM-SHA-1",
            "payload": payload_binary(b"n,,n=both,r=abc".to_vec()), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(started.get_f64("ok").unwrap(), 1.0);
    }

    /// mongod 8.2.11: an X509 user lives on `$external`, takes no password
    /// there (and must have one anywhere else), and `usersInfo` says its
    /// credentials are `{external: true}` and its mechanisms `["external"]`.
    #[test]
    fn x509_create_user_needs_no_password() {
        let (mut ctx, _auth) = ctx_with_store();
        let on_admin = dispatch(
            &doc! {"createUser": "CN=alice", "mechanisms": [MONGODB_X509], "roles": [], "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(
            on_admin.get_str("errmsg").unwrap(),
            "Must provide a 'pwd' field for all user documents, except those with '$external' \
             as the user's source db"
        );
        ctx.db_name = "$external".to_string();
        let reply = dispatch(
            &doc! {
                "createUser": "CN=alice",
                "mechanisms": [MONGODB_X509],
                "roles": [{"role": "readWrite", "db": "app"}],
                "$db": "$external",
            },
            &mut ctx,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply:?}");
        let info = dispatch(
            &doc! {"usersInfo": "CN=alice", "showCredentials": true, "$db": "$external"},
            &mut ctx,
        );
        let user = info.get_array("users").unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert_eq!(
            user.get_document("credentials").unwrap(),
            &doc! {"external": true}
        );
        assert_eq!(
            user.get_array("mechanisms").unwrap(),
            &vec![Bson::String("external".into())]
        );
    }

    #[test]
    fn x509_saslstart_authenticates_with_matching_cert() {
        let (mut ctx, _auth) = ctx_with_store();
        ctx.db_name = "$external".to_string();
        dispatch(
            &doc! {"createUser": "CN=bob", "mechanisms": [MONGODB_X509], "roles": [], "$db": "$external"},
            &mut ctx,
        );
        // No client cert → auth failure.
        let no_cert = dispatch(
            &doc! {"saslStart": 1, "mechanism": MONGODB_X509, "payload": payload_binary(Vec::new()), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(no_cert.get_i32("code").unwrap(), AUTHENTICATION_FAILED);

        // With the verified cert DN, X509 authenticates (done:true).
        ctx.peer_cert_dn = Some("CN=bob".to_string());
        let ok = dispatch(
            &doc! {"saslStart": 1, "mechanism": MONGODB_X509, "payload": payload_binary(Vec::new()), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(ok.get_f64("ok").unwrap(), 1.0, "{ok:?}");
        assert!(ok.get_bool("done").unwrap());
    }

    #[test]
    fn x509_rejects_scram_only_user_and_dn_mismatch() {
        let (mut ctx, _auth) = ctx_with_store();
        // SCRAM-only user (default mechanisms) — no X509 marker.
        dispatch(
            &doc! {"createUser": "CN=carol", "pwd": "pw", "roles": [], "$db": "admin"},
            &mut ctx,
        );
        ctx.peer_cert_dn = Some("CN=carol".to_string());
        let reply = dispatch(
            &doc! {"saslStart": 1, "mechanism": MONGODB_X509, "payload": payload_binary(Vec::new()), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(reply.get_i32("code").unwrap(), AUTHENTICATION_FAILED);

        // Payload-claimed username mismatching the cert DN is rejected.
        ctx.db_name = "$external".to_string();
        dispatch(
            &doc! {"createUser": "CN=dave", "mechanisms": [MONGODB_X509], "roles": [], "$db": "$external"},
            &mut ctx,
        );
        ctx.peer_cert_dn = Some("CN=dave".to_string());
        let mismatch = dispatch(
            &doc! {"saslStart": 1, "mechanism": MONGODB_X509, "payload": payload_binary(b"CN=someone-else".to_vec()), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(mismatch.get_i32("code").unwrap(), AUTHENTICATION_FAILED);
    }

    #[test]
    fn x509_legacy_authenticate_command() {
        let (mut ctx, _auth) = ctx_with_store();
        ctx.db_name = "$external".to_string();
        dispatch(
            &doc! {"createUser": "CN=erin", "mechanisms": [MONGODB_X509], "roles": [], "$db": "$external"},
            &mut ctx,
        );
        ctx.peer_cert_dn = Some("CN=erin".to_string());
        let reply = dispatch(
            &doc! {"authenticate": 1, "mechanism": MONGODB_X509, "user": "CN=erin", "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply:?}");
        assert_eq!(reply.get_str("user").unwrap(), "CN=erin");
        // A SCRAM mechanism does not run on this path, and mongod's answer
        // to it is the failure it gives any authentication.
        let scram = dispatch(
            &doc! {"authenticate": 1, "mechanism": SCRAM_SHA_256, "user": "x", "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(scram.get_i32("code").unwrap(), AUTHENTICATION_FAILED);
        assert_eq!(scram.get_str("errmsg").unwrap(), "Authentication failed.");
    }

    #[test]
    fn users_info_hides_credentials_by_default() {
        let (mut ctx, _auth) = ctx_with_store();
        dispatch(
            &doc! {"createUser": "carol", "pwd": "pw", "roles": ["read"], "$db": "admin"},
            &mut ctx,
        );
        let info = dispatch(&doc! {"usersInfo": "carol", "$db": "admin"}, &mut ctx);
        let users = info.get_array("users").unwrap();
        assert_eq!(users.len(), 1);
        let u = users[0].as_document().unwrap();
        assert_eq!(u.get_str("user").unwrap(), "carol");
        assert!(u.get("credentials").is_none(), "credentials hidden");
        // roles normalised to {role, db}
        let roles = u.get_array("roles").unwrap();
        let r0 = roles[0].as_document().unwrap();
        assert_eq!(r0.get_str("role").unwrap(), "read");
        assert_eq!(r0.get_str("db").unwrap(), "admin");

        // with showCredentials the SCRAM-SHA-256 block is present
        let info2 = dispatch(
            &doc! {"usersInfo": "carol", "showCredentials": true, "$db": "admin"},
            &mut ctx,
        );
        let u2 = info2.get_array("users").unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert!(u2
            .get_document("credentials")
            .unwrap()
            .contains_key(SCRAM_SHA_256));
    }

    #[test]
    fn drop_user_then_missing() {
        let (mut ctx, _auth) = ctx_with_store();
        dispatch(
            &doc! {"createUser": "dave", "pwd": "pw", "roles": [], "$db": "admin"},
            &mut ctx,
        );
        let drop = dispatch(&doc! {"dropUser": "dave", "$db": "admin"}, &mut ctx);
        assert_eq!(drop.get_f64("ok").unwrap(), 1.0);
        let again = dispatch(&doc! {"dropUser": "dave", "$db": "admin"}, &mut ctx);
        assert_eq!(again.get_i32("code").unwrap(), USER_NOT_FOUND);
    }

    // A minimal SCRAM-SHA-256 client final, mirroring secantus-auth's test client.
    fn client_final(password: &str, server_first: &[u8], client_first_bare: &[u8]) -> Vec<u8> {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::{Digest, Sha256};
        type HmacSha256 = Hmac<Sha256>;

        fn parse(payload: &[u8]) -> HashMap<String, String> {
            let mut out = HashMap::new();
            for chunk in payload.split(|&b| b == b',') {
                let mut it = chunk.splitn(2, |&b| b == b'=');
                let k = it.next().unwrap_or(&[]);
                let v = it.next().unwrap_or(&[]);
                if !k.is_empty() {
                    out.insert(
                        String::from_utf8_lossy(k).into_owned(),
                        String::from_utf8_lossy(v).into_owned(),
                    );
                }
            }
            out
        }
        fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
            let mut m = HmacSha256::new_from_slice(key).unwrap();
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }

        let attrs = parse(server_first);
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
}
