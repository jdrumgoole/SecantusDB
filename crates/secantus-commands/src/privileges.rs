//! Privileges as mongod stores, merges and lists them, and the built-in roles
//! it has. Everything here was measured on mongod 8.2.11 with
//! `tools/probes/user_role_commands.py` (2026-10-13).
//!
//! A privilege is a resource and a set of actions. mongod keeps the set as a
//! bitset, so the actions a role lists come back deduplicated and in the
//! order of its `ActionType` enum ([`builtin_roles::ACTIONS`]), whatever
//! order they were given in; and two privileges on one resource are one
//! privilege, which is what makes `grantPrivilegesToRole` of `insert` on a
//! collection join the `find` already there.
//!
//! The built-in roles are a generated table ([`builtin_roles`]): which roles
//! every database has, which only `admin` has, and the privileges of each.
//! `root` is a role of `admin` and of no other database, so `createUser` with
//! `roles: ["root"]` on another database is `RoleNotFound`.

use bson::{doc, Bson, Document};
use secantus_core::query::bson_type_name;

use crate::builtin_roles::{ACTIONS, ADMIN_ROLES, DATABASE_ROLES};
use crate::CommandError;

/// A generated table of built-in roles: each name with its privileges.
pub(crate) type RoleTable = &'static [(&'static str, &'static [(Res, &'static [&'static str])])];

/// A resource of the generated table. `@db` in a name stands for the
/// database the role is read on.
pub(crate) enum Res {
    Ns(&'static str, &'static str),
    Cluster,
    Any,
    SystemBuckets(&'static str, &'static str),
}

/// The resource of a privilege. `Ns` with an empty `db` is every database
/// and with an empty `collection` every collection of the database;
/// `SystemBuckets` with an empty `db` carries no `db` field at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resource {
    Ns { db: String, collection: String },
    Cluster,
    Any,
    SystemBuckets { db: String, bucket: String },
}

impl Resource {
    fn from_table(res: &Res, db: &str) -> Resource {
        let fill = |name: &str| {
            if name == "@db" {
                db.to_string()
            } else {
                name.to_string()
            }
        };
        match res {
            Res::Ns(d, c) => Resource::Ns {
                db: fill(d),
                collection: fill(c),
            },
            Res::Cluster => Resource::Cluster,
            Res::Any => Resource::Any,
            Res::SystemBuckets(d, b) => Resource::SystemBuckets {
                db: fill(d),
                bucket: fill(b),
            },
        }
    }

    /// The resource as mongod renders it in `rolesInfo` and `usersInfo`.
    pub(crate) fn to_bson(&self) -> Document {
        match self {
            Resource::Ns { db, collection } => doc! { "db": db, "collection": collection },
            Resource::Cluster => doc! { "cluster": true },
            Resource::Any => doc! { "anyResource": true },
            Resource::SystemBuckets { db, bucket } if db.is_empty() => {
                doc! { "system_buckets": bucket }
            }
            Resource::SystemBuckets { db, bucket } => doc! { "db": db, "system_buckets": bucket },
        }
    }

    /// A stored resource document, read leniently: a record written by
    /// another server is not re-validated.
    pub(crate) fn from_doc(res: &Document) -> Option<Resource> {
        if res.get_bool("cluster").unwrap_or(false) {
            return Some(Resource::Cluster);
        }
        if res.get_bool("anyResource").unwrap_or(false) {
            return Some(Resource::Any);
        }
        if let Ok(bucket) = res.get_str("system_buckets") {
            return Some(Resource::SystemBuckets {
                db: res.get_str("db").unwrap_or("").to_string(),
                bucket: bucket.to_string(),
            });
        }
        Some(Resource::Ns {
            db: res.get_str("db").ok()?.to_string(),
            collection: res.get_str("collection").unwrap_or("").to_string(),
        })
    }

    /// Whether a role of `role_db`, which is not `admin`, may hold this
    /// resource: only its own database's namespaces (8.2.11).
    pub(crate) fn within(&self, role_db: &str) -> bool {
        matches!(self, Resource::Ns { db, .. } if db == role_db)
    }
}

/// A privilege: one resource, its actions in mongod's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Privilege {
    pub(crate) resource: Resource,
    pub(crate) actions: Vec<String>,
}

impl Privilege {
    pub(crate) fn to_bson(&self) -> Document {
        doc! { "resource": self.resource.to_bson(), "actions": self.actions.clone() }
    }

    /// A stored privilege document.
    pub(crate) fn from_doc(p: &Document) -> Option<Privilege> {
        let resource = Resource::from_doc(p.get_document("resource").ok()?)?;
        let mut actions: Vec<String> = p
            .get_array("actions")
            .ok()?
            .iter()
            .filter_map(|a| a.as_str().map(String::from))
            .collect();
        sort_actions(&mut actions);
        Some(Privilege { resource, actions })
    }
}

/// Where `action` sits in mongod's `ActionType` enum.
pub(crate) fn action_index(action: &str) -> Option<usize> {
    ACTIONS.iter().position(|a| *a == action)
}

/// Deduplicate `actions` and put them in mongod's order, an action this
/// build does not know after the ones it does, as given.
pub(crate) fn sort_actions(actions: &mut Vec<String>) {
    let mut seen: Vec<String> = Vec::with_capacity(actions.len());
    for a in actions.drain(..) {
        if !seen.contains(&a) {
            seen.push(a);
        }
    }
    seen.sort_by_key(|a| action_index(a).unwrap_or(usize::MAX));
    *actions = seen;
}

/// Privileges keyed by resource, in order of first appearance.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrivilegeSet(Vec<Privilege>);

impl PrivilegeSet {
    pub(crate) fn from_docs(privileges: &[Bson]) -> PrivilegeSet {
        let mut set = PrivilegeSet::default();
        for p in privileges.iter().filter_map(Bson::as_document) {
            if let Some(p) = Privilege::from_doc(p) {
                set.add(&p);
            }
        }
        set
    }

    /// Add a privilege, joining its actions to those already held on the
    /// same resource.
    pub(crate) fn add(&mut self, privilege: &Privilege) {
        match self.0.iter_mut().find(|p| p.resource == privilege.resource) {
            Some(held) => {
                held.actions.extend(privilege.actions.iter().cloned());
                sort_actions(&mut held.actions);
            }
            None => {
                let mut p = privilege.clone();
                sort_actions(&mut p.actions);
                self.0.push(p);
            }
        }
    }

    /// Take a privilege's actions away from the same resource; a privilege
    /// left with none goes.
    pub(crate) fn remove(&mut self, privilege: &Privilege) {
        if let Some(held) = self.0.iter_mut().find(|p| p.resource == privilege.resource) {
            held.actions.retain(|a| !privilege.actions.contains(a));
        }
        self.0.retain(|p| !p.actions.is_empty());
    }

    pub(crate) fn extend(&mut self, privileges: &[Privilege]) {
        for p in privileges {
            self.add(p);
        }
    }

    /// The actions held on exactly this resource pattern, if any.
    pub(crate) fn actions_on(&self, resource: &Resource) -> Option<&[String]> {
        self.0
            .iter()
            .find(|p| &p.resource == resource)
            .map(|p| p.actions.as_slice())
    }

    /// The resource patterns privileges are held on.
    pub(crate) fn resources(&self) -> impl Iterator<Item = &Resource> {
        self.0.iter().map(|p| &p.resource)
    }

    pub(crate) fn to_bson(&self) -> Vec<Bson> {
        self.0.iter().map(|p| Bson::Document(p.to_bson())).collect()
    }
}

/// The built-in roles of `db`, in the order mongod listed them.
fn table_for(db: &str) -> RoleTable {
    if db == "admin" {
        ADMIN_ROLES
    } else {
        DATABASE_ROLES
    }
}

/// Whether `role` is a built-in role of `db`: every database has `read`,
/// `readWrite` and the other five, `admin` alone has `root` and the rest.
pub(crate) fn builtin_exists(role: &str, db: &str) -> bool {
    table_for(db).iter().any(|(name, _)| *name == role)
}

/// The built-in roles `db` has.
pub(crate) fn builtin_names(db: &str) -> Vec<&'static str> {
    table_for(db).iter().map(|(name, _)| *name).collect()
}

/// The privileges of built-in `role` on `db`, or `None` when `db` has no
/// such role.
pub(crate) fn builtin_privileges(role: &str, db: &str) -> Option<Vec<Privilege>> {
    let (_, privileges) = table_for(db).iter().find(|(name, _)| *name == role)?;
    Some(
        privileges
            .iter()
            .map(|(res, actions)| Privilege {
                resource: Resource::from_table(res, db),
                actions: actions.iter().map(|a| a.to_string()).collect(),
            })
            .collect(),
    )
}

fn bad_value(message: String) -> CommandError {
    CommandError::new(2, "BadValue", message)
}

fn wrong_type(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected {expected}",
            bson_type_name(value)
        ),
    )
}

/// mongod's `safeBool`: a bool or any number.
fn is_safe_bool(value: &Bson) -> bool {
    matches!(
        value,
        Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

/// The type check of a `privileges` argument, as mongod's parser does it
/// before any privilege is looked at: `<command>.privileges.<i>` is an
/// object, `<command>.privileges.resource` and `.actions` are present and
/// typed, and so are the resource's own fields. A null field is absent.
pub(crate) fn check_privileges_shape(
    privileges: &[Bson],
    command: &str,
) -> Result<(), CommandError> {
    let path = format!("{command}.privileges");
    for (i, p) in privileges.iter().enumerate() {
        let Bson::Document(p) = p else {
            return Err(wrong_type(&format!("{path}.{i}"), p, "type 'object'"));
        };
        for (key, value) in p {
            match (key.as_str(), value) {
                (_, Bson::Null) => {}
                ("resource", Bson::Document(res)) => {
                    for (k, v) in res {
                        match (k.as_str(), v) {
                            (_, Bson::Null) => {}
                            ("db" | "collection" | "system_buckets", Bson::String(_)) => {}
                            ("db" | "collection" | "system_buckets", v) => {
                                return Err(wrong_type(
                                    &format!("{path}.resource.{k}"),
                                    v,
                                    "type 'string'",
                                ))
                            }
                            ("cluster" | "anyResource", v) if !is_safe_bool(v) => {
                                return Err(wrong_type(
                                    &format!("{path}.resource.{k}"),
                                    v,
                                    "types '[long, int, double, bool, decimal]'",
                                ))
                            }
                            _ => {}
                        }
                    }
                }
                ("resource", v) => {
                    return Err(wrong_type(&format!("{path}.resource"), v, "type 'object'"))
                }
                ("actions", Bson::Array(actions)) => {
                    for (j, a) in actions.iter().enumerate() {
                        if !matches!(a, Bson::String(_)) {
                            return Err(wrong_type(
                                &format!("{path}.actions.{j}"),
                                a,
                                "type 'string'",
                            ));
                        }
                    }
                }
                ("actions", v) => {
                    return Err(wrong_type(&format!("{path}.actions"), v, "type 'array'"))
                }
                _ => {}
            }
        }
        for required in ["resource", "actions"] {
            if !p.get(required).is_some_and(|v| !matches!(v, Bson::Null)) {
                return Err(CommandError::new(
                    40414,
                    "IDLFailedToParse",
                    format!("BSON field '{path}.{required}' is missing but a required field"),
                ));
            }
        }
    }
    Ok(())
}

/// A resource document the client sent, checked as mongod checks it: one
/// of `{db, collection}`, `{cluster: true}`, `{anyResource: true}` or
/// `{db?, system_buckets}`, and nothing that mixes them.
fn parse_resource(res: &Document) -> Result<Resource, CommandError> {
    let truthy = |key: &str| {
        res.get(key)
            .filter(|v| !matches!(v, Bson::Null))
            .map(|v| crate::util::truthy_number_or_bool(v).unwrap_or(false))
    };
    let db = res.get_str("db").ok();
    let collection = res.get_str("collection").ok();
    let bucket = res.get_str("system_buckets").ok();
    let conflicts = |with: &str| {
        if db.is_some() {
            Some("db")
        } else if collection.is_some() {
            Some("collection")
        } else if bucket.is_some() {
            Some("system_buckets")
        } else {
            None
        }
        .map(|other| {
            bad_value(format!(
                "resource: {{{with}: true}} conflicts with resource type '{other}'"
            ))
        })
    };
    if let Some(any) = truthy("anyResource") {
        if !any {
            return Err(bad_value(
                "resource: {anyResource: false} must be true".into(),
            ));
        }
        if truthy("cluster").is_some() {
            return Err(bad_value(
                "resource: {cluster: true} conflicts with resource type 'anyResource'".into(),
            ));
        }
        if let Some(e) = conflicts("anyResource") {
            return Err(e);
        }
        return Ok(Resource::Any);
    }
    if let Some(cluster) = truthy("cluster") {
        if !cluster {
            return Err(bad_value("resource: {cluster: false} must be true".into()));
        }
        if let Some(e) = conflicts("cluster") {
            return Err(e);
        }
        return Ok(Resource::Cluster);
    }
    if let Some(bucket) = bucket {
        if collection.is_some() {
            return Err(bad_value(
                "resource: {collection: '...'} conflicts with resource type 'system_buckets'"
                    .into(),
            ));
        }
        return Ok(Resource::SystemBuckets {
            db: db.unwrap_or("").to_string(),
            bucket: bucket.to_string(),
        });
    }
    match (db, collection) {
        (Some(db), Some(collection)) => Ok(Resource::Ns {
            db: db.to_string(),
            collection: collection.to_string(),
        }),
        (None, Some(_)) => Err(bad_value(
            "resource {collection: '...'} must include 'db' field as well".into(),
        )),
        _ => Err(bad_value(
            "resource pattern must contain 'collection' or 'systemBuckets' specifier".into(),
        )),
    }
}

/// The privileges a client sent, each checked in turn: the resource's shape,
/// then that it has actions, then that every action is one mongod knows.
/// Call [`check_privileges_shape`] first.
pub(crate) fn parse_privileges(privileges: &[Bson]) -> Result<Vec<Privilege>, CommandError> {
    let mut out = Vec::with_capacity(privileges.len());
    for p in privileges.iter().filter_map(Bson::as_document) {
        let resource = parse_resource(p.get_document("resource").map_err(|_| {
            bad_value(
                "resource pattern must contain 'collection' or 'systemBuckets' specifier".into(),
            )
        })?)?;
        let mut actions: Vec<String> = p
            .get_array("actions")
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if actions.is_empty() {
            return Err(bad_value(
                "'actions' field of privilege resource must not be empty".into(),
            ));
        }
        let unknown: Vec<String> = actions
            .iter()
            .filter(|a| action_index(a).is_none())
            .map(|a| format!("'{a}'"))
            .collect();
        if !unknown.is_empty() {
            return Err(bad_value(format!(
                "Unknown action type{} in privilege set: {}",
                if unknown.len() == 1 { "" } else { "s" },
                unknown.join(", ")
            )));
        }
        sort_actions(&mut actions);
        out.push(Privilege { resource, actions });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_come_back_in_mongod_order_and_deduplicated() {
        let mut actions = vec!["update".to_string(), "find".to_string(), "find".to_string()];
        sort_actions(&mut actions);
        assert_eq!(actions, ["find", "update"]);
    }

    #[test]
    fn one_resource_holds_one_privilege() {
        let on_c = |actions: &[&str]| Privilege {
            resource: Resource::Ns {
                db: "t".into(),
                collection: "c".into(),
            },
            actions: actions.iter().map(|a| a.to_string()).collect(),
        };
        let mut set = PrivilegeSet::default();
        set.add(&on_c(&["find"]));
        set.add(&on_c(&["insert"]));
        assert_eq!(
            set.to_bson(),
            vec![Bson::Document(on_c(&["find", "insert"]).to_bson())]
        );
        set.remove(&on_c(&["find", "insert"]));
        assert!(set.to_bson().is_empty());
    }

    /// `root` is a role of `admin` only; `read` is a role of every database.
    #[test]
    fn builtin_roles_belong_to_a_database() {
        assert!(builtin_exists("read", "t"));
        assert!(builtin_exists("read", "admin"));
        assert!(builtin_exists("root", "admin"));
        assert!(!builtin_exists("root", "t"));
        assert!(!builtin_exists("nosuch", "admin"));
        assert_eq!(builtin_names("t").len(), 6);
        assert_eq!(builtin_names("admin").len(), 21);
    }

    /// The privileges of `read` on a database name that database (mongod's
    /// `rolesInfo: "read", showPrivileges: true` on `urs`, 8.2.11).
    #[test]
    fn builtin_privileges_name_the_database() {
        let read = builtin_privileges("read", "urs").unwrap();
        assert_eq!(
            read[0].resource,
            Resource::Ns {
                db: "urs".into(),
                collection: "".into()
            }
        );
        assert_eq!(
            read[0].actions,
            [
                "changeStream",
                "collStats",
                "dbHash",
                "dbStats",
                "find",
                "killCursors",
                "listCollections",
                "listIndexes",
                "listSearchIndexes",
                "planCacheRead",
                "performRawDataOperations"
            ]
        );
        assert_eq!(
            read[1].resource,
            Resource::Ns {
                db: "urs".into(),
                collection: "system.js".into()
            }
        );
        assert!(builtin_privileges("root", "urs").is_none());
    }

    /// Each resource shape's refusal, worded as mongod words it.
    #[test]
    fn resource_shapes_are_checked_as_mongod_checks_them() {
        let message = |res: Document| parse_resource(&res).err().map(|e| e.errmsg);
        assert_eq!(
            message(doc! {}).as_deref(),
            Some("resource pattern must contain 'collection' or 'systemBuckets' specifier")
        );
        assert_eq!(
            message(doc! {"collection": "c"}).as_deref(),
            Some("resource {collection: '...'} must include 'db' field as well")
        );
        assert_eq!(
            message(doc! {"db": "t", "collection": "c", "cluster": true}).as_deref(),
            Some("resource: {cluster: true} conflicts with resource type 'db'")
        );
        assert_eq!(
            message(doc! {"cluster": false}).as_deref(),
            Some("resource: {cluster: false} must be true")
        );
        assert_eq!(
            message(doc! {"anyResource": true, "cluster": true}).as_deref(),
            Some("resource: {cluster: true} conflicts with resource type 'anyResource'")
        );
        assert_eq!(
            message(doc! {"db": "t", "system_buckets": "b", "collection": "c"}).as_deref(),
            Some("resource: {collection: '...'} conflicts with resource type 'system_buckets'")
        );
        assert_eq!(
            parse_resource(&doc! {"cluster": 1}).unwrap(),
            Resource::Cluster
        );
        assert_eq!(
            parse_resource(&doc! {"db": "", "system_buckets": ""})
                .unwrap()
                .to_bson(),
            doc! {"system_buckets": ""}
        );
    }

    #[test]
    fn unknown_actions_are_named_singular_or_plural() {
        let privileges = |actions: Vec<&str>| {
            vec![Bson::Document(
                doc! {"resource": {"db": "t", "collection": "c"}, "actions": actions},
            )]
        };
        assert_eq!(
            parse_privileges(&privileges(vec!["find", "bogus"]))
                .unwrap_err()
                .errmsg,
            "Unknown action type in privilege set: 'bogus'"
        );
        assert_eq!(
            parse_privileges(&privileges(vec!["bogus", "nope"]))
                .unwrap_err()
                .errmsg,
            "Unknown action types in privilege set: 'bogus', 'nope'"
        );
        assert_eq!(
            parse_privileges(&privileges(vec![])).unwrap_err().errmsg,
            "'actions' field of privilege resource must not be empty"
        );
    }
}
