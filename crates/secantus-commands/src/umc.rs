//! What the user and role management commands share: the parse of their
//! arguments as mongod's IDL parser does it, role names, authentication
//! restrictions, the role graph (what a user or role inherits), and the
//! shapes `usersInfo` and `rolesInfo` answer in. Every message and key
//! order was measured on mongod 8.2.11 with
//! `tools/probes/user_role_commands.py` (2026-10-13).
//!
//! Two things about the stored records. A user record is kept in the order
//! mongod's own `system.users` document has (`_id`, `userId`, `user`, `db`,
//! `credentials`, then `authenticationRestrictions`, `customData`, `roles`
//! as given), because `usersInfo` lists the record's fields in stored order
//! and a field `updateUser` adds goes LAST, after `roles`. `mechanisms` is
//! not a field of mongod's record at all -- it is the credential names,
//! computed on the way out -- but this server has always stored it after
//! the rest, and keeps doing so for the other servers that read the table;
//! it is dropped and recomputed on output. A role record holds its
//! `authenticationRestrictions` as given; `rolesInfo` wraps them once more,
//! as mongod does.

use std::net::IpAddr;

use bson::{doc, Bson, Document};
use secantus_core::query::bson_type_name;

use crate::privileges::{self, Privilege, PrivilegeSet};
use crate::{CommandContext, CommandError};

/// What a command's field holds.
#[derive(Clone, Copy)]
pub(crate) enum Field {
    /// Anything: `dropAllUsersFromDatabase: 1` takes any value.
    Any,
    Str,
    Obj,
    /// mongod's `safeBool`: a bool or a number.
    SafeBool,
    /// An array of strings (`mechanisms`).
    StrArr,
    /// An array of role names: strings or `{role, db}` documents.
    RoleNames,
    /// An array of `{resource, actions}` documents.
    Privileges,
    /// An array of `{clientSource?, serverAddress?}` documents.
    Restrictions,
}

/// The commands this module serves: a database name they refuse is worded
/// as a database (`Invalid database name: ''`), not as a namespace.
pub(crate) fn is_user_or_role_command(name: &str) -> bool {
    matches!(
        name,
        "createUser"
            | "updateUser"
            | "dropUser"
            | "dropAllUsersFromDatabase"
            | "grantRolesToUser"
            | "revokeRolesFromUser"
            | "usersInfo"
            | "createRole"
            | "updateRole"
            | "dropRole"
            | "dropAllRolesFromDatabase"
            | "grantPrivilegesToRole"
            | "revokePrivilegesFromRole"
            | "grantRolesToRole"
            | "revokeRolesFromRole"
            | "rolesInfo"
    )
}

pub(crate) fn bad_value(message: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", message)
}

pub(crate) fn wrong_type(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected {expected}",
            bson_type_name(value)
        ),
    )
}

fn missing(path: &str) -> CommandError {
    CommandError::new(
        40414,
        "IDLFailedToParse",
        format!("BSON field '{path}' is missing but a required field"),
    )
}

fn unknown(path: &str) -> CommandError {
    CommandError::new(
        40415,
        "IDLUnknownField",
        format!("BSON field '{path}' is an unknown field."),
    )
}

const SAFE_BOOL: &str = "types '[bool, double, decimal, long, int]'";

fn is_safe_bool(value: &Bson) -> bool {
    matches!(
        value,
        Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

/// The restrictions array's own type checks: each entry an object whose
/// `clientSource` / `serverAddress` are arrays of strings, no other field.
fn check_restrictions_shape(path: &str, entries: &[Bson]) -> Result<(), CommandError> {
    for (i, entry) in entries.iter().enumerate() {
        let Bson::Document(entry) = entry else {
            return Err(wrong_type(&format!("{path}.{i}"), entry, "type 'object'"));
        };
        for (key, value) in entry {
            match (key.as_str(), value) {
                (_, Bson::Null) => {}
                ("clientSource" | "serverAddress", Bson::Array(items)) => {
                    for (j, item) in items.iter().enumerate() {
                        if !matches!(item, Bson::String(_)) {
                            return Err(wrong_type(
                                &format!("{path}.{key}.{j}"),
                                item,
                                "type 'string'",
                            ));
                        }
                    }
                }
                ("clientSource" | "serverAddress", v) => {
                    return Err(wrong_type(&format!("{path}.{key}"), v, "type 'array'"))
                }
                (other, _) => return Err(unknown(&format!("{path}.{other}"))),
            }
        }
    }
    Ok(())
}

/// Check a command's fields as mongod's parser does: in the order they were
/// sent, the first wrong type or unknown field is the error; a null field
/// is absent; a required field absent after the walk is 40414, in the order
/// of `fields`. The command's own key is a field like the others
/// (`createUser.createUser`). The generic fields every command takes pass.
pub(crate) fn check_fields(
    doc: &Document,
    command: &str,
    fields: &[(&str, Field, bool)],
) -> Result<(), CommandError> {
    for (key, value) in doc {
        let path = format!("{command}.{key}");
        let Some((_, kind, _)) = fields.iter().find(|(name, _, _)| name == key) else {
            if crate::generic::GENERIC.contains(&key.as_str()) {
                continue;
            }
            return Err(unknown(&path));
        };
        if matches!(value, Bson::Null) {
            continue;
        }
        match (kind, value) {
            (Field::Any, _) => {}
            (Field::Str, Bson::String(_)) => {}
            (Field::Str, v) => return Err(wrong_type(&path, v, "type 'string'")),
            (Field::Obj, Bson::Document(_)) => {}
            (Field::Obj, v) => return Err(wrong_type(&path, v, "type 'object'")),
            (Field::SafeBool, v) if is_safe_bool(v) => {}
            (Field::SafeBool, v) => return Err(wrong_type(&path, v, SAFE_BOOL)),
            (Field::StrArr, Bson::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    if !matches!(item, Bson::String(_)) {
                        return Err(wrong_type(&format!("{path}.{i}"), item, "type 'string'"));
                    }
                }
            }
            (Field::RoleNames, Bson::Array(items)) => {
                parse_role_names(items, "")?;
            }
            (Field::Privileges, Bson::Array(items)) => {
                privileges::check_privileges_shape(items, command)?;
            }
            (Field::Restrictions, Bson::Array(items)) => {
                check_restrictions_shape(&path, items)?;
            }
            (Field::StrArr | Field::RoleNames | Field::Privileges | Field::Restrictions, v) => {
                return Err(wrong_type(&path, v, "type 'array'"))
            }
        }
    }
    for (name, _, required) in fields {
        if *required && !doc.get(name).is_some_and(|v| !matches!(v, Bson::Null)) {
            return Err(missing(&format!("{command}.{name}")));
        }
    }
    Ok(())
}

/// A field that is present and not null.
pub(crate) fn given<'a>(doc: &'a Document, field: &str) -> Option<&'a Bson> {
    doc.get(field).filter(|v| !matches!(v, Bson::Null))
}

/// The role names of an array: a string is a role of `default_db`, a
/// document names its own. Checked as mongod's `RoleName` parser checks
/// them; a document's other fields are ignored, and the names are
/// deduplicated in order.
pub(crate) fn parse_role_names(
    items: &[Bson],
    default_db: &str,
) -> Result<Vec<(String, String)>, CommandError> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(items.len());
    for item in items {
        let pair = match item {
            Bson::String(role) => (role.clone(), default_db.to_string()),
            Bson::Document(name) => {
                let mut parts = Vec::with_capacity(2);
                for field in ["role", "db"] {
                    match name.get(field) {
                        Some(Bson::String(s)) => parts.push(s.clone()),
                        None => {
                            return Err(bad_value(format!(
                                "RoleName must contain a field named: {field}"
                            )))
                        }
                        Some(other) => {
                            return Err(bad_value(format!(
                                "RoleName must contain a string field named: {field}. But, has \
                                 type {}",
                                bson_type_name(other)
                            )))
                        }
                    }
                }
                (parts.remove(0), parts.remove(0))
            }
            _ => return Err(bad_value("Role name must be either a document or string")),
        };
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    Ok(out)
}

pub(crate) fn role_name_doc((role, db): &(String, String)) -> Bson {
    Bson::Document(doc! { "role": role, "db": db })
}

/// The `(role, db)` pairs of a stored `roles` array.
pub(crate) fn role_pairs(roles: &[Bson]) -> Vec<(String, String)> {
    roles
        .iter()
        .filter_map(|r| {
            let d = r.as_document()?;
            Some((
                d.get_str("role").ok()?.to_string(),
                d.get_str("db").ok()?.to_string(),
            ))
        })
        .collect()
}

/// A CIDR string as mongod's `CIDR` parses it: an address with an optional
/// prefix length. `None` when it is not one.
fn parse_cidr(text: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = match text.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (text, None),
    };
    let address: IpAddr = address.parse().ok()?;
    let width = if address.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(p) => p.parse::<u8>().ok().filter(|p| *p <= width)?,
        None => width,
    };
    Some((address, prefix))
}

/// The meaning of an `authenticationRestrictions` array, checked after its
/// shape: each entry needs a source or an address, and every one must be a
/// CIDR.
pub(crate) fn validate_restrictions(entries: &[Bson]) -> Result<(), CommandError> {
    for entry in entries.iter().filter_map(Bson::as_document) {
        let lists = ["clientSource", "serverAddress"]
            .iter()
            .filter_map(|k| entry.get_array(k).ok())
            .collect::<Vec<_>>();
        if lists.is_empty() {
            return Err(CommandError::new(
                162,
                "CollectionIsEmpty",
                "At least one of 'clientSource' or 'serverAddress' must be set",
            ));
        }
        for text in lists.iter().flat_map(|l| l.iter()).filter_map(Bson::as_str) {
            if parse_cidr(text).is_none() {
                return Err(bad_value("Invalid IP address in CIDR string"));
            }
        }
    }
    Ok(())
}

/// A restriction set as mongod renders one it INHERITED: an address with
/// no prefix length gets its full one (`127.0.0.1` is `127.0.0.1/32`), a
/// prefix given stays as given. A principal's own set is shown as stored.
pub(crate) fn normalize_restrictions(entries: &[Bson]) -> Vec<Bson> {
    entries
        .iter()
        .filter_map(Bson::as_document)
        .map(|entry| {
            let mut out = Document::new();
            for (key, value) in entry {
                let Bson::Array(items) = value else { continue };
                let items: Vec<Bson> = items
                    .iter()
                    .filter_map(Bson::as_str)
                    .map(|text| {
                        Bson::String(match (text.contains('/'), parse_cidr(text)) {
                            (false, Some((_, prefix))) => format!("{text}/{prefix}"),
                            _ => text.to_string(),
                        })
                    })
                    .collect();
                out.insert(key, items);
            }
            Bson::Document(out)
        })
        .collect()
}

/// A role as the graph sees it: its privileges, the roles it holds and its
/// restrictions; built-in roles hold no roles and no restrictions.
pub(crate) struct RoleRecord {
    pub(crate) privileges: Vec<Privilege>,
    pub(crate) roles: Vec<(String, String)>,
    pub(crate) restrictions: Vec<Bson>,
}

/// Read + decode a stored role record for `(db, name)`.
pub(crate) fn stored_role(
    ctx: &CommandContext,
    db: &str,
    name: &str,
) -> Result<Option<Document>, CommandError> {
    let Some(bytes) = ctx
        .storage()?
        .get_role(db, name)
        .map_err(crate::util::command_error)?
    else {
        return Ok(None);
    };
    secantus_core::bsondoc::from_bytes(bytes.as_slice())
        .map(Some)
        .map_err(|e| CommandError::new(1, "InternalError", format!("decode role record: {e}")))
}

/// Encode + persist a role record, replacing any there.
pub(crate) fn save_role(
    ctx: &CommandContext,
    db: &str,
    name: &str,
    record: &Document,
) -> Result<(), CommandError> {
    let mut bytes = Vec::new();
    crate::bsondoc::write(record, &mut bytes)
        .map_err(|e| CommandError::new(1, "InternalError", format!("encode role record: {e}")))?;
    ctx.storage()?
        .add_role(db, name, &bytes, true)
        .map_err(crate::util::command_error)?;
    Ok(())
}

pub(crate) fn record_from_doc(record: &Document) -> RoleRecord {
    RoleRecord {
        privileges: record
            .get_array("privileges")
            .map(|a| {
                a.iter()
                    .filter_map(Bson::as_document)
                    .filter_map(Privilege::from_doc)
                    .collect()
            })
            .unwrap_or_default(),
        roles: record
            .get_array("roles")
            .map(|a| role_pairs(a))
            .unwrap_or_default(),
        restrictions: record
            .get_array("authenticationRestrictions")
            .cloned()
            .unwrap_or_default(),
    }
}

/// The role `(name, db)` resolves to: a built-in of that database, else a
/// stored one, else nothing.
pub(crate) fn load_role(
    ctx: &CommandContext,
    name: &str,
    db: &str,
) -> Result<Option<RoleRecord>, CommandError> {
    if let Some(privileges) = privileges::builtin_privileges(name, db) {
        return Ok(Some(RoleRecord {
            privileges,
            roles: Vec::new(),
            restrictions: Vec::new(),
        }));
    }
    Ok(stored_role(ctx, db, name)?.as_ref().map(record_from_doc))
}

pub(crate) fn role_exists(
    ctx: &CommandContext,
    name: &str,
    db: &str,
) -> Result<bool, CommandError> {
    Ok(privileges::builtin_exists(name, db)
        || ctx
            .storage()?
            .get_role(db, name)
            .map_err(crate::util::command_error)?
            .is_some())
}

/// What a set of roles inherits, taken together.
#[derive(Default)]
pub(crate) struct Inherited {
    /// Every role reached, the starting ones first, a name that resolves to
    /// nothing included (mongod lists a dropped role a user still names).
    pub(crate) roles: Vec<(String, String)>,
    pub(crate) privileges: PrivilegeSet,
    /// One normalised set per reached role that has restrictions.
    pub(crate) restrictions: Vec<Bson>,
}

/// Walk the role graph from `start`.
pub(crate) fn inherit(
    ctx: &CommandContext,
    start: &[(String, String)],
) -> Result<Inherited, CommandError> {
    let mut out = Inherited::default();
    let mut pending: Vec<(String, String)> = start.to_vec();
    pending.reverse();
    while let Some(pair) = pending.pop() {
        if out.roles.contains(&pair) {
            continue;
        }
        out.roles.push(pair.clone());
        let Some(record) = load_role(ctx, &pair.0, &pair.1)? else {
            continue;
        };
        out.privileges.extend(&record.privileges);
        if !record.restrictions.is_empty() {
            out.restrictions
                .push(Bson::Array(normalize_restrictions(&record.restrictions)));
        }
        for held in record.roles.iter().rev() {
            if !out.roles.contains(held) {
                pending.push(held.clone());
            }
        }
    }
    Ok(out)
}

/// The names of the roles in `pairs`, as `rolesInfo` lists them.
fn role_docs(pairs: &[(String, String)]) -> Vec<Bson> {
    pairs.iter().map(role_name_doc).collect()
}

/// The `rolesInfo` document of a stored role. `listing` is the
/// `rolesInfo: 1` form, whose `isBuiltin` comes before the inherited
/// fields; a named lookup puts it last.
pub(crate) fn role_info_doc(
    ctx: &CommandContext,
    record: &Document,
    listing: bool,
    show_privileges: bool,
    show_restrictions: bool,
) -> Result<Document, CommandError> {
    let role = record_from_doc(record);
    let inherited = inherit(ctx, &role.roles)?;
    let mut own = PrivilegeSet::default();
    own.extend(&role.privileges);
    // mongod lists the role's own privileges as they are, then what the
    // roles it holds give, merged among themselves but not with its own: a
    // resource both name appears twice (8.2.11). A user's are all merged.
    let mut all = own.to_bson();
    all.extend(inherited.privileges.to_bson());
    let mut restrictions = Vec::new();
    if !role.restrictions.is_empty() {
        restrictions.push(Bson::Array(role.restrictions.clone()));
    }
    restrictions.extend(inherited.restrictions);

    let mut out = Document::new();
    for key in ["_id", "role", "db"] {
        if let Some(v) = record.get(key) {
            out.insert(key, v.clone());
        }
    }
    if show_privileges {
        out.insert("privileges", own.to_bson());
    }
    out.insert("roles", role_docs(&role.roles));
    if show_restrictions {
        out.insert("authenticationRestrictions", restrictions.clone());
    }
    if listing {
        out.insert("isBuiltin", false);
    }
    out.insert("inheritedRoles", role_docs(&inherited.roles));
    if show_privileges {
        out.insert("inheritedPrivileges", all);
    }
    if show_restrictions {
        out.insert("inheritedAuthenticationRestrictions", restrictions);
    }
    if !listing {
        out.insert("isBuiltin", false);
    }
    Ok(out)
}

/// The `rolesInfo` document of a built-in role: no `_id`, and a named
/// lookup puts `db` before `role`.
pub(crate) fn builtin_role_doc(
    role: &str,
    db: &str,
    listing: bool,
    show_privileges: bool,
    show_restrictions: bool,
) -> Document {
    let privileges: Vec<Bson> = privileges::builtin_privileges(role, db)
        .unwrap_or_default()
        .iter()
        .map(|p| Bson::Document(p.to_bson()))
        .collect();
    let empty: Vec<Bson> = Vec::new();
    let mut out = Document::new();
    if listing {
        out.insert("role", role);
        out.insert("db", db);
        out.insert("isBuiltin", true);
        out.insert("roles", empty.clone());
        out.insert("inheritedRoles", empty.clone());
        if show_privileges {
            out.insert("privileges", privileges.clone());
            out.insert("inheritedPrivileges", privileges);
        }
        if show_restrictions {
            out.insert("authenticationRestrictions", empty.clone());
            out.insert("inheritedAuthenticationRestrictions", empty);
        }
    } else {
        out.insert("db", db);
        out.insert("role", role);
        out.insert("roles", empty.clone());
        if show_privileges {
            out.insert("privileges", privileges.clone());
        }
        if show_restrictions {
            out.insert("authenticationRestrictions", empty.clone());
        }
        out.insert("inheritedRoles", empty.clone());
        if show_privileges {
            out.insert("inheritedPrivileges", privileges);
        }
        if show_restrictions {
            out.insert("inheritedAuthenticationRestrictions", empty);
        }
        out.insert("isBuiltin", true);
    }
    out
}

/// `rolesInfo` with `showPrivileges: "asUserFragment"`: what a user holding
/// exactly `roles` would inherit, whether or not each role exists.
pub(crate) fn user_fragment(
    ctx: &CommandContext,
    roles: &[(String, String)],
    show_restrictions: bool,
) -> Result<Document, CommandError> {
    let inherited = inherit(ctx, roles)?;
    let mut out = doc! {
        "roles": role_docs(roles),
        "inheritedRoles": role_docs(&inherited.roles),
        "inheritedPrivileges": inherited.privileges.to_bson(),
    };
    if show_restrictions {
        out.insert(
            "inheritedAuthenticationRestrictions",
            inherited.restrictions,
        );
    }
    Ok(out)
}

/// The mechanisms a user record's credentials name, in the record's order:
/// `SCRAM-SHA-1`, `SCRAM-SHA-256`, or `external` for a `$external` user.
pub(crate) fn mechanisms_of(record: &Document) -> Vec<Bson> {
    record
        .get_document("credentials")
        .map(|c| c.keys().map(|k| Bson::String(k.clone())).collect())
        .unwrap_or_default()
}

/// A user as `usersInfo` lists it without privilege details: the stored
/// fields in stored order, less the credentials unless asked for, the
/// restrictions always and the custom data when asked, and `mechanisms`
/// last.
pub(crate) fn user_summary(
    record: &Document,
    show_credentials: bool,
    show_custom_data: bool,
) -> Document {
    let mut out = Document::new();
    for (key, value) in record {
        match key.as_str() {
            "credentials" if !show_credentials => {}
            "customData" if !show_custom_data => {}
            "authenticationRestrictions" | "mechanisms" => {}
            _ => {
                out.insert(key, value.clone());
            }
        }
    }
    out.insert("mechanisms", mechanisms_of(record));
    out
}

/// A user as `usersInfo` describes it with `showPrivileges` or
/// `showAuthenticationRestrictions`: mongod's user description, where the
/// mechanisms stand at the credentials' place and the inherited fields
/// follow.
pub(crate) fn user_detail(
    ctx: &CommandContext,
    record: &Document,
    show_credentials: bool,
    show_custom_data: bool,
    show_restrictions: bool,
) -> Result<Document, CommandError> {
    let roles = record
        .get_array("roles")
        .map(|a| role_pairs(a))
        .unwrap_or_default();
    let inherited = inherit(ctx, &roles)?;
    let mut out = Document::new();
    let mut restrictions_shown = false;
    for (key, value) in record {
        match key.as_str() {
            "credentials" => {
                out.insert("mechanisms", mechanisms_of(record));
                if show_credentials {
                    out.insert("credentials", value.clone());
                }
            }
            "authenticationRestrictions" => {
                if show_restrictions {
                    out.insert(key, value.clone());
                    restrictions_shown = true;
                }
            }
            "customData" if !show_custom_data => {}
            "mechanisms" => {}
            _ => {
                out.insert(key, value.clone());
            }
        }
    }
    if show_restrictions && !restrictions_shown {
        out.insert("authenticationRestrictions", Vec::<Bson>::new());
    }
    out.insert("inheritedRoles", role_docs(&inherited.roles));
    out.insert("inheritedPrivileges", inherited.privileges.to_bson());
    out.insert(
        "inheritedAuthenticationRestrictions",
        inherited.restrictions,
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_checked_in_the_order_sent() {
        let spec = [
            ("createUser", Field::Str, true),
            ("pwd", Field::Str, false),
            ("customData", Field::Obj, false),
            ("roles", Field::RoleNames, true),
        ];
        let errmsg = |doc: Document| check_fields(&doc, "createUser", &spec).unwrap_err().errmsg;
        assert_eq!(
            errmsg(doc! {"createUser": "u", "pwd": "pw", "roles": [], "bogus": 1, "customData": 1}),
            "BSON field 'createUser.bogus' is an unknown field."
        );
        assert_eq!(
            errmsg(doc! {"createUser": "u", "customData": 1, "bogus": 1, "pwd": "pw", "roles": []}),
            "BSON field 'createUser.customData' is the wrong type 'int', expected type 'object'"
        );
        assert_eq!(
            errmsg(doc! {"createUser": "u", "pwd": "pw"}),
            "BSON field 'createUser.roles' is missing but a required field"
        );
        assert_eq!(
            errmsg(doc! {"createUser": Bson::Null, "pwd": "pw", "roles": []}),
            "BSON field 'createUser.createUser' is missing but a required field"
        );
        assert_eq!(
            errmsg(doc! {"createUser": "u", "pwd": "pw", "roles": [1]}),
            "Role name must be either a document or string"
        );
        assert!(check_fields(
            &doc! {"createUser": "u", "pwd": "pw", "roles": [], "writeConcern": {"w": 1}, "$db": "t"},
            "createUser",
            &spec
        )
        .is_ok());
    }

    #[test]
    fn role_names_need_role_then_db() {
        let errmsg = |items: Vec<Bson>| parse_role_names(&items, "t").unwrap_err().errmsg;
        assert_eq!(
            errmsg(vec![Bson::Document(doc! {"role": "read"})]),
            "RoleName must contain a field named: db"
        );
        assert_eq!(
            errmsg(vec![Bson::Document(doc! {"db": "t"})]),
            "RoleName must contain a field named: role"
        );
        assert_eq!(
            errmsg(vec![Bson::Document(doc! {"role": 1, "db": "t"})]),
            "RoleName must contain a string field named: role. But, has type int"
        );
        let names = parse_role_names(
            &[
                Bson::String("read".into()),
                Bson::Document(doc! {"role": "read", "db": "t", "bogus": 1}),
                Bson::Document(doc! {"role": "x", "db": "other"}),
            ],
            "t",
        )
        .unwrap();
        assert_eq!(
            names,
            [
                ("read".to_string(), "t".to_string()),
                ("x".to_string(), "other".to_string())
            ]
        );
    }

    #[test]
    fn restrictions_are_cidrs_and_inherited_ones_carry_a_prefix() {
        assert!(
            validate_restrictions(&[Bson::Document(doc! {"clientSource": ["127.0.0.1"]})]).is_ok()
        );
        assert_eq!(
            validate_restrictions(&[Bson::Document(doc! {"clientSource": ["bogus"]})])
                .unwrap_err()
                .errmsg,
            "Invalid IP address in CIDR string"
        );
        assert_eq!(
            validate_restrictions(&[Bson::Document(doc! {})])
                .unwrap_err()
                .code,
            162
        );
        assert_eq!(
            normalize_restrictions(&[Bson::Document(
                doc! {"clientSource": ["127.0.0.1", "10.0.0.0/8"], "serverAddress": ["::1"]}
            )]),
            vec![Bson::Document(
                doc! {"clientSource": ["127.0.0.1/32", "10.0.0.0/8"], "serverAddress": ["::1/128"]}
            )]
        );
    }
}
