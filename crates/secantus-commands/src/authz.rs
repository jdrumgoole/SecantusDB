//! What each command requires of an authenticated user, as mongod 8.2.11
//! checks it (`tools/probes/auth_enforcement.py`, 2026-10-14). The
//! privileges come from [`crate::rbac`]; this module says which actions on
//! which resource a command needs, reading its arguments where that
//! depends on them.
//!
//! The default is closed: a command the server has and this table does not
//! name is refused for everyone. A command any authenticated user may run
//! says so here with `Ok(())`.

use bson::{Bson, Document};

use crate::authz_pipeline::{self as pipe, any_normal, ns, Need};
use crate::privileges::{Privilege, Resource};
use crate::rbac::Authz;
use crate::{CommandContext, CommandError};

/// mongod's refusal: the database and the command, not what was missing.
pub(crate) fn unauthorized(db: &str, doc: &Document) -> CommandError {
    CommandError::new(
        13,
        "Unauthorized",
        format!("not authorized on {db} to execute command {doc}"),
    )
}

fn database(db: &str) -> Resource {
    ns(db, "")
}

fn truthy(value: Option<&Bson>) -> bool {
    value
        .and_then(crate::util::truthy_number_or_bool)
        .unwrap_or(false)
}

/// The collection a command's first value names; a value that is not a
/// string makes the whole database the target.
fn target(doc: &Document, name: &str, db: &str) -> Resource {
    match doc.get_str(name) {
        Ok(coll) => ns(db, coll),
        Err(_) => database(db),
    }
}

struct Check<'a> {
    az: &'a Authz,
    db: &'a str,
    allowed: bool,
}

impl Check<'_> {
    fn need(&mut self, resource: Resource, actions: &[&str]) {
        if !self.az.is_authorized(&resource, actions) {
            self.allowed = false;
        }
    }

    fn needs(&mut self, needs: Vec<Need>) {
        for (resource, actions) in needs {
            self.need(resource, &actions);
        }
    }

    fn cluster(&mut self, action: &str) {
        self.need(Resource::Cluster, &[action]);
    }

    fn on_collection(&mut self, doc: &Document, name: &str, actions: &[&str]) {
        self.need(target(doc, name, self.db), actions);
    }

    fn on_database(&mut self, actions: &[&str]) {
        self.need(database(self.db), actions);
    }

    /// `grantRole` (or `revokeRole`) on each named role's database.
    fn on_roles(&mut self, doc: &Document, field: &str, action: &str) -> Result<(), CommandError> {
        if let Some(Bson::Array(items)) = doc.get(field) {
            for (_, role_db) in crate::umc::parse_role_names(items, self.db)? {
                self.need(database(&role_db), &[action]);
            }
        }
        Ok(())
    }

    /// `grantRole` (or `revokeRole`) on the database of each privilege's
    /// resource; a privilege on the cluster, on any resource or on every
    /// database takes it on `admin`.
    fn on_privileges(&mut self, doc: &Document, action: &str) {
        let Some(Bson::Array(items)) = doc.get("privileges") else {
            return;
        };
        for item in items {
            let db = match item.as_document().and_then(Privilege::from_doc) {
                Some(Privilege {
                    resource: Resource::Ns { db, .. },
                    ..
                }) if !db.is_empty() => db,
                _ => "admin".to_string(),
            };
            self.need(database(&db), &[action]);
        }
    }
}

/// The user or users a `usersInfo` names, as `(db, user)`: `None` for a
/// form that names a whole database or every database.
fn named_users(arg: Option<&Bson>, db: &str) -> Option<Vec<(String, String)>> {
    let one = |item: &Bson| -> Option<(String, String)> {
        match item {
            Bson::String(user) => Some((db.to_string(), user.clone())),
            Bson::Document(d) => Some((
                d.get_str("db").ok()?.to_string(),
                d.get_str("user").ok()?.to_string(),
            )),
            _ => None,
        }
    };
    match arg {
        Some(Bson::Array(items)) => items.iter().map(one).collect(),
        Some(item @ (Bson::String(_) | Bson::Document(_))) => one(item).map(|u| vec![u]),
        _ => None,
    }
}

/// The roles a `rolesInfo` names, as `(role, db)`: `None` for `rolesInfo: 1`.
fn named_roles(arg: Option<&Bson>, db: &str) -> Option<Vec<(String, String)>> {
    match arg {
        Some(Bson::Array(items)) => crate::umc::parse_role_names(items, db).ok(),
        Some(item @ (Bson::String(_) | Bson::Document(_))) => {
            crate::umc::parse_role_names(std::slice::from_ref(item), db).ok()
        }
        _ => None,
    }
}

/// Refuse `name` unless the connection's privileges cover what mongod
/// checks for it. `db` is the command's database.
pub(crate) fn check(
    name: &str,
    doc: &Document,
    db: &str,
    ctx: &CommandContext,
    az: &Authz,
) -> Result<(), CommandError> {
    let mut c = Check {
        az,
        db,
        allowed: true,
    };
    match name {
        // Any authenticated user: the handshake, its own session, its
        // own operations and cursors (the cursor checks are in the
        // handlers, which know the cursor), and the catalogue of commands.
        "hello" | "isMaster" | "ismaster" | "ping" | "buildInfo" | "buildinfo" | "saslStart"
        | "saslContinue" | "authenticate" | "logout" | "connectionStatus" | "whatsmyuri"
        | "listCommands" | "startSession" | "endSessions" | "refreshSessions" | "getMore"
        | "killCursors" | "releaseMemory" | "commitTransaction" | "abortTransaction"
        | "configureFailPoint" | "listDatabases" | "features" => {}
        "killSessions" => {
            // Its own sessions need nothing; another user's need the cluster.
            let own = ctx.conn_auth.as_ref().map(|a| {
                let guard = a.lock().unwrap_or_else(|e| e.into_inner());
                guard.authenticated.clone()
            });
            if let Some(Bson::Array(listed)) = doc.get("killSessions") {
                let digest = own.as_deref().map(crate::session::user_digest);
                let all_own =
                    listed
                        .iter()
                        .all(|s| match s.as_document().and_then(|d| d.get("uid")) {
                            None => true,
                            Some(Bson::Binary(b)) => digest.as_ref().is_some_and(|d| *d == b.bytes),
                            Some(_) => false,
                        });
                if !all_own {
                    c.cluster("killAnySession");
                }
            }
        }
        "killAllSessions" | "killAllSessionsByPattern" => c.cluster("killAnySession"),

        "find" | "count" | "distinct" => c.on_collection(doc, name, &["find"]),
        "dataSize" | "datasize" => match doc.get_str(name).ok().and_then(pipe::namespace) {
            Some(target) => c.need(target, &["find"]),
            None => c.on_database(&["find"]),
        },
        "insert" => {
            let mut actions = vec!["insert"];
            if truthy(doc.get("bypassDocumentValidation")) {
                actions.push("bypassDocumentValidation");
            }
            c.on_collection(doc, name, &actions);
        }
        "update" => {
            let mut actions = vec!["update"];
            let upserts = match doc.get("updates") {
                Some(Bson::Array(stmts)) => stmts
                    .iter()
                    .any(|s| truthy(s.as_document().and_then(|d| d.get("upsert")))),
                _ => false,
            };
            if upserts {
                actions.push("insert");
            }
            if truthy(doc.get("bypassDocumentValidation")) {
                actions.push("bypassDocumentValidation");
            }
            c.on_collection(doc, name, &actions);
        }
        "delete" => c.on_collection(doc, name, &["remove"]),
        "findAndModify" | "findandmodify" => {
            let mut actions = vec!["find"];
            if truthy(doc.get("remove")) {
                actions.push("remove");
            } else {
                actions.push("update");
                if truthy(doc.get("upsert")) {
                    actions.push("insert");
                }
            }
            if truthy(doc.get("bypassDocumentValidation")) {
                actions.push("bypassDocumentValidation");
            }
            c.on_collection(doc, name, &actions);
        }
        "bulkWrite" => c.needs(pipe::bulk_write(doc)),
        "aggregate" => c.needs(pipe::aggregate(doc, db)),
        "mapReduce" | "mapreduce" => c.needs(pipe::map_reduce(doc, db)),
        "explain" => {
            if let Some(Bson::Document(inner)) = doc.get("explain") {
                let inner_name = crate::command_name(inner);
                return check(inner_name, inner, db, ctx, az);
            }
        }

        "listIndexes" => c.on_collection(doc, name, &["listIndexes"]),
        "createIndexes" => c.on_collection(doc, name, &["createIndex"]),
        "dropIndexes" => c.on_collection(doc, name, &["dropIndex"]),
        "createSearchIndexes" | "updateSearchIndex" | "dropSearchIndex" => {
            c.on_collection(doc, name, &[name]);
        }
        "create" => {
            // `createCollection` or `insert` on the collection makes it; a
            // capped one needs `convertToCapped` too. A view needs
            // `createCollection` itself and nothing on `viewOn` (measured:
            // a user with `insert` on any resource makes a collection and
            // not a view; one with `createCollection` on the view alone
            // makes it).
            let target = target(doc, name, db);
            let view = doc.get("viewOn").is_some_and(|v| !matches!(v, Bson::Null));
            if !az.is_authorized(&target, &["createCollection"])
                && (view || !az.is_authorized(&target, &["insert"]))
            {
                c.allowed = false;
            }
            if truthy(doc.get("capped")) {
                c.need(target, &["convertToCapped"]);
            }
        }
        "drop" => c.on_collection(doc, name, &["dropCollection"]),
        "collMod" | "collmod" => c.on_collection(doc, name, &["collMod"]),
        "convertToCapped" => c.on_collection(doc, name, &["convertToCapped"]),
        "cloneCollectionAsCapped" => {
            c.on_collection(doc, name, &["find"]);
            match doc.get_str("toCollection") {
                Ok(to) => c.need(ns(db, to), &["insert", "createIndex", "convertToCapped"]),
                Err(_) => c.on_database(&["insert", "createIndex", "convertToCapped"]),
            }
        }
        "compact" => c.on_collection(doc, name, &["compact"]),
        "reIndex" => c.on_collection(doc, name, &["reIndex"]),
        "dropDatabase" => c.on_database(&["dropDatabase"]),
        "renameCollection" => {
            let source = doc.get_str(name).ok().and_then(pipe::namespace);
            let to = doc.get_str("to").ok().and_then(pipe::namespace);
            let drop_target = truthy(doc.get("dropTarget"));
            let (Some(source), Some(to)) = (source, to) else {
                c.allowed = false;
                return finish(c, db, doc);
            };
            let same_db = matches!((&source, &to), (Resource::Ns { db: a, collection: ca }, Resource::Ns { db: b, collection: cb })
                if a == b && !ca.starts_with("system.") && !cb.starts_with("system."));
            let source_db = match &source {
                Resource::Ns { db, .. } => db.clone(),
                _ => db.to_string(),
            };
            // `renameCollectionSameDB` on the database is enough within one
            // database (with `dropCollection` on the target when it is
            // dropped); otherwise the source is read and dropped and the
            // target written and indexed.
            if same_db && az.is_authorized(&database(&source_db), &["renameCollectionSameDB"]) {
                if drop_target {
                    c.need(to, &["dropCollection"]);
                }
            } else {
                c.need(source, &["find", "dropCollection"]);
                let mut actions = vec!["insert", "createIndex"];
                if drop_target {
                    actions.push("dropCollection");
                }
                c.need(to, &actions);
            }
        }
        "listCollections" => {
            // `listCollections` on the database; or, for the names only of
            // the collections the user may touch, any action in it.
            let name_only = truthy(doc.get("nameOnly"));
            let authorized_only = truthy(doc.get("authorizedCollections"));
            if !az.is_authorized(&database(db), &["listCollections"])
                && !(name_only && authorized_only && az.any_action_in_db(db))
            {
                c.allowed = false;
            }
        }
        "collStats" | "collstats" => c.on_collection(doc, name, &["collStats"]),
        "dbStats" | "dbstats" => c.on_database(&["dbStats"]),
        "validate" => c.on_collection(doc, name, &["validate"]),
        "profile" => {
            // Reading the level needs only `find` on `system.profile`;
            // setting it, or reading with `slowms` / `sampleRate` /
            // `filter`, needs `enableProfiler` on the database.
            let level = doc.get(name).and_then(crate::util::as_i64).unwrap_or(0);
            let reads = level < 0
                && !["slowms", "sampleRate", "filter"]
                    .iter()
                    .any(|f| doc.get(f).is_some_and(|v| !matches!(v, Bson::Null)));
            if !(reads && az.is_authorized(&ns(db, "system.profile"), &["find"])) {
                c.on_database(&["enableProfiler"]);
            }
        }

        "serverStatus" => c.cluster("serverStatus"),
        "hostInfo" => c.cluster("hostInfo"),
        "top" => c.cluster("top"),
        "getCmdLineOpts" => c.cluster("getCmdLineOpts"),
        "getLog" => c.cluster("getLog"),
        "getParameter" => c.cluster("getParameter"),
        "setParameter" => c.cluster("setParameter"),
        "fsync" => c.cluster("fsync"),
        "fsyncUnlock" => c.cluster("unlock"),
        "replSetGetStatus" => c.cluster("replSetGetStatus"),
        "replSetGetConfig" => c.cluster("replSetGetConfig"),
        "replSetReconfig" => c.cluster("replSetConfigure"),
        "replSetStepDown" => c.cluster("replSetStateChange"),
        "getDefaultRWConcern" => c.cluster("getDefaultRWConcern"),
        "setDefaultRWConcern" => c.cluster("setDefaultRWConcern"),
        "setFeatureCompatibilityVersion" => c.cluster("setFeatureCompatibilityVersion"),
        "logRotate" => c.cluster("logRotate"),
        "connPoolStats" => c.cluster("connPoolStats"),
        "replSetInitiate" => c.cluster("replSetConfigure"),
        "replSetResizeOplog" => c.cluster("replSetResizeOplog"),
        "appendOplogNote" => c.cluster("appendOplogNote"),
        "lockInfo" => c.cluster("serverStatus"),
        "getClusterParameter" => c.cluster("getClusterParameter"),
        "setClusterParameter" => c.cluster("setClusterParameter"),
        "invalidateUserCache" => c.cluster("invalidateUserCache"),
        "replSetGetRBID" => c.cluster("internal"),
        "replSetFreeze" | "replSetMaintenance" => c.cluster("replSetStateChange"),
        "dropConnections" => c.cluster("dropConnections"),
        // `filemd5` reads `<root>.chunks`.
        "filemd5" => {
            let root = doc.get_str("root").unwrap_or("fs");
            let chunks = format!("{root}.chunks");
            c.need(ns(db, &chunks), &["find"]);
        }
        "planCacheClear" => c.on_collection(doc, name, &["planCacheWrite"]),
        "planCacheClearFilters" | "planCacheSetFilter" => {
            c.on_collection(doc, name, &["planCacheIndexFilter"])
        }
        "planCacheListFilters" => c.on_collection(doc, name, &["planCacheRead"]),
        "dbHash" | "dbhash" => c.on_database(&["dbHash"]),
        "shardingState" => c.cluster("shardingState"),
        "killOp" => c.cluster("killop"),
        "currentOp" => {
            if !truthy(doc.get("$ownOps")) {
                c.cluster("inprog");
            }
        }
        // This server's own commands, which mongod does not have: the
        // backup ones take what the `backup` role has on the cluster, the
        // restore one what `restore` has, the pruning ones what
        // `hostManager` has.
        "secantusAdmin.backupArchive" | "secantusAdmin.archiveBaseSnapshot" => {
            c.cluster("readBackupFile");
        }
        "secantusAdmin.restoreArchive" => c.cluster("applyOps"),
        "secantusAdmin.pruneOplog" => c.cluster("replSetResizeOplog"),
        "secantusAdmin.pruneTtl" => c.cluster("compact"),

        "usersInfo" => match named_users(doc.get(name), db) {
            Some(users) => {
                for (user_db, user) in users {
                    if !az.is_self(&user_db, &user) {
                        c.need(database(&user_db), &["viewUser"]);
                    }
                }
            }
            None => {
                let every_db = doc
                    .get_document(name)
                    .ok()
                    .is_some_and(|d| truthy(d.get("forAllDBs")));
                if every_db {
                    c.cluster("viewUser");
                } else {
                    c.on_database(&["viewUser"]);
                }
            }
        },
        "createUser" => {
            c.on_database(&["createUser"]);
            c.on_roles(doc, "roles", "grantRole")?;
            if doc.contains_key("authenticationRestrictions") {
                c.on_database(&["setAuthenticationRestriction"]);
            }
        }
        "updateUser" => {
            let user = doc.get_str(name).unwrap_or("");
            let own = az.is_self(db, user);
            let given = |field: &str| doc.get(field).is_some_and(|v| !matches!(v, Bson::Null));
            if (given("pwd") || given("mechanisms"))
                && !(own && az.is_authorized(&database(db), &["changeOwnPassword"]))
            {
                c.on_database(&["changePassword"]);
            }
            if given("customData")
                && !(own && az.is_authorized(&database(db), &["changeOwnCustomData"]))
            {
                c.on_database(&["changeCustomData"]);
            }
            if given("authenticationRestrictions") {
                c.on_database(&["setAuthenticationRestriction"]);
            }
            if given("roles") {
                // Any role might be revoked, so the right to revoke any.
                c.need(any_normal(), &["revokeRole"]);
                c.on_roles(doc, "roles", "grantRole")?;
            }
        }
        "dropUser" | "dropAllUsersFromDatabase" => c.on_database(&["dropUser"]),
        "grantRolesToUser" | "grantRolesToRole" => c.on_roles(doc, "roles", "grantRole")?,
        "revokeRolesFromUser" | "revokeRolesFromRole" => {
            c.on_roles(doc, "roles", "revokeRole")?;
        }
        "rolesInfo" => match named_roles(doc.get(name), db) {
            Some(roles) => {
                for (role, role_db) in roles {
                    if !az.holds_role(&role, &role_db) {
                        c.need(database(&role_db), &["viewRole"]);
                    }
                }
            }
            None => c.on_database(&["viewRole"]),
        },
        "createRole" => {
            c.on_database(&["createRole"]);
            c.on_roles(doc, "roles", "grantRole")?;
            c.on_privileges(doc, "grantRole");
            if doc.contains_key("authenticationRestrictions") {
                c.on_database(&["setAuthenticationRestriction"]);
            }
        }
        "updateRole" => {
            c.need(any_normal(), &["revokeRole"]);
            c.on_roles(doc, "roles", "grantRole")?;
            c.on_privileges(doc, "grantRole");
            if doc.contains_key("authenticationRestrictions") {
                c.on_database(&["setAuthenticationRestriction"]);
            }
        }
        "dropRole" | "dropAllRolesFromDatabase" => c.on_database(&["dropRole"]),
        "grantPrivilegesToRole" => c.on_privileges(doc, "grantRole"),
        "revokePrivilegesFromRole" => c.on_privileges(doc, "revokeRole"),

        // Closed by default: a command this table does not know is refused.
        _ => c.allowed = false,
    }
    finish(c, db, doc)
}

fn finish(c: Check<'_>, db: &str, doc: &Document) -> Result<(), CommandError> {
    if c.allowed {
        Ok(())
    } else {
        Err(unauthorized(db, doc))
    }
}
