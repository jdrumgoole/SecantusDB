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
//! ## Deferred (later slices, tracked in `tasks/rust-server-plan.md`)
//!
//! * **SCRAM-SHA-1** (legacy MD5 prepass) and non-ASCII SASLprep.

use std::sync::{Arc, Mutex};

use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};

use secantus_auth::{begin_scram, continue_scram, derive_credentials, peek_username, ScramState};

use crate::{rbac, CommandContext, CommandError, HandlerResult};

/// The only mechanism this slice implements (the modern driver default).
const SCRAM_SHA_256: &str = "SCRAM-SHA-256";
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
}

impl ConnectionAuth {
    pub fn new() -> Self {
        ConnectionAuth {
            scram: None,
            next_conversation_id: 0,
            authenticated: Vec::new(),
            effective_roles: Vec::new(),
            client_metadata: None,
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

/// Extract a SCRAM payload (BSON Binary, or raw bytes) into a byte vector.
fn payload_bytes(value: Option<&Bson>) -> Vec<u8> {
    match value {
        Some(Bson::Binary(b)) => b.bytes.clone(),
        _ => Vec::new(),
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

/// Look up a user's SCRAM-SHA-256 credentials, decoding the stored record.
/// Returns `None` when the user doesn't exist or carries no SCRAM-SHA-256 entry
/// (so `begin_scram` fabricates credentials and fails at the proof step).
fn lookup_creds(
    ctx: &CommandContext,
    db: &str,
    username: &str,
) -> Option<secantus_auth::StoredCredentials> {
    let record_bytes = ctx.storage().ok()?.get_user(db, username).ok()??;
    let record = Document::from_reader(&mut record_bytes.as_slice()).ok()?;
    let creds = record.get_document("credentials").ok()?;
    let sub = creds.get_document(SCRAM_SHA_256).ok()?;
    let iteration_count = sub.get_i32("iterationCount").ok()? as u32;
    let salt = sub.get_str("salt").ok()?;
    let stored_key = sub.get_str("storedKey").ok()?;
    let server_key = sub.get_str("serverKey").ok()?;
    secantus_auth::StoredCredentials::from_b64(iteration_count, salt, stored_key, server_key).ok()
}

/// The `roles` array (`[{role, db}, ...]`) stored on a user record, or `None`
/// if the user / roles can't be read.
fn lookup_roles(ctx: &CommandContext, db: &str, username: &str) -> Option<Vec<Bson>> {
    let record_bytes = ctx.storage().ok()?.get_user(db, username).ok()??;
    let record = Document::from_reader(&mut record_bytes.as_slice()).ok()?;
    record.get_array("roles").ok().cloned()
}

/// `saslStart` — begin a SCRAM-SHA-256 conversation.
pub fn sasl_start(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let mechanism = doc.get_str("mechanism").unwrap_or("");
    if mechanism == MONGODB_X509 {
        return sasl_start_x509(doc, ctx);
    }
    if mechanism != SCRAM_SHA_256 {
        return Err(auth_failure(format!(
            "Unsupported SASL mechanism: '{mechanism}' (supported: {SCRAM_SHA_256}, {MONGODB_X509})"
        )));
    }
    let payload = payload_bytes(doc.get("payload"));
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    let username = peek_username(&payload).unwrap_or_default();
    let creds = lookup_creds(ctx, &db_name, &username);

    let auth = conn_auth(ctx)?;
    let mut auth = auth
        .lock()
        .map_err(|_| CommandError::new(1, "InternalError", "connection auth state corrupted"))?;
    let conversation_id = auth.new_conversation_id();
    let (server_first, state) = begin_scram(conversation_id, &db_name, &payload, creds)
        .map_err(|e| auth_failure(e.to_string()))?;
    auth.scram = Some(state);

    Ok(doc! {
        "conversationId": conversation_id,
        "done": false,
        "payload": payload_binary(server_first),
        "ok": 1.0,
    })
}

/// `saslContinue` — verify the client proof and finish the conversation.
pub fn sasl_continue(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let payload = payload_bytes(doc.get("payload"));
    let incoming_id = doc.get_i32("conversationId").ok();

    let auth = conn_auth(ctx)?;
    let mut auth = auth
        .lock()
        .map_err(|_| CommandError::new(1, "InternalError", "connection auth state corrupted"))?;
    let Some(mut state) = auth.scram.take() else {
        return Err(auth_failure("No SCRAM conversation in progress"));
    };
    if incoming_id != Some(state.conversation_id) {
        // Restore so a spurious id doesn't silently drop the conversation.
        auth.scram = Some(state);
        return Err(auth_failure("SCRAM conversation id mismatch"));
    }
    let server_final = match continue_scram(&mut state, &payload) {
        Ok(v) => v,
        Err(e) => return Err(auth_failure(e.to_string())),
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
        "payload": payload_binary(server_final),
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
    let Some(dn) = ctx.peer_cert_dn.clone() else {
        return Err(auth_failure(
            "MONGODB-X509 requires the client to present a verified TLS cert \
             (connection is plaintext or no client cert was offered)",
        ));
    };
    if let Some(claimed) = claimed {
        if !claimed.is_empty() && claimed != dn {
            return Err(auth_failure(format!(
                "MONGODB-X509: claimed user '{claimed}' doesn't match cert DN '{dn}'"
            )));
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
    let has_x509 = record
        .get_document("credentials")
        .map(|c| c.contains_key(MONGODB_X509))
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
    Document::from_reader(&mut bytes.as_slice()).ok()
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
    let mechanism = doc.get_str("mechanism").unwrap_or(SCRAM_SHA_256);
    if mechanism != MONGODB_X509 {
        return Err(bad_value(format!(
            "authenticate: only '{MONGODB_X509}' is supported on this command path; \
             use saslStart for '{SCRAM_SHA_256}'"
        )));
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

/// Coerce a `roles` argument into the canonical `[{role, db}]` shape and
/// validate each role name. Accepts the list-of-strings shorthand (each bound to
/// `default_db`) and the list-of-docs form. A role validates if it's a built-in
/// **or** a custom role that exists in storage (`get_role`); otherwise it's a
/// `RoleNotFound` (31) error.
fn normalise_roles(
    arg: Option<&Bson>,
    default_db: &str,
    ctx: &CommandContext,
) -> Result<Vec<Bson>, CommandError> {
    let items = match arg {
        Some(Bson::Array(items)) => items,
        None => return Ok(Vec::new()),
        _ => return Err(bad_value("createUser: roles must be an array")),
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let (role, db) = match item {
            Bson::String(role) => (role.clone(), default_db.to_string()),
            Bson::Document(d) => {
                let role = d
                    .get_str("role")
                    .map_err(|_| bad_value("createUser: role entry needs a string 'role'"))?;
                let db = d.get_str("db").unwrap_or(default_db);
                (role.to_string(), db.to_string())
            }
            _ => return Err(bad_value("createUser: roles must be strings or {role, db}")),
        };
        let known = rbac::is_known_role(&role)
            || ctx
                .storage()
                .ok()
                .and_then(|s| s.get_role(&db, &role).ok().flatten())
                .is_some();
        if !known {
            return Err(CommandError::new(
                ROLE_NOT_FOUND,
                "RoleNotFound",
                format!("Role {role}@{db} not found"),
            ));
        }
        out.push(Bson::Document(doc! { "role": role, "db": db }));
    }
    Ok(out)
}

/// `createUser` — store a user record with SCRAM-SHA-256 and/or MONGODB-X509
/// credentials. `mechanisms` selects which (default `["SCRAM-SHA-256"]`); a
/// password is required only when a SCRAM mechanism is requested (an
/// X509-only user authenticates by client cert, no password).
pub fn create_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let username = match doc.get_str("createUser") {
        Ok(u) if !u.is_empty() => u.to_string(),
        _ => return Err(bad_value("createUser: username (string) required")),
    };
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    // Requested mechanisms (default SCRAM-SHA-256). We implement SCRAM-SHA-256
    // and MONGODB-X509; an unknown mechanism is dropped.
    let requested: Vec<String> = match doc.get_array("mechanisms") {
        Ok(arr) if !arr.is_empty() => arr
            .iter()
            .filter_map(|m| m.as_str())
            .filter(|m| *m == SCRAM_SHA_256 || *m == MONGODB_X509)
            .map(String::from)
            .collect(),
        _ => vec![SCRAM_SHA_256.to_string()],
    };
    if requested.is_empty() {
        return Err(bad_value(format!(
            "createUser: mechanisms must contain at least one of '{SCRAM_SHA_256}', '{MONGODB_X509}'"
        )));
    }
    let scram_requested = requested.iter().any(|m| m == SCRAM_SHA_256);
    let x509_requested = requested.iter().any(|m| m == MONGODB_X509);

    let mut credentials = Document::new();
    if scram_requested {
        let pwd = match doc.get_str("pwd") {
            Ok(p) if !p.is_empty() => p.to_string(),
            _ => {
                return Err(bad_value(
                    "createUser: pwd (string) required when SCRAM mechanisms are requested",
                ))
            }
        };
        let creds = derive_credentials(&pwd, None, None)
            .map_err(|e| bad_value(format!("createUser: {e}")))?;
        credentials.insert(
            SCRAM_SHA_256,
            doc! {
                "iterationCount": creds.iteration_count as i32,
                "salt": creds.salt_b64(),
                "storedKey": creds.stored_key_b64(),
                "serverKey": creds.server_key_b64(),
            },
        );
    }
    if x509_requested {
        // The credential IS the cert presented at the TLS handshake; this
        // sentinel marks the user as X509-capable (matches the Python marker).
        credentials.insert(MONGODB_X509, "external");
    }

    let roles = normalise_roles(doc.get("roles"), &db_name, ctx)?;
    let mechanisms: Vec<Bson> = requested.iter().map(|m| Bson::String(m.clone())).collect();
    let record = doc! {
        "_id": format!("{db_name}.{username}"),
        "user": &username,
        "db": &db_name,
        "credentials": credentials,
        "roles": roles,
        "mechanisms": mechanisms,
    };
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
    Ok(doc! { "ok": 1.0 })
}

/// `dropUser` — remove a user record.
pub fn drop_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let username = match doc.get_str("dropUser") {
        Ok(u) if !u.is_empty() => u.to_string(),
        _ => return Err(bad_value("dropUser: username (string) required")),
    };
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    let removed = ctx
        .storage()?
        .drop_user(&db_name, &username)
        .map_err(crate::util::command_error)?;
    if !removed {
        return Err(CommandError::new(
            USER_NOT_FOUND,
            "UserNotFound",
            format!("User '{username}@{db_name}' not found"),
        ));
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

/// Rebuild the calling connection's effective roles from its authenticated
/// principals' current records (after a role change). Mirrors
/// `commands.py::_refresh_effective_roles`.
fn refresh_effective_roles(ctx: &CommandContext) {
    let Some(auth) = &ctx.conn_auth else { return };
    let Ok(mut auth) = auth.lock() else { return };
    let principals = auth.authenticated.clone();
    auth.effective_roles.clear();
    for (db, user) in principals {
        if let Some(roles) = lookup_roles(ctx, &db, &user) {
            auth.add_principal_roles(&roles);
        }
    }
}

/// `updateUser` — rotate the password and/or replace role bindings in place
/// (without invalidating other connections, matching mongod). At least one of
/// `pwd` / `roles` must be supplied.
pub fn update_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let username = match doc.get_str("updateUser") {
        Ok(u) if !u.is_empty() => u.to_string(),
        _ => return Err(bad_value("updateUser: username (string) required")),
    };
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    let Some(bytes) = ctx
        .storage()?
        .get_user(&db_name, &username)
        .map_err(crate::util::command_error)?
    else {
        return Err(CommandError::new(
            USER_NOT_FOUND,
            "UserNotFound",
            format!("User '{username}@{db_name}' not found"),
        ));
    };
    let mut record = Document::from_reader(&mut bytes.as_slice())
        .map_err(|e| CommandError::new(1, "InternalError", format!("decode user record: {e}")))?;

    let pwd = doc.get_str("pwd").ok();
    let has_roles = doc.contains_key("roles");
    if pwd.is_none() && !has_roles {
        return Err(bad_value(
            "updateUser: nothing to update (supply pwd and/or roles)",
        ));
    }
    if let Some(pwd) = pwd {
        if pwd.is_empty() {
            return Err(bad_value("updateUser: pwd must be a non-empty string"));
        }
        let creds = derive_credentials(pwd, None, None)
            .map_err(|e| bad_value(format!("updateUser: {e}")))?;
        record.insert(
            "credentials",
            doc! {
                SCRAM_SHA_256: {
                    "iterationCount": creds.iteration_count as i32,
                    "salt": creds.salt_b64(),
                    "storedKey": creds.stored_key_b64(),
                    "serverKey": creds.server_key_b64(),
                }
            },
        );
    }
    if has_roles {
        let roles = normalise_roles(doc.get("roles"), &db_name, ctx)?;
        record.insert("roles", roles);
    }
    let mut new_bytes = Vec::new();
    record
        .to_writer(&mut new_bytes)
        .map_err(|e| CommandError::new(1, "InternalError", format!("encode user record: {e}")))?;
    ctx.storage()?
        .add_user(&db_name, &username, &new_bytes, true)
        .map_err(crate::util::command_error)?;
    if has_roles {
        refresh_effective_roles(ctx);
    }
    Ok(doc! { "ok": 1.0 })
}

/// Load and decode a user record, or return `UserNotFound`.
fn load_user_record(
    ctx: &mut CommandContext,
    db_name: &str,
    username: &str,
) -> Result<Document, CommandError> {
    let Some(bytes) = ctx
        .storage()?
        .get_user(db_name, username)
        .map_err(crate::util::command_error)?
    else {
        return Err(CommandError::new(
            USER_NOT_FOUND,
            "UserNotFound",
            format!("User '{username}@{db_name}' not found"),
        ));
    };
    Document::from_reader(&mut bytes.as_slice())
        .map_err(|e| CommandError::new(1, "InternalError", format!("decode user record: {e}")))
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

/// `(role, db)` identity of a role-assignment entry, for dedup / set membership.
fn role_pair(b: &Bson) -> Option<(String, String)> {
    let d = b.as_document()?;
    Some((
        d.get_str("role").ok()?.to_string(),
        d.get_str("db").ok()?.to_string(),
    ))
}

/// `grantRolesToUser` — add roles to a user's assignment list (deduped by
/// `(role, db)`), taking effect immediately on the calling connection. Mirrors
/// the Python `grantRolesToUser` command.
pub fn grant_roles_to_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let username = match doc.get_str("grantRolesToUser") {
        Ok(u) if !u.is_empty() => u.to_string(),
        _ => return Err(bad_value("grantRolesToUser: username (string) required")),
    };
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    // Validate the requested roles (RoleNotFound) before touching the user.
    let additions = normalise_roles(doc.get("roles"), &db_name, ctx)?;
    let mut record = load_user_record(ctx, &db_name, &username)?;

    let mut roles: Vec<Bson> = record.get_array("roles").cloned().unwrap_or_default();
    let mut seen: Vec<(String, String)> = roles.iter().filter_map(role_pair).collect();
    for add in additions {
        if let Some(pair) = role_pair(&add) {
            if !seen.contains(&pair) {
                seen.push(pair);
                roles.push(add);
            }
        }
    }
    record.insert("roles", roles);
    save_user_record(ctx, &db_name, &username, &record)?;
    refresh_effective_roles(ctx);
    Ok(doc! { "ok": 1.0 })
}

/// `revokeRolesFromUser` — remove roles from a user's assignment list (matched
/// by `(role, db)`), taking effect immediately on the calling connection.
/// Mirrors the Python `revokeRolesFromUser` command.
pub fn revoke_roles_from_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let username = match doc.get_str("revokeRolesFromUser") {
        Ok(u) if !u.is_empty() => u.to_string(),
        _ => return Err(bad_value("revokeRolesFromUser: username (string) required")),
    };
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    let revocations = normalise_roles(doc.get("roles"), &db_name, ctx)?;
    let mut record = load_user_record(ctx, &db_name, &username)?;

    let drop_set: Vec<(String, String)> = revocations.iter().filter_map(role_pair).collect();
    let kept: Vec<Bson> = record
        .get_array("roles")
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| role_pair(r).map(|p| !drop_set.contains(&p)).unwrap_or(true))
        .collect();
    record.insert("roles", kept);
    save_user_record(ctx, &db_name, &username, &record)?;
    refresh_effective_roles(ctx);
    Ok(doc! { "ok": 1.0 })
}

/// `dropAllUsersFromDatabase` — drop every user bound to the calling db; returns
/// `n` = removed count.
pub fn drop_all_users_from_database(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    let storage = ctx.storage()?;
    let mut removed = 0_i32;
    loop {
        let batch = storage
            .list_users(Some(&db_name), 0, 1000)
            .map_err(crate::util::command_error)?;
        let n = batch.len();
        for bytes in &batch {
            if let Ok(rec) = Document::from_reader(&mut bytes.as_slice()) {
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
    Ok(doc! { "ok": 1.0, "n": removed })
}

/// `usersInfo` — list user records (optionally with credentials).
pub fn users_info(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    let show_credentials = doc.get_bool("showCredentials").unwrap_or(false);
    let storage = ctx.storage()?;

    // Resolve the requested principal records per the `usersInfo` argument form.
    let mut raw: Vec<Vec<u8>> = Vec::new();
    match doc.get("usersInfo") {
        // `{usersInfo: 1}` / `{usersInfo: true}` ⇒ all users in this db.
        Some(Bson::Int32(1))
        | Some(Bson::Int64(1))
        | Some(Bson::Double(_))
        | Some(Bson::Boolean(true)) => {
            raw = storage
                .list_users(Some(&db_name), 0, 0)
                .map_err(crate::util::command_error)?;
        }
        // `{usersInfo: "name"}` ⇒ a single named user in this db.
        Some(Bson::String(name)) => {
            if let Some(r) = storage
                .get_user(&db_name, name)
                .map_err(crate::util::command_error)?
            {
                raw.push(r);
            }
        }
        // `{usersInfo: {user, db}}` ⇒ a single fully-qualified principal.
        Some(Bson::Document(spec)) => {
            if let Ok(u) = spec.get_str("user") {
                let d = spec.get_str("db").unwrap_or(&db_name);
                if let Some(r) = storage.get_user(d, u).map_err(crate::util::command_error)? {
                    raw.push(r);
                }
            }
        }
        // `{usersInfo: [ ... ]}` ⇒ a list of names / principals.
        Some(Bson::Array(items)) => {
            for item in items {
                let rec = match item {
                    Bson::String(name) => storage.get_user(&db_name, name),
                    Bson::Document(spec) => match spec.get_str("user") {
                        Ok(u) => {
                            let d = spec.get_str("db").unwrap_or(&db_name);
                            storage.get_user(d, u)
                        }
                        Err(_) => Ok(None),
                    },
                    _ => Ok(None),
                }
                .map_err(crate::util::command_error)?;
                if let Some(r) = rec {
                    raw.push(r);
                }
            }
        }
        _ => {}
    }

    let mut users = Vec::with_capacity(raw.len());
    for bytes in raw {
        let mut record = Document::from_reader(&mut bytes.as_slice()).map_err(|e| {
            CommandError::new(1, "InternalError", format!("decode user record: {e}"))
        })?;
        if !show_credentials {
            record.remove("credentials");
        }
        users.push(Bson::Document(record));
    }
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
        let mut pairs: Vec<(String, String)> = lookup_roles(ctx, "admin", user)
            .unwrap_or_default()
            .iter()
            .filter_map(role_pair)
            .collect();
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
            &doc! {"saslStart": 1, "mechanism": "SCRAM-SHA-1", "payload": payload_binary(b"n,,n=x,r=y".to_vec()), "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(reply.get_i32("code").unwrap(), AUTHENTICATION_FAILED);
    }

    #[test]
    fn x509_create_user_needs_no_password() {
        let (mut ctx, _auth) = ctx_with_store();
        // X509-only user: no pwd required.
        let reply = dispatch(
            &doc! {
                "createUser": "CN=alice",
                "mechanisms": [MONGODB_X509],
                "roles": [{"role": "readWrite", "db": "app"}],
                "$db": "admin",
            },
            &mut ctx,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply:?}");
        // The stored record carries the X509 credential marker.
        let info = dispatch(
            &doc! {"usersInfo": "CN=alice", "showCredentials": true, "$db": "admin"},
            &mut ctx,
        );
        let creds = info.get_array("users").unwrap()[0]
            .as_document()
            .unwrap()
            .get_document("credentials")
            .unwrap()
            .clone();
        assert!(creds.contains_key(MONGODB_X509));
        assert!(!creds.contains_key(SCRAM_SHA_256));
    }

    #[test]
    fn x509_saslstart_authenticates_with_matching_cert() {
        let (mut ctx, _auth) = ctx_with_store();
        dispatch(
            &doc! {"createUser": "CN=bob", "mechanisms": [MONGODB_X509], "roles": [], "$db": "admin"},
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
        dispatch(
            &doc! {"createUser": "CN=dave", "mechanisms": [MONGODB_X509], "roles": [], "$db": "admin"},
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
        dispatch(
            &doc! {"createUser": "CN=erin", "mechanisms": [MONGODB_X509], "roles": [], "$db": "admin"},
            &mut ctx,
        );
        ctx.peer_cert_dn = Some("CN=erin".to_string());
        let reply = dispatch(
            &doc! {"authenticate": 1, "mechanism": MONGODB_X509, "user": "CN=erin", "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply:?}");
        assert_eq!(reply.get_str("user").unwrap(), "CN=erin");
        // authenticate with SCRAM mechanism is rejected on this path.
        let scram = dispatch(
            &doc! {"authenticate": 1, "mechanism": SCRAM_SHA_256, "user": "x", "$db": "admin"},
            &mut ctx,
        );
        assert_eq!(scram.get_i32("code").unwrap(), 2);
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
