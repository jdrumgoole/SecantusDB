//! Role-based access control (R5b-2): built-in roles, the action catalogue, and
//! the privilege check the dispatcher runs when `--auth` is on.
//!
//! A pure port of `src/secantus/rbac.py` — no I/O, no storage. The dispatcher
//! resolves a command to an `(action, scope)` pair, computes the target db /
//! cluster flag, and asks [`check_privilege`] whether the connection's effective
//! role bindings grant it.
//!
//! **Scope:** the built-in role catalogue (`read` / `readWrite` / `dbAdmin` /
//! `userAdmin` / `dbOwner`, the `*AnyDatabase` variants, the cluster bundles, and
//! `root`). **Custom user-defined roles** (the `createRole` family + the
//! inheritance-graph resolver Python threads through `check_privilege`) are
//! deferred to a later slice — the resolver hook isn't wired here, so an unknown
//! role name grants nothing, exactly as mongod treats a role it can't see.

// --- Action constants (mirror rbac.py) ----------------------------------

pub const A_FIND: &str = "find";
pub const A_LIST_COLLECTIONS: &str = "listCollections";
pub const A_LIST_INDEXES: &str = "listIndexes";
pub const A_LIST_DATABASES: &str = "listDatabases";
pub const A_DB_STATS: &str = "dbStats";
pub const A_COLL_STATS: &str = "collStats";
pub const A_KILL_CURSORS: &str = "killCursors";
pub const A_CHANGE_STREAM: &str = "changeStream";

pub const A_INSERT: &str = "insert";
pub const A_UPDATE: &str = "update";
pub const A_REMOVE: &str = "remove";
pub const A_BYPASS_DOC_VALIDATION: &str = "bypassDocumentValidation";

pub const A_CREATE_COLLECTION: &str = "createCollection";
pub const A_DROP_COLLECTION: &str = "dropCollection";
pub const A_DROP_DATABASE: &str = "dropDatabase";
pub const A_CREATE_INDEX: &str = "createIndex";
pub const A_DROP_INDEX: &str = "dropIndex";
pub const A_RENAME_COLL_SAME_DB: &str = "renameCollectionSameDB";
pub const A_COLL_MOD: &str = "collMod";

pub const A_CREATE_USER: &str = "createUser";
pub const A_DROP_USER: &str = "dropUser";
pub const A_GRANT_ROLE: &str = "grantRole";
pub const A_REVOKE_ROLE: &str = "revokeRole";
pub const A_VIEW_USER: &str = "viewUser";
pub const A_VIEW_ROLE: &str = "viewRole";
pub const A_CHANGE_PASSWORD: &str = "changeOwnPassword";
pub const A_CREATE_ROLE: &str = "createRole";
pub const A_DROP_ROLE: &str = "dropRole";

pub const A_SERVER_STATUS: &str = "serverStatus";
pub const A_HOST_INFO: &str = "hostInfo";
pub const A_GET_CMD_LINE_OPTS: &str = "getCmdLineOpts";
pub const A_GET_LOG: &str = "getLog";
pub const A_INPROG: &str = "inprog";
pub const A_KILLOP: &str = "killop";
pub const A_FSYNC: &str = "fsync";
pub const A_ENABLE_PROFILER: &str = "enableProfiler";

// --- Resource scopes -----------------------------------------------------

pub const SCOPE_COLLECTION: &str = "collection";
pub const SCOPE_DATABASE: &str = "database";
pub const SCOPE_CLUSTER: &str = "cluster";

// --- Built-in role action sets -------------------------------------------

const READ_ACTIONS: &[&str] = &[
    A_FIND,
    A_LIST_COLLECTIONS,
    A_LIST_INDEXES,
    A_DB_STATS,
    A_COLL_STATS,
    A_KILL_CURSORS,
    A_CHANGE_STREAM,
];

const READWRITE_EXTRA: &[&str] = &[
    A_INSERT,
    A_UPDATE,
    A_REMOVE,
    A_CREATE_COLLECTION,
    A_DROP_COLLECTION,
    A_CREATE_INDEX,
    A_DROP_INDEX,
    A_RENAME_COLL_SAME_DB,
    A_COLL_MOD,
    A_BYPASS_DOC_VALIDATION,
];

const DBADMIN_ACTIONS: &[&str] = &[
    A_LIST_COLLECTIONS,
    A_LIST_INDEXES,
    A_DB_STATS,
    A_COLL_STATS,
    A_KILL_CURSORS,
    A_CREATE_COLLECTION,
    A_DROP_COLLECTION,
    A_DROP_DATABASE,
    A_CREATE_INDEX,
    A_DROP_INDEX,
    A_RENAME_COLL_SAME_DB,
    A_COLL_MOD,
    A_VIEW_ROLE,
    A_ENABLE_PROFILER,
];

const USERADMIN_ACTIONS: &[&str] = &[
    A_CREATE_USER,
    A_DROP_USER,
    A_GRANT_ROLE,
    A_REVOKE_ROLE,
    A_VIEW_USER,
    A_VIEW_ROLE,
    A_CHANGE_PASSWORD,
    A_CREATE_ROLE,
    A_DROP_ROLE,
];

const CLUSTER_MONITOR_ACTIONS: &[&str] = &[
    A_LIST_DATABASES,
    A_SERVER_STATUS,
    A_HOST_INFO,
    A_GET_CMD_LINE_OPTS,
    A_GET_LOG,
    A_INPROG,
];

const CLUSTER_ADMIN_EXTRA: &[&str] = &[A_FSYNC, A_DROP_DATABASE, A_KILLOP];

/// Actions + scope flags for a built-in role. Mirrors `rbac.py::_RoleSpec`.
struct RoleSpec {
    actions: Vec<&'static str>,
    any_db: bool,
    cluster: bool,
    admin_only: bool,
}

fn concat(parts: &[&[&'static str]]) -> Vec<&'static str> {
    let mut v = Vec::new();
    for p in parts {
        v.extend_from_slice(p);
    }
    v
}

/// The built-in role spec for `name`, or `None` if not a built-in role.
fn built_in_role(name: &str) -> Option<RoleSpec> {
    let spec = match name {
        "read" => RoleSpec {
            actions: READ_ACTIONS.to_vec(),
            any_db: false,
            cluster: false,
            admin_only: false,
        },
        "readWrite" => RoleSpec {
            actions: concat(&[READ_ACTIONS, READWRITE_EXTRA]),
            any_db: false,
            cluster: false,
            admin_only: false,
        },
        "dbAdmin" => RoleSpec {
            actions: DBADMIN_ACTIONS.to_vec(),
            any_db: false,
            cluster: false,
            admin_only: false,
        },
        "userAdmin" => RoleSpec {
            actions: USERADMIN_ACTIONS.to_vec(),
            any_db: false,
            cluster: false,
            admin_only: false,
        },
        "dbOwner" => RoleSpec {
            actions: concat(&[
                READ_ACTIONS,
                READWRITE_EXTRA,
                DBADMIN_ACTIONS,
                USERADMIN_ACTIONS,
            ]),
            any_db: false,
            cluster: false,
            admin_only: false,
        },
        "readAnyDatabase" => RoleSpec {
            actions: READ_ACTIONS.to_vec(),
            any_db: true,
            cluster: false,
            admin_only: true,
        },
        "readWriteAnyDatabase" => RoleSpec {
            actions: concat(&[READ_ACTIONS, READWRITE_EXTRA]),
            any_db: true,
            cluster: false,
            admin_only: true,
        },
        "dbAdminAnyDatabase" => RoleSpec {
            actions: DBADMIN_ACTIONS.to_vec(),
            any_db: true,
            cluster: false,
            admin_only: true,
        },
        "userAdminAnyDatabase" => RoleSpec {
            actions: USERADMIN_ACTIONS.to_vec(),
            any_db: true,
            cluster: false,
            admin_only: true,
        },
        "clusterMonitor" => RoleSpec {
            actions: CLUSTER_MONITOR_ACTIONS.to_vec(),
            any_db: true,
            cluster: true,
            admin_only: true,
        },
        "clusterAdmin" => RoleSpec {
            actions: concat(&[CLUSTER_MONITOR_ACTIONS, CLUSTER_ADMIN_EXTRA]),
            any_db: true,
            cluster: true,
            admin_only: true,
        },
        "backup" => RoleSpec {
            actions: concat(&[READ_ACTIONS, CLUSTER_MONITOR_ACTIONS]),
            any_db: true,
            cluster: true,
            admin_only: true,
        },
        "restore" => RoleSpec {
            actions: concat(&[
                READ_ACTIONS,
                READWRITE_EXTRA,
                DBADMIN_ACTIONS,
                USERADMIN_ACTIONS,
                CLUSTER_MONITOR_ACTIONS,
                &[A_DROP_DATABASE],
            ]),
            any_db: true,
            cluster: true,
            admin_only: true,
        },
        "root" => RoleSpec {
            actions: concat(&[
                READ_ACTIONS,
                READWRITE_EXTRA,
                DBADMIN_ACTIONS,
                USERADMIN_ACTIONS,
                CLUSTER_MONITOR_ACTIONS,
                CLUSTER_ADMIN_EXTRA,
            ]),
            any_db: true,
            cluster: true,
            admin_only: true,
        },
        _ => return None,
    };
    Some(spec)
}

/// Whether `name` is a known built-in role (used by `createUser` role
/// validation). Custom roles aren't recognised here yet.
pub fn is_known_role(name: &str) -> bool {
    built_in_role(name).is_some()
}

/// Does the bound role `(role_name @ role_db)` grant `action` on the target?
/// Mirrors `rbac.py::role_grants_action`.
fn role_grants_action(
    role_name: &str,
    role_db: &str,
    action: &str,
    target_db: Option<&str>,
    cluster: bool,
) -> bool {
    let Some(spec) = built_in_role(role_name) else {
        return false;
    };
    if spec.admin_only && role_db != "admin" {
        return false; // invalid binding; never grants anything
    }
    if cluster {
        return spec.cluster && spec.actions.contains(&action);
    }
    if !spec.actions.contains(&action) {
        return false;
    }
    if spec.any_db {
        return true;
    }
    Some(role_db) == target_db
}

/// True iff any role binding in `roles` (each `(role_name, role_db)`) grants
/// `action` on the resource. Built-in roles only (see module docs).
pub fn check_privilege(
    roles: &[(String, String)],
    action: &str,
    target_db: Option<&str>,
    cluster: bool,
) -> bool {
    roles
        .iter()
        .any(|(name, db)| role_grants_action(name, db, action, target_db, cluster))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(r, d)| (r.to_string(), d.to_string()))
            .collect()
    }

    #[test]
    fn read_grants_find_on_bound_db_only() {
        let r = roles(&[("read", "app")]);
        assert!(check_privilege(&r, A_FIND, Some("app"), false));
        assert!(!check_privilege(&r, A_FIND, Some("other"), false));
        // read does NOT grant writes
        assert!(!check_privilege(&r, A_INSERT, Some("app"), false));
    }

    #[test]
    fn readwrite_grants_writes() {
        let r = roles(&[("readWrite", "app")]);
        assert!(check_privilege(&r, A_INSERT, Some("app"), false));
        assert!(check_privilege(&r, A_UPDATE, Some("app"), false));
        assert!(check_privilege(&r, A_FIND, Some("app"), false));
    }

    #[test]
    fn any_database_spans_all_dbs_but_admin_bound() {
        let ok = roles(&[("readAnyDatabase", "admin")]);
        assert!(check_privilege(&ok, A_FIND, Some("anything"), false));
        // bound to a non-admin db ⇒ invalid binding, grants nothing
        let bad = roles(&[("readAnyDatabase", "app")]);
        assert!(!check_privilege(&bad, A_FIND, Some("anything"), false));
    }

    #[test]
    fn cluster_monitor_grants_cluster_actions() {
        let r = roles(&[("clusterMonitor", "admin")]);
        assert!(check_privilege(&r, A_SERVER_STATUS, None, true));
        assert!(check_privilege(&r, A_HOST_INFO, None, true));
        // but not cluster writes
        assert!(!check_privilege(&r, A_FSYNC, None, true));
    }

    #[test]
    fn root_grants_everything() {
        let r = roles(&[("root", "admin")]);
        assert!(check_privilege(&r, A_FIND, Some("x"), false));
        assert!(check_privilege(&r, A_INSERT, Some("x"), false));
        assert!(check_privilege(&r, A_CREATE_USER, Some("x"), false));
        assert!(check_privilege(&r, A_SERVER_STATUS, None, true));
        assert!(check_privilege(&r, A_FSYNC, None, true));
    }

    #[test]
    fn useradmin_grants_user_management() {
        let r = roles(&[("userAdmin", "app")]);
        assert!(check_privilege(&r, A_CREATE_USER, Some("app"), false));
        assert!(check_privilege(&r, A_DROP_USER, Some("app"), false));
        // but not data reads
        assert!(!check_privilege(&r, A_FIND, Some("app"), false));
    }

    #[test]
    fn unknown_role_grants_nothing() {
        let r = roles(&[("madeUpRole", "app")]);
        assert!(!check_privilege(&r, A_FIND, Some("app"), false));
        assert!(!is_known_role("madeUpRole"));
        assert!(is_known_role("readWrite"));
    }

    #[test]
    fn dbowner_is_union_of_rw_dbadmin_useradmin() {
        let r = roles(&[("dbOwner", "app")]);
        assert!(check_privilege(&r, A_INSERT, Some("app"), false)); // readWrite
        assert!(check_privilege(&r, A_DROP_DATABASE, Some("app"), false)); // dbAdmin
        assert!(check_privilege(&r, A_CREATE_USER, Some("app"), false)); // userAdmin
    }
}
