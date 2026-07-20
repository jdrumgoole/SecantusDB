//! Custom user-defined role management (R5b-3 / R5b-4): `createRole` /
//! `updateRole` / `dropRole` / `dropAllRolesFromDatabase` / `rolesInfo`, plus the
//! incremental `grant`/`revoke` quartet (`grantPrivilegesToRole` /
//! `revokePrivilegesFromRole` / `grantRolesToRole` / `revokeRolesFromRole`).
//!
//! A port of `commands.py`'s role handlers. Custom roles carry a `privileges`
//! array (`[{resource, actions}]`) plus an inherited-`roles` array, stored as
//! opaque BSON in the role table (`Storage::add_role` / `get_role` / `drop_role`
//! / `list_roles`). The dispatcher's RBAC check expands them through
//! [`crate::rbac::check_privilege_resolved`] (privilege match + inheritance walk
//! with cycle detection).
//!
//! Stored record shape (identical to the Python server's, so both share the
//! role table):
//!
//! ```text
//! { _id: "<db>.<role>", role, db,
//!   privileges: [{ resource: {...}, actions: [...] }, ...],
//!   roles: [{ role, db }, ...] }
//! ```

use bson::{doc, Bson, Document};

use crate::util::command_error;
use crate::{rbac, CommandContext, CommandError, HandlerResult};

/// `RoleNotFound`.
const ROLE_NOT_FOUND: i32 = 31;
/// `Location51002` — a role with that name already exists.
const ROLE_ALREADY_EXISTS: i32 = 51002;

fn bad_value(msg: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", msg)
}

fn role_not_found(name: &str, db: &str) -> CommandError {
    CommandError::new(
        ROLE_NOT_FOUND,
        "RoleNotFound",
        format!("Role '{name}' not found on database '{db}'"),
    )
}

fn db_of(ctx: &CommandContext) -> String {
    if ctx.db_name.is_empty() {
        "admin".to_string()
    } else {
        ctx.db_name.clone()
    }
}

/// Validate / normalise a `privileges` array into `[{resource, actions}]`.
/// Returns `Err(BadValue)` on a malformed entry; an empty list is fine (a role
/// can be a pure inheritor). Mirrors `commands.py::_normalise_privileges`.
fn normalise_privileges(arg: Option<&Bson>) -> Result<Vec<Bson>, CommandError> {
    let items = match arg {
        Some(Bson::Array(items)) => items,
        None => return Ok(Vec::new()),
        _ => {
            return Err(bad_value(
                "privileges must be an array of {resource, actions}",
            ))
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for priv_ in items {
        let Bson::Document(p) = priv_ else {
            return Err(bad_value(
                "privileges must be an array of {resource, actions}",
            ));
        };
        let resource = p
            .get_document("resource")
            .map_err(|_| bad_value("privilege.resource must be a document"))?;
        let actions = p
            .get_array("actions")
            .map_err(|_| bad_value("privilege.actions must be an array"))?;
        if !actions.iter().all(|a| matches!(a, Bson::String(_))) {
            return Err(bad_value("privilege.actions must be an array of strings"));
        }
        out.push(Bson::Document(doc! {
            "resource": resource.clone(),
            "actions": actions.clone(),
        }));
    }
    Ok(out)
}

/// Validate / normalise an inherited-`roles` array into `[{role, db}]`. Each
/// entry is `"<name>"` (bound to `default_db`) or `{role, db}`. Mirrors
/// `commands.py::_normalise_inherited_roles`.
fn normalise_inherited_roles(
    arg: Option<&Bson>,
    default_db: &str,
) -> Result<Vec<Bson>, CommandError> {
    let items = match arg {
        Some(Bson::Array(items)) => items,
        None => return Ok(Vec::new()),
        _ => {
            return Err(bad_value(
                "roles must be a list of names or {role, db} dicts",
            ))
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for entry in items {
        let (role, db) = match entry {
            Bson::String(s) if !s.is_empty() => (s.clone(), default_db.to_string()),
            Bson::Document(d) => {
                let role = d
                    .get_str("role")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| bad_value("inherited role needs a non-empty 'role'"))?;
                let db = d.get_str("db").unwrap_or(default_db);
                if db.is_empty() {
                    return Err(bad_value("inherited role 'db' must be non-empty"));
                }
                (role.to_string(), db.to_string())
            }
            _ => {
                return Err(bad_value(
                    "roles must be a list of names or {role, db} dicts",
                ))
            }
        };
        out.push(Bson::Document(doc! { "role": role, "db": db }));
    }
    Ok(out)
}

/// `createRole` — define a custom role. Rejects built-in names (mongod refuses
/// `createRole: "read"` etc.).
pub fn create_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = match doc.get_str("createRole") {
        Ok(n) if !n.is_empty() => n.to_string(),
        _ => return Err(bad_value("createRole: role name (string) required")),
    };
    if rbac::is_known_role(&name) {
        return Err(bad_value(format!(
            "Cannot create role with name '{name}': name is reserved for a built-in"
        )));
    }
    let db_name = db_of(ctx);
    let privileges = normalise_privileges(doc.get("privileges"))?;
    let inherited = normalise_inherited_roles(doc.get("roles"), &db_name)?;
    let record = doc! {
        "_id": format!("{db_name}.{name}"),
        "role": &name,
        "db": &db_name,
        "privileges": privileges,
        "roles": inherited,
    };
    let mut bytes = Vec::new();
    record
        .to_writer(&mut bytes)
        .map_err(|e| CommandError::new(1, "InternalError", format!("encode role record: {e}")))?;
    let added = ctx
        .storage()?
        .add_role(&db_name, &name, &bytes, false)
        .map_err(command_error)?;
    if !added {
        return Err(CommandError::new(
            ROLE_ALREADY_EXISTS,
            "Location51002",
            format!("Role \"{name}@{db_name}\" already exists"),
        ));
    }
    Ok(doc! { "ok": 1.0 })
}

/// Read + decode a stored role record for `(db, name)`.
fn get_role_doc(
    ctx: &CommandContext,
    db: &str,
    name: &str,
) -> Result<Option<Document>, CommandError> {
    let Some(bytes) = ctx.storage()?.get_role(db, name).map_err(command_error)? else {
        return Ok(None);
    };
    Document::from_reader(&mut bytes.as_slice())
        .map(Some)
        .map_err(|e| CommandError::new(1, "InternalError", format!("decode role record: {e}")))
}

/// `updateRole` — replace a custom role's privileges / inherited roles in place.
/// Either field may be supplied; omitted fields stay as-is.
pub fn update_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = match doc.get_str("updateRole") {
        Ok(n) if !n.is_empty() => n.to_string(),
        _ => return Err(bad_value("updateRole: role name (string) required")),
    };
    let db_name = db_of(ctx);
    let Some(mut record) = get_role_doc(ctx, &db_name, &name)? else {
        return Err(role_not_found(&name, &db_name));
    };
    if doc.contains_key("privileges") {
        let privileges = normalise_privileges(doc.get("privileges"))?;
        record.insert("privileges", privileges);
    }
    if doc.contains_key("roles") {
        let inherited = normalise_inherited_roles(doc.get("roles"), &db_name)?;
        record.insert("roles", inherited);
    }
    let mut bytes = Vec::new();
    record
        .to_writer(&mut bytes)
        .map_err(|e| CommandError::new(1, "InternalError", format!("encode role record: {e}")))?;
    ctx.storage()?
        .add_role(&db_name, &name, &bytes, true)
        .map_err(command_error)?;
    Ok(doc! { "ok": 1.0 })
}

/// `dropRole` — remove a custom role.
pub fn drop_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = match doc.get_str("dropRole") {
        Ok(n) if !n.is_empty() => n.to_string(),
        _ => return Err(bad_value("dropRole: role name (string) required")),
    };
    let db_name = db_of(ctx);
    if !ctx
        .storage()?
        .drop_role(&db_name, &name)
        .map_err(command_error)?
    {
        return Err(role_not_found(&name, &db_name));
    }
    Ok(doc! { "ok": 1.0 })
}

/// Encode + persist a (replace) role record.
fn save_role(
    ctx: &CommandContext,
    db: &str,
    name: &str,
    record: &Document,
) -> Result<(), CommandError> {
    let mut bytes = Vec::new();
    record
        .to_writer(&mut bytes)
        .map_err(|e| CommandError::new(1, "InternalError", format!("encode role record: {e}")))?;
    ctx.storage()?
        .add_role(db, name, &bytes, true)
        .map_err(command_error)?;
    Ok(())
}

/// The `actions` string list of a privilege document.
fn priv_actions(p: &Document) -> Vec<String> {
    p.get_array("actions")
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// `grantPrivilegesToRole` — merge privileges into a custom role (by resource,
/// deduping actions).
pub fn grant_privileges_to_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = match doc.get_str("grantPrivilegesToRole") {
        Ok(n) if !n.is_empty() => n.to_string(),
        _ => {
            return Err(bad_value(
                "grantPrivilegesToRole: role name (string) required",
            ))
        }
    };
    let db_name = db_of(ctx);
    let Some(mut record) = get_role_doc(ctx, &db_name, &name)? else {
        return Err(role_not_found(&name, &db_name));
    };
    let additions = normalise_privileges(doc.get("privileges"))?;
    let mut privs: Vec<Document> = record
        .get_array("privileges")
        .map(|a| a.iter().filter_map(|p| p.as_document().cloned()).collect())
        .unwrap_or_default();
    for add in &additions {
        let Bson::Document(add) = add else { continue };
        let add_resource = add.get_document("resource").ok();
        let mut merged = false;
        for existing in privs.iter_mut() {
            if existing.get_document("resource").ok() == add_resource {
                let mut actions = priv_actions(existing);
                for a in priv_actions(add) {
                    if !actions.contains(&a) {
                        actions.push(a);
                    }
                }
                existing.insert("actions", actions);
                merged = true;
                break;
            }
        }
        if !merged {
            privs.push(add.clone());
        }
    }
    record.insert(
        "privileges",
        privs.into_iter().map(Bson::Document).collect::<Vec<_>>(),
    );
    save_role(ctx, &db_name, &name, &record)?;
    Ok(doc! { "ok": 1.0 })
}

/// `revokePrivilegesFromRole` — remove actions from matching-resource
/// privileges; privileges left with no actions are dropped.
pub fn revoke_privileges_from_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = match doc.get_str("revokePrivilegesFromRole") {
        Ok(n) if !n.is_empty() => n.to_string(),
        _ => {
            return Err(bad_value(
                "revokePrivilegesFromRole: role name (string) required",
            ))
        }
    };
    let db_name = db_of(ctx);
    let Some(mut record) = get_role_doc(ctx, &db_name, &name)? else {
        return Err(role_not_found(&name, &db_name));
    };
    let revocations = normalise_privileges(doc.get("privileges"))?;
    let mut privs: Vec<Document> = record
        .get_array("privileges")
        .map(|a| a.iter().filter_map(|p| p.as_document().cloned()).collect())
        .unwrap_or_default();
    for rev in &revocations {
        let Bson::Document(rev) = rev else { continue };
        let rev_resource = rev.get_document("resource").ok();
        let rev_actions = priv_actions(rev);
        for existing in privs.iter_mut() {
            if existing.get_document("resource").ok() == rev_resource {
                let kept: Vec<String> = priv_actions(existing)
                    .into_iter()
                    .filter(|a| !rev_actions.contains(a))
                    .collect();
                existing.insert("actions", kept);
            }
        }
    }
    privs.retain(|p| !priv_actions(p).is_empty());
    record.insert(
        "privileges",
        privs.into_iter().map(Bson::Document).collect::<Vec<_>>(),
    );
    save_role(ctx, &db_name, &name, &record)?;
    Ok(doc! { "ok": 1.0 })
}

/// The `(role, db)` pairs of an inherited-roles array.
fn role_pairs(arr: &[Bson]) -> Vec<(String, String)> {
    arr.iter()
        .filter_map(|r| {
            let d = r.as_document()?;
            Some((
                d.get_str("role").ok()?.to_string(),
                d.get_str("db").ok()?.to_string(),
            ))
        })
        .collect()
}

/// `grantRolesToRole` — add inherited roles (deduped).
pub fn grant_roles_to_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = match doc.get_str("grantRolesToRole") {
        Ok(n) if !n.is_empty() => n.to_string(),
        _ => return Err(bad_value("grantRolesToRole: role name (string) required")),
    };
    let db_name = db_of(ctx);
    let Some(mut record) = get_role_doc(ctx, &db_name, &name)? else {
        return Err(role_not_found(&name, &db_name));
    };
    let additions = normalise_inherited_roles(doc.get("roles"), &db_name)?;
    let mut inherited: Vec<Bson> = record.get_array("roles").cloned().unwrap_or_default();
    let mut seen: Vec<(String, String)> = role_pairs(&inherited);
    for add in additions {
        if let Bson::Document(d) = &add {
            let pair = (
                d.get_str("role").unwrap_or("").to_string(),
                d.get_str("db").unwrap_or("").to_string(),
            );
            if !seen.contains(&pair) {
                seen.push(pair);
                inherited.push(add);
            }
        }
    }
    record.insert("roles", inherited);
    save_role(ctx, &db_name, &name, &record)?;
    Ok(doc! { "ok": 1.0 })
}

/// `revokeRolesFromRole` — remove inherited roles.
pub fn revoke_roles_from_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = match doc.get_str("revokeRolesFromRole") {
        Ok(n) if !n.is_empty() => n.to_string(),
        _ => {
            return Err(bad_value(
                "revokeRolesFromRole: role name (string) required",
            ))
        }
    };
    let db_name = db_of(ctx);
    let Some(mut record) = get_role_doc(ctx, &db_name, &name)? else {
        return Err(role_not_found(&name, &db_name));
    };
    let revocations = normalise_inherited_roles(doc.get("roles"), &db_name)?;
    let drop_set = role_pairs(&revocations);
    let kept: Vec<Bson> = record
        .get_array("roles")
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| {
            r.as_document()
                .and_then(|d| {
                    Some((
                        d.get_str("role").ok()?.to_string(),
                        d.get_str("db").ok()?.to_string(),
                    ))
                })
                .map(|pair| !drop_set.contains(&pair))
                .unwrap_or(true)
        })
        .collect();
    record.insert("roles", kept);
    save_role(ctx, &db_name, &name, &record)?;
    Ok(doc! { "ok": 1.0 })
}

/// `dropAllRolesFromDatabase` — drop every custom role bound to the calling db.
/// Returns `n` = removed count. Built-in roles are never persisted, so they're
/// unaffected.
pub fn drop_all_roles_from_database(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let db_name = db_of(ctx);
    let storage = ctx.storage()?;
    let mut removed = 0;
    loop {
        let batch = storage
            .list_roles(Some(&db_name), 0, 1000)
            .map_err(command_error)?;
        let n = batch.len();
        for bytes in &batch {
            if let Ok(rec) = Document::from_reader(&mut bytes.as_slice()) {
                if let Ok(role) = rec.get_str("role") {
                    if storage.drop_role(&db_name, role).map_err(command_error)? {
                        removed += 1;
                    }
                }
            }
        }
        if n < 1000 {
            break;
        }
    }
    Ok(doc! { "ok": 1.0, "n": removed })
}

/// `rolesInfo` — return custom-role records. Accepts `1`/`true` (all roles in
/// this db), a name string, a `{role, db}` doc, or a list thereof.
pub fn roles_info(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let db_name = db_of(ctx);
    let storage = ctx.storage()?;

    let mut raw: Vec<Vec<u8>> = Vec::new();
    match doc.get("rolesInfo") {
        Some(Bson::Int32(1)) | Some(Bson::Int64(1)) | Some(Bson::Boolean(true)) => {
            raw = storage
                .list_roles(Some(&db_name), 0, 0)
                .map_err(command_error)?;
        }
        Some(Bson::String(name)) => {
            if let Some(r) = storage.get_role(&db_name, name).map_err(command_error)? {
                raw.push(r);
            }
        }
        Some(Bson::Document(spec)) => {
            if let Ok(role) = spec.get_str("role") {
                let d = spec.get_str("db").unwrap_or(&db_name);
                if let Some(r) = storage.get_role(d, role).map_err(command_error)? {
                    raw.push(r);
                }
            }
        }
        Some(Bson::Array(items)) => {
            for item in items {
                let rec = match item {
                    Bson::String(name) => storage.get_role(&db_name, name),
                    Bson::Document(spec) => match spec.get_str("role") {
                        Ok(role) => {
                            let d = spec.get_str("db").unwrap_or(&db_name);
                            storage.get_role(d, role)
                        }
                        Err(_) => Ok(None),
                    },
                    _ => Ok(None),
                }
                .map_err(command_error)?;
                if let Some(r) = rec {
                    raw.push(r);
                }
            }
        }
        _ => {}
    }

    let mut roles = Vec::with_capacity(raw.len());
    for bytes in raw {
        let record = Document::from_reader(&mut bytes.as_slice()).map_err(|e| {
            CommandError::new(1, "InternalError", format!("decode role record: {e}"))
        })?;
        roles.push(Bson::Document(record));
    }
    Ok(doc! { "roles": roles, "ok": 1.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    /// In-memory user + role store (only the role/user methods are overridden).
    #[derive(Default)]
    struct RoleStore {
        roles: StdMutex<HashMap<(String, String), Vec<u8>>>,
    }

    impl crate::Storage for RoleStore {
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
        fn add_role(
            &self,
            db: &str,
            name: &str,
            record: &[u8],
            replace: bool,
        ) -> Result<bool, crate::StorageError> {
            let mut roles = self.roles.lock().unwrap_or_else(|e| e.into_inner());
            let key = (db.to_string(), name.to_string());
            if roles.contains_key(&key) && !replace {
                return Ok(false);
            }
            roles.insert(key, record.to_vec());
            Ok(true)
        }
        fn get_role(&self, db: &str, name: &str) -> Result<Option<Vec<u8>>, crate::StorageError> {
            Ok(self
                .roles
                .lock()
                .unwrap()
                .get(&(db.to_string(), name.to_string()))
                .cloned())
        }
        fn drop_role(&self, db: &str, name: &str) -> Result<bool, crate::StorageError> {
            Ok(self
                .roles
                .lock()
                .unwrap()
                .remove(&(db.to_string(), name.to_string()))
                .is_some())
        }
        fn list_roles(
            &self,
            db: Option<&str>,
            _skip: usize,
            _limit: usize,
        ) -> Result<Vec<Vec<u8>>, crate::StorageError> {
            Ok(self
                .roles
                .lock()
                .unwrap()
                .iter()
                .filter(|((d, _), _)| db.is_none() || db == Some(d.as_str()))
                .map(|(_, v)| v.clone())
                .collect())
        }
    }

    fn ctx() -> CommandContext {
        let store: std::sync::Arc<dyn crate::Storage> = std::sync::Arc::new(RoleStore::default());
        let mut c = CommandContext::new(1).with_storage(store);
        c.db_name = "admin".to_string();
        c
    }

    #[test]
    fn create_then_roles_info_and_drop() {
        let mut c = ctx();
        let create = dispatch(
            &doc! {
                "createRole": "appReader",
                "privileges": [{"resource": {"db": "app", "collection": ""}, "actions": ["find"]}],
                "roles": [],
            },
            &mut c,
        );
        assert_eq!(create.get_f64("ok").unwrap(), 1.0, "{create:?}");

        // duplicate
        let dup = dispatch(
            &doc! {"createRole": "appReader", "privileges": [], "roles": []},
            &mut c,
        );
        assert_eq!(dup.get_i32("code").unwrap(), ROLE_ALREADY_EXISTS);

        // built-in name rejected
        let builtin = dispatch(
            &doc! {"createRole": "read", "privileges": [], "roles": []},
            &mut c,
        );
        assert_eq!(builtin.get_i32("code").unwrap(), 2);

        // rolesInfo by name
        let info = dispatch(&doc! {"rolesInfo": "appReader"}, &mut c);
        let roles = info.get_array("roles").unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(
            roles[0].as_document().unwrap().get_str("role").unwrap(),
            "appReader"
        );

        // drop
        assert_eq!(
            dispatch(&doc! {"dropRole": "appReader"}, &mut c)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        assert_eq!(
            dispatch(&doc! {"dropRole": "appReader"}, &mut c)
                .get_i32("code")
                .unwrap(),
            ROLE_NOT_FOUND
        );
    }

    #[test]
    fn update_role_replaces_fields() {
        let mut c = ctx();
        dispatch(
            &doc! {"createRole": "r", "privileges": [], "roles": []},
            &mut c,
        );
        let upd = dispatch(
            &doc! {
                "updateRole": "r",
                "privileges": [{"resource": {"db": "app", "collection": ""}, "actions": ["insert"]}],
            },
            &mut c,
        );
        assert_eq!(upd.get_f64("ok").unwrap(), 1.0);
        let info = dispatch(&doc! {"rolesInfo": "r"}, &mut c);
        let rec = info.get_array("roles").unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        let privs = rec.get_array("privileges").unwrap();
        assert_eq!(privs.len(), 1);

        // updateRole on a missing role → RoleNotFound
        let missing = dispatch(&doc! {"updateRole": "ghost", "roles": []}, &mut c);
        assert_eq!(missing.get_i32("code").unwrap(), ROLE_NOT_FOUND);
    }

    #[test]
    fn custom_role_privilege_check_via_resolver() {
        // Build a role record and verify check_privilege_resolved honours it,
        // including inheritance.
        let mut c = ctx();
        dispatch(
            &doc! {
                "createRole": "writer",
                "privileges": [{"resource": {"db": "app", "collection": ""}, "actions": ["insert"]}],
                "roles": [],
            },
            &mut c,
        );
        dispatch(
            &doc! {
                "createRole": "super",
                "privileges": [],
                "roles": [{"role": "writer", "db": "admin"}],
            },
            &mut c,
        );
        let storage = c.storage().unwrap();
        let resolver = |db: &str, role: &str| -> Option<Document> {
            let bytes = storage.get_role(db, role).ok()??;
            Document::from_reader(&mut bytes.as_slice()).ok()
        };
        // direct grant
        assert!(rbac::check_privilege_resolved(
            &[("writer".into(), "admin".into())],
            rbac::A_INSERT,
            Some("app"),
            false,
            Some(&resolver),
        ));
        // inherited grant (super → writer)
        assert!(rbac::check_privilege_resolved(
            &[("super".into(), "admin".into())],
            rbac::A_INSERT,
            Some("app"),
            false,
            Some(&resolver),
        ));
        // not granted on a different db
        assert!(!rbac::check_privilege_resolved(
            &[("writer".into(), "admin".into())],
            rbac::A_INSERT,
            Some("other"),
            false,
            Some(&resolver),
        ));
    }

    fn privileges(rec: &Document) -> Vec<Document> {
        rec.get_array("privileges")
            .unwrap()
            .iter()
            .map(|p| p.as_document().unwrap().clone())
            .collect()
    }

    fn fetch(c: &mut CommandContext, name: &str) -> Document {
        let info = dispatch(&doc! {"rolesInfo": name}, c);
        info.get_array("roles").unwrap()[0]
            .as_document()
            .unwrap()
            .clone()
    }

    #[test]
    fn grant_and_revoke_privileges() {
        let mut c = ctx();
        dispatch(
            &doc! {"createRole": "r", "privileges": [], "roles": []},
            &mut c,
        );
        // grant find on app
        dispatch(
            &doc! {
                "grantPrivilegesToRole": "r",
                "privileges": [{"resource": {"db": "app", "collection": ""}, "actions": ["find"]}],
            },
            &mut c,
        );
        // grant insert on the same resource → merges into one privilege, two actions
        dispatch(
            &doc! {
                "grantPrivilegesToRole": "r",
                "privileges": [{"resource": {"db": "app", "collection": ""}, "actions": ["insert"]}],
            },
            &mut c,
        );
        let privs = privileges(&fetch(&mut c, "r"));
        assert_eq!(privs.len(), 1);
        let actions: Vec<&str> = privs[0]
            .get_array("actions")
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert!(actions.contains(&"find") && actions.contains(&"insert"));

        // revoke find → only insert left
        dispatch(
            &doc! {
                "revokePrivilegesFromRole": "r",
                "privileges": [{"resource": {"db": "app", "collection": ""}, "actions": ["find"]}],
            },
            &mut c,
        );
        let privs = privileges(&fetch(&mut c, "r"));
        assert_eq!(privs[0].get_array("actions").unwrap().len(), 1);

        // revoke the last action → the privilege is dropped entirely
        dispatch(
            &doc! {
                "revokePrivilegesFromRole": "r",
                "privileges": [{"resource": {"db": "app", "collection": ""}, "actions": ["insert"]}],
            },
            &mut c,
        );
        assert!(privileges(&fetch(&mut c, "r")).is_empty());
    }

    #[test]
    fn grant_and_revoke_inherited_roles() {
        let mut c = ctx();
        dispatch(
            &doc! {"createRole": "base", "privileges": [], "roles": []},
            &mut c,
        );
        dispatch(
            &doc! {"createRole": "r", "privileges": [], "roles": []},
            &mut c,
        );
        // grant base (twice → deduped)
        dispatch(
            &doc! {"grantRolesToRole": "r", "roles": [{"role": "base", "db": "admin"}]},
            &mut c,
        );
        dispatch(
            &doc! {"grantRolesToRole": "r", "roles": [{"role": "base", "db": "admin"}]},
            &mut c,
        );
        assert_eq!(fetch(&mut c, "r").get_array("roles").unwrap().len(), 1);

        // revoke base
        dispatch(
            &doc! {"revokeRolesFromRole": "r", "roles": [{"role": "base", "db": "admin"}]},
            &mut c,
        );
        assert!(fetch(&mut c, "r").get_array("roles").unwrap().is_empty());
    }

    #[test]
    fn grant_to_missing_role_is_role_not_found() {
        let mut c = ctx();
        let reply = dispatch(&doc! {"grantRolesToRole": "ghost", "roles": []}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), ROLE_NOT_FOUND);
    }
}
