//! Role management: `createRole` / `updateRole` / `dropRole` /
//! `dropAllRolesFromDatabase` / `rolesInfo`, plus the incremental quartet
//! (`grantPrivilegesToRole` / `revokePrivilegesFromRole` / `grantRolesToRole`
//! / `revokeRolesFromRole`). Every reply was measured on mongod 8.2.11 with
//! `tools/probes/user_role_commands.py` (2026-10-13).
//!
//! A custom role carries a `privileges` array (`[{resource, actions}]`,
//! one per resource, actions in mongod's order -- see [`crate::privileges`])
//! and the `roles` it holds, stored as opaque BSON in the role table
//! (`Storage::add_role` / `get_role` / `drop_role` / `list_roles`). The
//! dispatcher's RBAC check expands them through
//! [`crate::rbac::check_privilege_resolved`].
//!
//! Stored record shape (identical to the Python server's, so both share the
//! role table):
//!
//! ```text
//! { _id: "<db>.<role>", role, db,
//!   privileges: [{ resource: {...}, actions: [...] }, ...],
//!   roles: [{ role, db }, ...],
//!   authenticationRestrictions: [...] }       -- when given
//! ```
//!
//! Three rules of mongod's worth knowing. A role of a database other than
//! `admin` may hold only that database's namespaces and only that
//! database's roles (`InvalidRoleModification`, 49). A built-in role exists
//! on the databases mongod gives it to -- `root` only on `admin` -- and
//! cannot be modified, dropped or shadowed. And `dropRole` takes the role
//! away from every user and role that held it, while
//! `dropAllRolesFromDatabase` takes nothing away from anyone.

use bson::{doc, Bson, Document};

use crate::privileges::{self, Privilege, PrivilegeSet};
use crate::umc::{self, bad_value, Field};
use crate::util::command_error;
use crate::{CommandContext, CommandError, HandlerResult};

/// `RoleNotFound`.
const ROLE_NOT_FOUND: i32 = 31;
/// `InvalidRoleModification`.
const INVALID_ROLE_MODIFICATION: i32 = 49;
/// `Location51002` — a role with that name already exists.
const ROLE_ALREADY_EXISTS: i32 = 51002;

fn role_not_found(message: String) -> CommandError {
    CommandError::new(ROLE_NOT_FOUND, "RoleNotFound", message)
}

fn invalid_modification(message: String) -> CommandError {
    CommandError::new(
        INVALID_ROLE_MODIFICATION,
        "InvalidRoleModification",
        message,
    )
}

/// The database a role command works on; the empty name is refused as
/// mongod refuses it.
fn role_db(ctx: &CommandContext) -> Result<String, CommandError> {
    if ctx.db_name.is_empty() {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            "Invalid database name: ''",
        ));
    }
    Ok(ctx.db_name.clone())
}

/// The `privileges` argument, parsed; `[]` when it was not given.
fn privileges_arg(doc: &Document) -> Result<Vec<Privilege>, CommandError> {
    match umc::given(doc, "privileges").and_then(Bson::as_array) {
        Some(items) => privileges::parse_privileges(items),
        None => Ok(Vec::new()),
    }
}

/// The `roles` argument as `(role, db)` pairs; `[]` when it was not given.
fn roles_arg(doc: &Document, db: &str) -> Result<Vec<(String, String)>, CommandError> {
    match umc::given(doc, "roles").and_then(Bson::as_array) {
        Some(items) => umc::parse_role_names(items, db),
        None => Ok(Vec::new()),
    }
}

/// What mongod checks before it looks a granted role up: a role is not
/// granted to itself, and a role of a database other than `admin` holds
/// only that database's roles.
fn grantable_roles(name: &str, db: &str, roles: &[(String, String)]) -> Result<(), CommandError> {
    if roles.iter().any(|(r, d)| r == name && d == db) {
        return Err(invalid_modification(format!(
            "Cannot grant role {name}@{db} to itself."
        )));
    }
    if db != "admin" && roles.iter().any(|(_, d)| d != db) {
        return Err(invalid_modification(format!(
            "Roles on the '{db}' database cannot be granted roles from other databases"
        )));
    }
    Ok(())
}

/// Every granted role must exist, in the words `createRole` and its kin use.
fn granted_roles_exist(
    ctx: &CommandContext,
    name: &str,
    db: &str,
    roles: &[(String, String)],
) -> Result<(), CommandError> {
    for (role, role_db) in roles {
        if !umc::role_exists(ctx, role, role_db)? {
            return Err(role_not_found(format!(
                "Cannot grant roles to '{name}@{db}': Could not find role: {role}@{role_db}"
            )));
        }
    }
    Ok(())
}

/// Granting `roles` to `(name, db)` must not make the graph cyclic.
fn no_cycle(
    ctx: &CommandContext,
    name: &str,
    db: &str,
    roles: &[(String, String)],
) -> Result<(), CommandError> {
    let reached = umc::inherit(ctx, roles)?.roles;
    if reached.iter().any(|(r, d)| r == name && d == db) {
        return Err(invalid_modification(format!(
            "Granting roles to {name}@{db} would introduce a cycle in the role graph"
        )));
    }
    Ok(())
}

/// A role of a database other than `admin` holds only that database's
/// namespaces.
fn grantable_privileges(db: &str, privileges: &[Privilege]) -> Result<(), CommandError> {
    if db != "admin" && privileges.iter().any(|p| !p.resource.within(db)) {
        return Err(invalid_modification(format!(
            "Roles on the '{db}' database cannot be granted privileges that target other \
             databases or the cluster"
        )));
    }
    Ok(())
}

/// A built-in role is not modified: `read@db is a built-in role and cannot
/// be modified`.
fn not_builtin(name: &str, db: &str) -> Result<(), CommandError> {
    if privileges::builtin_exists(name, db) {
        return Err(bad_value(format!(
            "{name}@{db} is a built-in role and cannot be modified"
        )));
    }
    Ok(())
}

/// The stored record the grant / revoke commands modify, else mongod's
/// `Role x@db not found`.
fn stored_or_not_found(
    ctx: &CommandContext,
    name: &str,
    db: &str,
) -> Result<Document, CommandError> {
    umc::stored_role(ctx, db, name)?
        .ok_or_else(|| role_not_found(format!("Role {name}@{db} not found")))
}

fn role_docs(roles: &[(String, String)]) -> Vec<Bson> {
    roles.iter().map(umc::role_name_doc).collect()
}

const CREATE_ROLE_FIELDS: &[(&str, Field, bool)] = &[
    ("createRole", Field::Str, true),
    ("privileges", Field::Privileges, true),
    ("roles", Field::RoleNames, true),
    ("authenticationRestrictions", Field::Restrictions, false),
];

/// `createRole`, in mongod's order (8.2.11): the fields' types; a
/// non-empty name, not on `local`, not a built-in's; the privileges parsed
/// (resource shape, actions known); the roles grantable, then existing;
/// the privileges within the database; the restrictions; then the insert.
pub fn create_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(doc, "createRole", CREATE_ROLE_FIELDS)?;
    let db_name = role_db(ctx)?;
    let name = doc.get_str("createRole").unwrap_or("").to_string();
    if name.is_empty() {
        return Err(bad_value("Role name must be non-empty"));
    }
    if db_name == "local" {
        return Err(bad_value("Cannot create roles in the local database"));
    }
    if privileges::builtin_exists(&name, &db_name) {
        return Err(bad_value(
            "Cannot create roles with the same name as a built-in role",
        ));
    }
    let privileges = privileges_arg(doc)?;
    let roles = roles_arg(doc, &db_name)?;
    grantable_roles(&name, &db_name, &roles)?;
    granted_roles_exist(ctx, &name, &db_name, &roles)?;
    grantable_privileges(&db_name, &privileges)?;
    let restrictions = umc::given(doc, "authenticationRestrictions")
        .and_then(Bson::as_array)
        .cloned();
    if let Some(r) = &restrictions {
        umc::validate_restrictions(r)?;
    }
    let mut set = PrivilegeSet::default();
    set.extend(&privileges);
    let mut record = doc! {
        "_id": format!("{db_name}.{name}"),
        "role": &name,
        "db": &db_name,
        "privileges": set.to_bson(),
        "roles": role_docs(&roles),
    };
    if let Some(r) = restrictions {
        record.insert("authenticationRestrictions", r);
    }
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

const UPDATE_ROLE_FIELDS: &[(&str, Field, bool)] = &[
    ("updateRole", Field::Str, true),
    ("privileges", Field::Privileges, false),
    ("roles", Field::RoleNames, false),
    ("authenticationRestrictions", Field::Restrictions, false),
];

/// `updateRole` — replace a role's privileges, roles or restrictions
/// (8.2.11): something to update; the privileges parsed; the role known to
/// the graph (a built-in is, and fails only at the write: `Role read@db not
/// found`); the roles grantable, existing and acyclic; the privileges
/// within the database; the restrictions.
pub fn update_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(doc, "updateRole", UPDATE_ROLE_FIELDS)?;
    let db_name = role_db(ctx)?;
    let name = doc.get_str("updateRole").unwrap_or("").to_string();
    let given = |field: &str| umc::given(doc, field).is_some();
    if !given("privileges") && !given("roles") && !given("authenticationRestrictions") {
        return Err(bad_value(
            "Must specify at least one field to update in updateRole",
        ));
    }
    let privileges = privileges_arg(doc)?;
    if !umc::role_exists(ctx, &name, &db_name)? {
        return Err(role_not_found(format!(
            "Could not find role: {name}@{db_name}"
        )));
    }
    let roles = roles_arg(doc, &db_name)?;
    if given("roles") {
        grantable_roles(&name, &db_name, &roles)?;
        granted_roles_exist(ctx, &name, &db_name, &roles)?;
        no_cycle(ctx, &name, &db_name, &roles)?;
    }
    grantable_privileges(&db_name, &privileges)?;
    let restrictions = umc::given(doc, "authenticationRestrictions")
        .and_then(Bson::as_array)
        .cloned();
    if let Some(r) = &restrictions {
        umc::validate_restrictions(r)?;
    }
    let mut record = stored_or_not_found(ctx, &name, &db_name)?;
    if given("privileges") {
        let mut set = PrivilegeSet::default();
        set.extend(&privileges);
        record.insert("privileges", set.to_bson());
    }
    if given("roles") {
        record.insert("roles", role_docs(&roles));
    }
    if let Some(r) = restrictions {
        record.insert("authenticationRestrictions", r);
    }
    umc::save_role(ctx, &db_name, &name, &record)?;
    crate::auth::refresh_effective_roles(ctx);
    Ok(doc! { "ok": 1.0 })
}

/// `dropRole` — remove a custom role, and take it from every user and role
/// holding it, on every database (8.2.11).
pub fn drop_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(doc, "dropRole", &[("dropRole", Field::Str, true)])?;
    let db_name = role_db(ctx)?;
    let name = doc.get_str("dropRole").unwrap_or("").to_string();
    not_builtin(&name, &db_name)?;
    if !ctx
        .storage()?
        .drop_role(&db_name, &name)
        .map_err(command_error)?
    {
        return Err(role_not_found(format!(
            "Could not find role: {name}@{db_name}"
        )));
    }
    let dropped = (name.clone(), db_name.clone());
    crate::auth::remove_role_from_users(ctx, &dropped)?;
    let records = ctx
        .storage()?
        .list_roles(None, 0, 0)
        .map_err(command_error)?;
    for bytes in records {
        let Ok(mut record) = secantus_core::bsondoc::from_bytes(bytes.as_slice()) else {
            continue;
        };
        let (Ok(db), Ok(role)) = (
            record.get_str("db").map(String::from),
            record.get_str("role").map(String::from),
        ) else {
            continue;
        };
        let held = umc::role_pairs(record.get_array("roles").map(Vec::as_slice).unwrap_or(&[]));
        if held.contains(&dropped) {
            let kept: Vec<(String, String)> = held.into_iter().filter(|p| *p != dropped).collect();
            record.insert("roles", role_docs(&kept));
            umc::save_role(ctx, &db, &role, &record)?;
        }
    }
    crate::auth::refresh_effective_roles(ctx);
    Ok(doc! { "ok": 1.0 })
}

/// `grantPrivilegesToRole` — merge privileges into a custom role, by
/// resource (8.2.11): a non-empty `privileges`, parsed; not a built-in;
/// within the database; the role stored.
pub fn grant_privileges_to_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    change_privileges(doc, ctx, "grantPrivilegesToRole", true)
}

/// `revokePrivilegesFromRole` — take actions from a role's privileges on
/// the same resources; a privilege left with none goes.
pub fn revoke_privileges_from_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    change_privileges(doc, ctx, "revokePrivilegesFromRole", false)
}

fn change_privileges(
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
            ("privileges", Field::Privileges, true),
        ],
    )?;
    let db_name = role_db(ctx)?;
    let name = doc.get_str(command).unwrap_or("").to_string();
    if doc.get_array("privileges").is_ok_and(|p| p.is_empty()) {
        return Err(bad_value(format!(
            "{command} command requires a non-empty \"privileges\" array"
        )));
    }
    let changes = privileges_arg(doc)?;
    not_builtin(&name, &db_name)?;
    if grant {
        grantable_privileges(&db_name, &changes)?;
    }
    let mut record = stored_or_not_found(ctx, &name, &db_name)?;
    let mut set = PrivilegeSet::from_docs(
        record
            .get_array("privileges")
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    );
    for change in &changes {
        if grant {
            set.add(change);
        } else {
            set.remove(change);
        }
    }
    record.insert("privileges", set.to_bson());
    umc::save_role(ctx, &db_name, &name, &record)?;
    Ok(doc! { "ok": 1.0 })
}

/// `grantRolesToRole` — add roles to a custom role (8.2.11): a non-empty
/// `roles`; grantable (not itself, not another database's); not a
/// built-in; the roles existing; the role stored; no cycle.
pub fn grant_roles_to_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    change_roles(doc, ctx, "grantRolesToRole", true)
}

/// `revokeRolesFromRole` — take roles from a custom role; a role it never
/// held, or that does not exist, is no error.
pub fn revoke_roles_from_role(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    change_roles(doc, ctx, "revokeRolesFromRole", false)
}

fn change_roles(
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
    let db_name = role_db(ctx)?;
    let name = doc.get_str(command).unwrap_or("").to_string();
    if doc.get_array("roles").is_ok_and(|r| r.is_empty()) {
        return Err(bad_value(format!(
            "{command} command requires a non-empty \"roles\" array"
        )));
    }
    let changes = roles_arg(doc, &db_name)?;
    if grant {
        grantable_roles(&name, &db_name, &changes)?;
    }
    not_builtin(&name, &db_name)?;
    if grant {
        granted_roles_exist(ctx, &name, &db_name, &changes)?;
    }
    let mut record = stored_or_not_found(ctx, &name, &db_name)?;
    let mut held = umc::role_pairs(record.get_array("roles").map(Vec::as_slice).unwrap_or(&[]));
    if grant {
        no_cycle(ctx, &name, &db_name, &changes)?;
        for pair in changes {
            if !held.contains(&pair) {
                held.push(pair);
            }
        }
    } else {
        held.retain(|p| !changes.contains(p));
    }
    record.insert("roles", role_docs(&held));
    umc::save_role(ctx, &db_name, &name, &record)?;
    crate::auth::refresh_effective_roles(ctx);
    Ok(doc! { "ok": 1.0 })
}

/// `dropAllRolesFromDatabase` — drop every custom role of the calling db,
/// answering `{n, ok}`. Nothing is taken from the users or roles that held
/// them (8.2.11).
pub fn drop_all_roles_from_database(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(
        doc,
        "dropAllRolesFromDatabase",
        &[("dropAllRolesFromDatabase", Field::Any, false)],
    )?;
    let db_name = role_db(ctx)?;
    let storage = ctx.storage()?;
    let mut removed = 0;
    loop {
        let batch = storage
            .list_roles(Some(&db_name), 0, 1000)
            .map_err(command_error)?;
        let n = batch.len();
        for bytes in &batch {
            if let Ok(rec) = secantus_core::bsondoc::from_bytes(bytes.as_slice()) {
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
    Ok(doc! { "n": removed, "ok": 1.0 })
}

/// What `showPrivileges` asks `rolesInfo` for.
enum ShowPrivileges {
    No,
    Yes,
    UserFragment,
}

const ROLES_INFO_FIELDS: &[(&str, Field, bool)] = &[
    ("rolesInfo", Field::Any, false),
    ("showPrivileges", Field::Any, false),
    ("showBuiltinRoles", Field::SafeBool, false),
    ("showAuthenticationRestrictions", Field::SafeBool, false),
];

/// `rolesInfo` (8.2.11). `1` lists the database's custom roles (its
/// built-ins first under `showBuiltinRoles`); a name, a `{role, db}` or a
/// list of those answers each that exists, built-in or stored, in the
/// order named; `showPrivileges: "asUserFragment"` answers what a user
/// holding those roles would inherit. The field order of a listed role,
/// a named role and a built-in role each differ, as mongod's do.
pub fn roles_info(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    umc::check_fields(doc, "rolesInfo", ROLES_INFO_FIELDS)?;
    crate::serverinfo::principal_argument(doc.get("rolesInfo"), "RoleName", "role", false)?;
    let show_privileges = match doc.get("showPrivileges") {
        None => ShowPrivileges::No,
        Some(Bson::String(word)) if word == "asUserFragment" => ShowPrivileges::UserFragment,
        Some(v) if crate::util::truthy_number_or_bool(v).is_some() => {
            if crate::util::truthy_number_or_bool(v).unwrap_or(false) {
                ShowPrivileges::Yes
            } else {
                ShowPrivileges::No
            }
        }
        Some(other) => {
            return Err(bad_value(format!(
                "Failed to parse 'showPrivileges'. 'showPrivileges' should either be a boolean \
                 or the string 'asUserFragment', given: showPrivileges: {}",
                secantus_core::query::bson_value_repr(other)
            )))
        }
    };
    let flag = |name: &str| {
        umc::given(doc, name)
            .and_then(crate::util::truthy_number_or_bool)
            .unwrap_or(false)
    };
    let show_builtin = flag("showBuiltinRoles");
    let show_restrictions = flag("showAuthenticationRestrictions");
    let db_name = role_db(ctx)?;
    let privileges_shown = matches!(show_privileges, ShowPrivileges::Yes);

    let listing = matches!(
        doc.get("rolesInfo"),
        Some(Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))
    );
    if listing {
        if matches!(show_privileges, ShowPrivileges::UserFragment) {
            return Err(CommandError::new(
                20,
                "IllegalOperation",
                "Cannot get user fragment for all roles in a database",
            ));
        }
        let mut roles: Vec<Bson> = Vec::new();
        if show_builtin {
            for name in privileges::builtin_names(&db_name) {
                roles.push(Bson::Document(umc::builtin_role_doc(
                    name,
                    &db_name,
                    true,
                    privileges_shown,
                    show_restrictions,
                )));
            }
        }
        let stored = ctx
            .storage()?
            .list_roles(Some(&db_name), 0, 0)
            .map_err(command_error)?;
        for bytes in stored {
            let record = secantus_core::bsondoc::from_bytes(bytes.as_slice()).map_err(|e| {
                CommandError::new(1, "InternalError", format!("decode role record: {e}"))
            })?;
            roles.push(Bson::Document(umc::role_info_doc(
                ctx,
                &record,
                true,
                privileges_shown,
                show_restrictions,
            )?));
        }
        return Ok(doc! { "roles": roles, "ok": 1.0 });
    }

    let items: Vec<Bson> = match doc.get("rolesInfo") {
        Some(Bson::Array(items)) => items.clone(),
        Some(other) => vec![other.clone()],
        None => Vec::new(),
    };
    let names = umc::parse_role_names(&items, &db_name)?;
    if matches!(show_privileges, ShowPrivileges::UserFragment) {
        let fragment = umc::user_fragment(ctx, &names, show_restrictions)?;
        return Ok(doc! { "userFragment": fragment, "ok": 1.0 });
    }
    let mut roles: Vec<Bson> = Vec::new();
    for (name, db) in &names {
        if privileges::builtin_exists(name, db) {
            roles.push(Bson::Document(umc::builtin_role_doc(
                name,
                db,
                false,
                privileges_shown,
                show_restrictions,
            )));
        } else if let Some(record) = umc::stored_role(ctx, db, name)? {
            roles.push(Bson::Document(umc::role_info_doc(
                ctx,
                &record,
                false,
                privileges_shown,
                show_restrictions,
            )?));
        }
    }
    Ok(doc! { "roles": roles, "ok": 1.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use crate::rbac;
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
        let info = dispatch(&doc! {"rolesInfo": "r", "showPrivileges": true}, &mut c);
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
            secantus_core::bsondoc::from_bytes(bytes.as_slice()).ok()
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
        let info = dispatch(&doc! {"rolesInfo": name, "showPrivileges": true}, c);
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

    /// mongod 8.2.11: an empty `roles` is refused before anything is looked
    /// up; a role that does not exist is `Role ghost@admin not found`.
    #[test]
    fn grant_to_missing_role_is_role_not_found() {
        let mut c = ctx();
        let empty = dispatch(&doc! {"grantRolesToRole": "ghost", "roles": []}, &mut c);
        assert_eq!(empty.get_i32("code").unwrap(), 2);
        assert_eq!(
            empty.get_str("errmsg").unwrap(),
            "grantRolesToRole command requires a non-empty \"roles\" array"
        );
        dispatch(
            &doc! {"createRole": "base", "privileges": [], "roles": []},
            &mut c,
        );
        let reply = dispatch(
            &doc! {"grantRolesToRole": "ghost", "roles": ["base"]},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), ROLE_NOT_FOUND);
        assert_eq!(
            reply.get_str("errmsg").unwrap(),
            "Role ghost@admin not found"
        );
    }
}
