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
//! ## Deferred (later slices, tracked in `tasks/rust-server-plan.md`)
//!
//! * **Custom user-defined roles** — the `createRole` / `updateRole` family and
//!   the inheritance-graph resolver `check_privilege` threads through in Python.
//!   Only built-in role names validate / grant today.
//! * **`updateUser`** / **`dropAllUsersFromDatabase`** / `saslSupportedMechs`
//!   handshake hinting.
//! * **SCRAM-SHA-1** (legacy MD5 prepass) and **MONGODB-X509** (needs TLS, R5c).

use std::sync::{Arc, Mutex};

use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};

use secantus_auth::{begin_scram, continue_scram, derive_credentials, peek_username, ScramState};

use crate::{rbac, CommandContext, CommandError, HandlerResult};

/// The only mechanism this slice implements (the modern driver default).
const SCRAM_SHA_256: &str = "SCRAM-SHA-256";
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
}

impl ConnectionAuth {
    pub fn new() -> Self {
        ConnectionAuth {
            scram: None,
            next_conversation_id: 0,
            authenticated: Vec::new(),
            effective_roles: Vec::new(),
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
    if mechanism != SCRAM_SHA_256 {
        return Err(auth_failure(format!(
            "Unsupported SASL mechanism: '{mechanism}' (supported: {SCRAM_SHA_256})"
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
    let mut auth = auth.lock().expect("conn auth mutex poisoned");
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
    let mut auth = auth.lock().expect("conn auth mutex poisoned");
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

/// A `BadValue` (2) command error.
fn bad_value(msg: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", msg)
}

/// `RoleNotFound`.
const ROLE_NOT_FOUND: i32 = 31;

/// Coerce a `roles` argument into the canonical `[{role, db}]` shape and
/// validate each role name against the built-in catalogue. Accepts the
/// list-of-strings shorthand (each bound to `default_db`) and the list-of-docs
/// form. An unrecognised role name is a `RoleNotFound` (31) error. Custom
/// user-defined roles aren't recognised yet (deferred with the `createRole`
/// family), so only built-in role names validate.
fn normalise_roles(arg: Option<&Bson>, default_db: &str) -> Result<Vec<Bson>, CommandError> {
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
        if !rbac::is_known_role(&role) {
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

/// `createUser` — derive SCRAM-SHA-256 credentials and store the user record.
pub fn create_user(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let username = match doc.get_str("createUser") {
        Ok(u) if !u.is_empty() => u.to_string(),
        _ => return Err(bad_value("createUser: username (string) required")),
    };
    let pwd = match doc.get_str("pwd") {
        Ok(p) if !p.is_empty() => p.to_string(),
        _ => {
            return Err(bad_value(
                "createUser: pwd (string) required when SCRAM mechanisms are requested",
            ))
        }
    };
    let db_name = if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    };
    let roles = normalise_roles(doc.get("roles"), &db_name)?;

    let creds = derive_credentials(&pwd, None, None);
    let credentials = doc! {
        SCRAM_SHA_256: {
            "iterationCount": creds.iteration_count as i32,
            "salt": creds.salt_b64(),
            "storedKey": creds.stored_key_b64(),
            "serverKey": creds.server_key_b64(),
        }
    };
    let record = doc! {
        "_id": format!("{db_name}.{username}"),
        "user": &username,
        "db": &db_name,
        "credentials": credentials,
        "roles": roles,
        "mechanisms": [SCRAM_SHA_256],
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
        let mut auth = auth.lock().expect("conn auth mutex poisoned");
        auth.authenticated
            .retain(|(d, u)| !(d == &db_name && u == &username));
    }
    Ok(doc! { "ok": 1.0 })
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
            let mut users = self.users.lock().unwrap();
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
