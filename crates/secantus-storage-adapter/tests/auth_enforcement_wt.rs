//! What an authenticated user may run under `--auth`, over real WiredTiger.
//! Every expectation is what mongod 8.2.11 answered to the same user and
//! command (`tools/probes/auth_enforcement.py`, 2026-10-14): `13` is
//! `Unauthorized`, `None` is a command that ran, and any other code is a
//! command that was allowed and then failed on its own account.

mod common;

use std::sync::{Arc, Mutex};

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::auth::ConnectionAuth;
use secantus_commands::failpoints::FailPointRegistry;
use secantus_commands::CommandContext;

const DB: &str = "ae";
const DB2: &str = "ae2";

/// The error code of a reply, `None` when it ran.
fn code(reply: &Document) -> Option<i32> {
    if reply.get_f64("ok") == Ok(1.0) {
        None
    } else {
        Some(reply.get_i32("code").unwrap_or(-1))
    }
}

/// Dispatch on `db`, with the data of a cursor merged in.
fn on(c: &mut CommandContext, db: &str, command: Document) -> Document {
    let was = std::mem::replace(&mut c.db_name, db.to_string());
    let reply = dispatch_full(&command, c);
    c.db_name = was;
    reply
}

fn run(c: &mut CommandContext, db: &str, command: Document) -> Option<i32> {
    code(&on(c, db, command))
}

/// Make a stored role on `admin`, as the probe does, so it may name any
/// resource.
fn role(c: &mut CommandContext, name: &str, privileges: Vec<Document>, roles: Vec<Document>) {
    let reply = on(
        c,
        "admin",
        doc! {"createRole": name, "privileges": privileges, "roles": roles},
    );
    assert_eq!(code(&reply), None, "createRole {name}: {reply}");
}

fn user(c: &mut CommandContext, name: &str, roles: Vec<Document>) {
    let reply = on(
        c,
        "admin",
        doc! {"createUser": name, "pwd": "pw", "roles": roles},
    );
    assert_eq!(code(&reply), None, "createUser {name}: {reply}");
}

fn privilege(db: &str, collection: &str, actions: &[&str]) -> Document {
    doc! {"resource": {"db": db, "collection": collection}, "actions": actions.to_vec()}
}

fn cluster(actions: &[&str]) -> Document {
    doc! {"resource": {"cluster": true}, "actions": actions.to_vec()}
}

fn held(role: &str, db: &str) -> Document {
    doc! {"role": role, "db": db}
}

/// The data every user starts from: `ae.c1` and `ae.c2` with three
/// documents each, `ae2.c1` with one, and something in `local` and `config`.
fn seed(c: &mut CommandContext) {
    for (db, coll, n) in [(DB, "c1", 3), (DB, "c2", 3), (DB2, "c1", 1)] {
        on(c, db, doc! {"drop": coll});
        let docs: Vec<Bson> = (0..n)
            .map(|i| Bson::Document(doc! {"_id": i, "a": i}))
            .collect();
        on(c, db, doc! {"insert": coll, "documents": docs});
    }
    on(
        c,
        "local",
        doc! {"insert": "stuff", "documents": [{"_id": 1}]},
    );
    on(
        c,
        "config",
        doc! {"insert": "settings", "documents": [{"_id": 1}]},
    );
    on(c, "admin", doc! {"insert": "c", "documents": [{"_id": 1}]});
}

/// Turn access control on, with `name` (a user of `admin`) authenticated.
fn as_user(c: &mut CommandContext, name: &str) {
    let auth = ConnectionAuth::new();
    let auth = Arc::new(Mutex::new(auth));
    auth.lock()
        .unwrap()
        .authenticated
        .push(("admin".to_string(), name.to_string()));
    c.conn_auth = Some(auth);
    c.require_auth = true;
}

/// Access control off, for seeding.
fn as_nobody(c: &mut CommandContext) {
    c.conn_auth = None;
    c.require_auth = false;
}

fn find(coll: &str) -> Document {
    doc! {"find": coll}
}

fn names(reply: &Document) -> Vec<String> {
    let mut out: Vec<String> = reply
        .get_document("cursor")
        .and_then(|c| c.get_array("firstBatch"))
        .map(|b| {
            b.iter()
                .filter_map(|d| d.as_document()?.get_str("name").ok())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn db_names(reply: &Document) -> Vec<String> {
    let mut out: Vec<String> = reply
        .get_array("databases")
        .map(|b| {
            b.iter()
                .filter_map(|d| d.as_document()?.get_str("name").ok())
                .filter(|n| [DB, DB2, "admin", "local", "config"].contains(n))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// A privilege on `ae.c1` reaches `ae.c1` and nothing else: not `c2`, not
/// another database's `c1`, not a write (users `c_collFind`, `c_collWrite`).
#[test]
fn a_collection_privilege_names_the_collection() {
    with_wt(|c| {
        seed(c);
        role(c, "collFind", vec![privilege(DB, "c1", &["find"])], vec![]);
        role(
            c,
            "collWrite",
            vec![privilege(DB, "c1", &["insert", "update", "remove"])],
            vec![],
        );
        user(c, "reader", vec![held("collFind", "admin")]);
        user(c, "writer", vec![held("collWrite", "admin")]);

        as_user(c, "reader");
        assert_eq!(run(c, DB, find("c1")), None);
        assert_eq!(run(c, DB, find("c2")), Some(13));
        assert_eq!(run(c, DB2, find("c1")), Some(13));
        assert_eq!(run(c, DB, doc! {"count": "c1"}), None);
        assert_eq!(run(c, DB, doc! {"distinct": "c1", "key": "a"}), None);
        assert_eq!(run(c, DB, doc! {"dataSize": "ae.c1"}), None);
        assert_eq!(
            run(c, DB, doc! {"insert": "c1", "documents": [{"_id": 9}]}),
            Some(13)
        );
        assert_eq!(run(c, DB, doc! {"listIndexes": "c1"}), Some(13));
        assert_eq!(run(c, DB, doc! {"listCollections": 1}), Some(13));

        as_user(c, "writer");
        assert_eq!(run(c, DB, find("c1")), Some(13));
        assert_eq!(
            run(c, DB, doc! {"insert": "c1", "documents": [{"_id": 9}]}),
            None
        );
        assert_eq!(
            run(c, DB, doc! {"insert": "c2", "documents": [{"_id": 9}]}),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"update": "c1", "updates": [{"q": {"_id": 0}, "u": {"$set": {"b": 1}}}]}
            ),
            None
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"delete": "c1", "deletes": [{"q": {"_id": 0}, "limit": 1}]}
            ),
            None
        );
        // `findAndModify` reads as well as writes.
        assert_eq!(
            run(
                c,
                DB,
                doc! {"findAndModify": "c1", "query": {"_id": 1}, "update": {"$set": {"b": 1}}}
            ),
            Some(13)
        );
        // `insert` on a collection is enough to create it: an existing one
        // answers for itself rather than refusing.
        assert_ne!(run(c, DB, doc! {"create": "c1"}), Some(13));
        // Bypassing validation is an action of its own.
        assert_eq!(
            run(
                c,
                DB,
                doc! {"insert": "c1", "documents": [{"_id": 10}], "bypassDocumentValidation": true}
            ),
            Some(13)
        );
    });
}

/// `{db, collection: ""}` covers the normal collections of the database and
/// not its system collections; `read` adds `system.js` by name and `dbAdmin`
/// adds `system.profile` (users `c_dbFind`, `db_read`, `db_dbAdmin`).
#[test]
fn a_database_privilege_skips_system_collections() {
    with_wt(|c| {
        seed(c);
        role(c, "dbFind", vec![privilege(DB, "", &["find"])], vec![]);
        user(c, "db", vec![held("dbFind", "admin")]);
        user(c, "reader", vec![held("read", DB)]);
        user(c, "dba", vec![held("dbAdmin", DB)]);

        as_user(c, "db");
        assert_eq!(run(c, DB, find("c1")), None);
        assert_eq!(run(c, DB, find("c2")), None);
        assert_eq!(run(c, DB, find("nope")), None);
        assert_eq!(run(c, DB, find("system.js")), Some(13));
        assert_eq!(run(c, DB, find("system.profile")), Some(13));
        assert_eq!(run(c, DB2, find("c1")), Some(13));

        as_user(c, "reader");
        assert_eq!(run(c, DB, find("system.js")), None);
        assert_eq!(run(c, DB, find("system.profile")), Some(13));
        assert_eq!(run(c, DB, doc! {"listCollections": 1}), None);
        assert_eq!(run(c, DB, doc! {"collStats": "c1"}), None);
        assert_eq!(run(c, DB, doc! {"dbStats": 1}), None);

        as_user(c, "dba");
        assert_eq!(run(c, DB, find("system.profile")), None);
        assert_eq!(run(c, DB, find("c1")), Some(13));
        assert_eq!(
            run(c, DB, doc! {"collMod": "c1", "validationLevel": "moderate"}),
            None
        );
        assert_eq!(run(c, DB, doc! {"validate": "c1"}), None);
        assert_eq!(run(c, DB, doc! {"compact": "c1"}), None);
        assert_eq!(
            run(
                c,
                DB,
                doc! {"createIndexes": "c1", "indexes": [{"key": {"b": 1}, "name": "b_1"}]}
            ),
            None
        );
        assert_eq!(run(c, DB, doc! {"dropDatabase": 1}), None);
    });
}

/// `{db: "", collection: ""}` covers every normal collection of every
/// database but `local` and `config`; `{db: "", collection: "c1"}` covers
/// that name everywhere, `local` included; `{db: "local", collection: ""}`
/// covers `local` (users `c_everyFind`, `c_nameFind`, `c_localFind`,
/// `adm_readAnyDatabase`, `adm_clusterMonitor`).
#[test]
fn every_database_and_collection_name_patterns() {
    with_wt(|c| {
        seed(c);
        role(c, "everyFind", vec![privilege("", "", &["find"])], vec![]);
        role(c, "nameFind", vec![privilege("", "c1", &["find"])], vec![]);
        role(
            c,
            "localFind",
            vec![privilege("local", "", &["find"])],
            vec![],
        );
        user(c, "every", vec![held("everyFind", "admin")]);
        user(c, "name", vec![held("nameFind", "admin")]);
        user(c, "local", vec![held("localFind", "admin")]);
        user(c, "readAny", vec![held("readAnyDatabase", "admin")]);
        user(c, "monitor", vec![held("clusterMonitor", "admin")]);

        as_user(c, "every");
        assert_eq!(run(c, DB, find("c1")), None);
        assert_eq!(run(c, DB2, find("c1")), None);
        assert_eq!(run(c, "local", find("stuff")), Some(13));
        assert_eq!(run(c, "config", find("settings")), Some(13));
        assert_eq!(run(c, DB, find("system.js")), Some(13));

        as_user(c, "name");
        assert_eq!(run(c, DB, find("c1")), None);
        assert_eq!(run(c, DB2, find("c1")), None);
        assert_eq!(run(c, DB, find("c2")), Some(13));

        as_user(c, "local");
        assert_eq!(run(c, "local", find("stuff")), None);
        assert_eq!(run(c, DB, find("c1")), Some(13));

        as_user(c, "readAny");
        assert_eq!(run(c, DB, find("c1")), None);
        assert_eq!(run(c, DB, find("system.js")), None);
        assert_eq!(run(c, "admin", find("system.users")), Some(13));
        assert_eq!(run(c, "local", find("stuff")), Some(13));

        as_user(c, "monitor");
        assert_eq!(run(c, "local", find("stuff")), None);
        assert_eq!(run(c, "config", find("settings")), None);
        assert_eq!(run(c, DB, find("c1")), Some(13));
        assert_eq!(run(c, DB, doc! {"collStats": "c1"}), None);
        assert_eq!(run(c, "admin", doc! {"serverStatus": 1}), None);
    });
}

/// The built-in roles grant what mongod's table says, no more: `readWrite`
/// has no `collMod`, `restore` has no `find`, `hostManager` no
/// `serverStatus`, `enableSharding` and `directShardOperations` nothing a
/// user runs here, and a built-in bound off its database grants nothing.
#[test]
fn built_in_roles_come_from_the_table() {
    with_wt(|c| {
        seed(c);
        user(c, "rw", vec![held("readWrite", DB)]);
        user(c, "admrw", vec![held("readWrite", "admin")]);
        user(c, "restore", vec![held("restore", "admin")]);
        user(c, "host", vec![held("hostManager", "admin")]);
        user(c, "shard", vec![held("enableSharding", DB)]);
        user(c, "direct", vec![held("directShardOperations", "admin")]);
        user(c, "backup", vec![held("backup", "admin")]);
        user(c, "uaad", vec![held("userAdminAnyDatabase", "admin")]);

        as_user(c, "rw");
        assert_eq!(
            run(c, DB, doc! {"insert": "c1", "documents": [{"_id": 9}]}),
            None
        );
        assert_eq!(
            run(c, DB2, doc! {"insert": "c1", "documents": [{"_id": 9}]}),
            Some(13)
        );
        assert_eq!(
            run(c, DB, doc! {"collMod": "c1", "validationLevel": "moderate"}),
            Some(13)
        );
        assert_eq!(run(c, DB, doc! {"validate": "c1"}), Some(13));
        assert_eq!(run(c, DB, doc! {"dropDatabase": 1}), Some(13));
        assert_eq!(
            run(c, DB, doc! {"create": "made", "capped": true, "size": 4096}),
            None
        );
        assert_eq!(
            run(c, DB, doc! {"convertToCapped": "c2", "size": 4096}),
            None
        );
        assert_eq!(run(c, DB, doc! {"usersInfo": 1}), Some(13));

        as_user(c, "admrw");
        assert_eq!(run(c, DB, find("c1")), Some(13));
        assert_eq!(run(c, "admin", find("system.users")), Some(13));

        as_user(c, "restore");
        assert_eq!(
            run(c, DB, doc! {"insert": "c1", "documents": [{"_id": 9}]}),
            None
        );
        assert_eq!(run(c, DB, find("c1")), Some(13));
        assert_eq!(
            run(c, DB, doc! {"createUser": "v", "pwd": "x", "roles": []}),
            None
        );
        assert_eq!(run(c, DB, doc! {"listCollections": 1}), None);

        as_user(c, "host");
        assert_eq!(
            run(c, "admin", doc! {"setParameter": 1, "logLevel": 0}),
            None
        );
        assert_eq!(run(c, "admin", doc! {"serverStatus": 1}), Some(13));
        assert_eq!(run(c, DB, doc! {"killAllSessions": []}), None);
        assert_eq!(run(c, "admin", doc! {"fsync": 1}), None);

        as_user(c, "shard");
        assert_eq!(run(c, DB, find("c1")), Some(13));
        as_user(c, "direct");
        assert_eq!(run(c, DB, find("c1")), Some(13));
        assert_eq!(run(c, "admin", doc! {"serverStatus": 1}), Some(13));

        as_user(c, "backup");
        assert_eq!(run(c, DB, find("c1")), None);
        assert_eq!(run(c, "admin", find("system.users")), None);
        assert_eq!(
            run(c, DB, doc! {"insert": "c1", "documents": [{"_id": 9}]}),
            Some(13)
        );
        assert_eq!(run(c, DB, doc! {"listIndexes": "c1"}), None);
        assert_eq!(
            run(c, "admin", doc! {"getParameter": 1, "logLevel": 1}),
            None
        );

        as_user(c, "uaad");
        assert_eq!(run(c, "admin", find("system.users")), None);
        assert_eq!(
            run(c, "admin", doc! {"usersInfo": {"forAllDBs": true}}),
            None
        );
        assert_eq!(run(c, DB, find("c1")), Some(13));
    });
}

/// A stored role named like a built-in, on a database that has no such
/// built-in, is the stored role: `root` on `app` finds `app.c` and runs
/// nothing on the cluster. Roles a role holds are inherited, and a role
/// held only through another is not one the user may describe.
#[test]
fn stored_roles_and_inheritance() {
    with_wt(|c| {
        seed(c);
        on(c, "app", doc! {"insert": "c", "documents": [{"_id": 1}]});
        assert_eq!(
            run(
                c,
                "app",
                doc! {"createRole": "root", "privileges": [privilege("app", "c", &["find"])], "roles": []}
            ),
            None
        );
        user(c, "approot", vec![held("root", "app")]);
        role(c, "collFind", vec![privilege(DB, "c1", &["find"])], vec![]);
        role(c, "inherits", vec![], vec![held("collFind", "admin")]);
        role(c, "inheritsRead", vec![], vec![held("read", DB)]);
        user(c, "heir", vec![held("inherits", "admin")]);
        user(c, "readheir", vec![held("inheritsRead", "admin")]);
        user(c, "reader", vec![held("read", DB)]);

        as_user(c, "approot");
        assert_eq!(run(c, "app", find("c")), None);
        assert_eq!(run(c, "app", find("d")), Some(13));
        assert_eq!(run(c, "admin", doc! {"serverStatus": 1}), Some(13));

        as_user(c, "heir");
        assert_eq!(run(c, DB, find("c1")), None);
        assert_eq!(run(c, DB, find("c2")), Some(13));

        as_user(c, "readheir");
        assert_eq!(run(c, DB, find("c2")), None);
        assert_eq!(
            run(c, DB, doc! {"insert": "c1", "documents": [{"_id": 9}]}),
            Some(13)
        );
        assert_eq!(run(c, DB, doc! {"rolesInfo": "read"}), Some(13));

        as_user(c, "reader");
        assert_eq!(run(c, DB, doc! {"rolesInfo": "read"}), None);
        assert_eq!(run(c, DB, doc! {"rolesInfo": 1}), Some(13));
    });
}

/// An aggregation needs what its stages need: `$out` writes, `$merge`
/// updates, the `$lookup` family reads the foreign collection, `$collStats`
/// and `$indexStats` read statistics, `$currentOp` needs the cluster only
/// for other users' operations; `explain` needs what it explains (users
/// `c_outOnly`, `c_dbFind`, `c_collStatsOnly`, `c_clusterInprog`, `none`).
#[test]
fn pipelines_need_what_their_stages_need() {
    with_wt(|c| {
        seed(c);
        role(
            c,
            "outOnly",
            vec![
                privilege(DB, "c1", &["find"]),
                privilege(DB, "out", &["insert", "remove"]),
            ],
            vec![],
        );
        role(c, "dbFind", vec![privilege(DB, "", &["find"])], vec![]);
        role(
            c,
            "statsOnly",
            vec![privilege(
                DB,
                "c1",
                &["collStats", "indexStats", "listIndexes"],
            )],
            vec![],
        );
        role(c, "inprog", vec![cluster(&["inprog"])], vec![]);
        user(c, "out", vec![held("outOnly", "admin")]);
        user(c, "db", vec![held("dbFind", "admin")]);
        user(c, "stats", vec![held("statsOnly", "admin")]);
        user(c, "ops", vec![held("inprog", "admin")]);
        user(c, "none", vec![]);
        let agg = |coll: &str, pipeline: Vec<Document>| doc! {"aggregate": coll, "pipeline": pipeline, "cursor": {}};

        as_user(c, "out");
        assert_eq!(run(c, DB, agg("c1", vec![])), None);
        assert_eq!(run(c, DB, agg("c1", vec![doc! {"$out": "out"}])), None);
        assert_eq!(
            run(c, DB, agg("c1", vec![doc! {"$merge": {"into": "out"}}])),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                agg(
                    "c1",
                    vec![
                        doc! {"$lookup": {"from": "c2", "localField": "a", "foreignField": "a", "as": "j"}}
                    ]
                )
            ),
            Some(13)
        );
        assert_eq!(
            run(c, DB, agg("c1", vec![doc! {"$unionWith": "c2"}])),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"explain": agg("c1", vec![doc! {"$out": "out"}])}
            ),
            None
        );
        assert_eq!(run(c, DB, doc! {"explain": find("c2")}), Some(13));
        assert_eq!(
            run(
                c,
                DB,
                doc! {"mapReduce": "c1", "map": Bson::JavaScriptCode("function() { emit(this.a, 1); }".into()), "reduce": Bson::JavaScriptCode("function(k, v) { return Array.sum(v); }".into()), "out": {"merge": "out"}}
            ),
            Some(13)
        );

        as_user(c, "db");
        assert_eq!(
            run(
                c,
                DB,
                agg(
                    "c1",
                    vec![
                        doc! {"$lookup": {"from": "c2", "localField": "a", "foreignField": "a", "as": "j"}}
                    ]
                )
            ),
            None
        );
        assert_eq!(run(c, DB, agg("c1", vec![doc! {"$unionWith": "c2"}])), None);
        assert_eq!(
            run(c, DB, agg("c1", vec![doc! {"$collStats": {"count": {}}}])),
            Some(13)
        );

        as_user(c, "stats");
        assert_eq!(
            run(c, DB, agg("c1", vec![doc! {"$collStats": {"count": {}}}])),
            None
        );
        assert_eq!(run(c, DB, agg("c1", vec![doc! {"$indexStats": {}}])), None);
        assert_eq!(run(c, DB, agg("c1", vec![])), Some(13));

        as_user(c, "none");
        assert_eq!(
            run(
                c,
                "admin",
                doc! {"aggregate": 1, "pipeline": [{"$currentOp": {}}], "cursor": {}}
            ),
            None
        );
        assert_eq!(
            run(
                c,
                "admin",
                doc! {"aggregate": 1, "pipeline": [{"$currentOp": {"allUsers": true}}], "cursor": {}}
            ),
            Some(13)
        );
        assert_eq!(run(c, "admin", doc! {"currentOp": 1}), Some(13));
        assert_eq!(
            run(c, "admin", doc! {"currentOp": 1, "$ownOps": true}),
            None
        );

        as_user(c, "ops");
        assert_eq!(
            run(
                c,
                "admin",
                doc! {"aggregate": 1, "pipeline": [{"$currentOp": {"allUsers": true}}], "cursor": {}}
            ),
            None
        );
        assert_eq!(run(c, "admin", doc! {"currentOp": 1}), None);
    });
}

/// A user reads and kills its own cursors; another user's are refused, and
/// killed only with `killAnyCursor` on the cluster (users `c_collFind`,
/// `c_collKill`, `c_clusterKill`). An unknown cursor is reported, not
/// refused.
#[test]
fn cursors_belong_to_the_user_that_opened_them() {
    with_wt(|c| {
        seed(c);
        role(c, "collFind", vec![privilege(DB, "c1", &["find"])], vec![]);
        role(
            c,
            "collKill",
            vec![privilege(DB, "c1", &["find", "killCursors"])],
            vec![],
        );
        role(c, "clusterKill", vec![cluster(&["killAnyCursor"])], vec![]);
        user(c, "a", vec![held("collFind", "admin")]);
        user(c, "b", vec![held("collKill", "admin")]);
        user(c, "k", vec![held("clusterKill", "admin")]);
        user(c, "none", vec![]);
        let open = |c: &mut CommandContext| {
            on(c, DB, doc! {"find": "c1", "batchSize": 1})
                .get_document("cursor")
                .unwrap()
                .get_i64("id")
                .unwrap()
        };

        as_user(c, "a");
        let own = open(c);
        assert_eq!(run(c, DB, doc! {"getMore": own, "collection": "c1"}), None);
        let own = open(c);
        let killed = on(c, DB, doc! {"killCursors": "c1", "cursors": [own]});
        assert_eq!(killed.get_array("cursorsKilled").unwrap().len(), 1);
        let theirs = open(c);

        as_user(c, "b");
        assert_eq!(
            run(c, DB, doc! {"getMore": theirs, "collection": "c1"}),
            Some(13)
        );
        assert_eq!(
            run(c, DB, doc! {"killCursors": "c1", "cursors": [theirs]}),
            Some(13)
        );

        as_user(c, "none");
        let unknown = on(
            c,
            DB,
            doc! {"killCursors": "c1", "cursors": [123456789_i64]},
        );
        assert_eq!(code(&unknown), None);
        assert_eq!(unknown.get_array("cursorsNotFound").unwrap().len(), 1);
        assert_eq!(
            run(c, DB, doc! {"getMore": 123456789_i64, "collection": "c1"}),
            Some(43)
        );

        as_user(c, "k");
        let killed = on(c, DB, doc! {"killCursors": "c1", "cursors": [theirs]});
        assert_eq!(killed.get_array("cursorsKilled").unwrap().len(), 1);
    });
}

/// `listDatabases` and `listCollections` with `authorizedCollections` list
/// what the user may touch rather than refusing (users `db_read`,
/// `adm_read`, `c_clusterStatus`, `c_everyFind`, `c_nameFind`, `c_collFind`,
/// `c_collInsertOnly`, `none`).
#[test]
fn listings_are_filtered_by_privilege() {
    with_wt(|c| {
        seed(c);
        role(
            c,
            "clusterStatus",
            vec![cluster(&["serverStatus", "listDatabases"])],
            vec![],
        );
        role(c, "everyFind", vec![privilege("", "", &["find"])], vec![]);
        role(c, "nameFind", vec![privilege("", "c1", &["find"])], vec![]);
        role(c, "collFind", vec![privilege(DB, "c1", &["find"])], vec![]);
        role(
            c,
            "collInsertOnly",
            vec![privilege(DB, "made", &["insert"])],
            vec![],
        );
        user(c, "reader", vec![held("read", DB)]);
        user(c, "admreader", vec![held("read", "admin")]);
        user(c, "status", vec![held("clusterStatus", "admin")]);
        user(c, "every", vec![held("everyFind", "admin")]);
        user(c, "name", vec![held("nameFind", "admin")]);
        user(c, "coll", vec![held("collFind", "admin")]);
        user(c, "made", vec![held("collInsertOnly", "admin")]);
        user(c, "none", vec![]);
        let list = doc! {"listDatabases": 1, "nameOnly": true};
        let authorized = doc! {"listDatabases": 1, "nameOnly": true, "authorizedDatabases": true};
        let name_only =
            doc! {"listCollections": 1, "nameOnly": true, "authorizedCollections": true};

        as_user(c, "reader");
        assert_eq!(db_names(&on(c, "admin", list.clone())), ["ae"]);
        assert_eq!(names(&on(c, DB, name_only.clone())), ["c1", "c2"]);
        as_user(c, "admreader");
        assert_eq!(db_names(&on(c, "admin", list.clone())), ["admin"]);
        assert_eq!(run(c, DB, name_only.clone()), Some(13));
        as_user(c, "status");
        assert_eq!(
            db_names(&on(c, "admin", list.clone())),
            ["admin", "ae", "ae2", "config", "local"]
        );
        assert!(db_names(&on(c, "admin", authorized.clone())).is_empty());
        as_user(c, "every");
        assert_eq!(
            db_names(&on(c, "admin", list.clone())),
            ["admin", "ae", "ae2"]
        );
        as_user(c, "name");
        assert_eq!(
            db_names(&on(c, "admin", list.clone())),
            ["admin", "ae", "ae2", "config", "local"]
        );
        as_user(c, "coll");
        assert_eq!(names(&on(c, DB, name_only.clone())), ["c1"]);
        assert_eq!(
            run(
                c,
                DB,
                doc! {"listCollections": 1, "authorizedCollections": true}
            ),
            Some(13)
        );
        as_user(c, "made");
        assert!(names(&on(c, DB, name_only.clone())).is_empty());
        assert_eq!(db_names(&on(c, "admin", list.clone())), ["ae"]);
        as_user(c, "none");
        assert!(db_names(&on(c, "admin", list.clone())).is_empty());
        assert_eq!(run(c, DB, name_only), Some(13));
    });
}

/// User and role commands: on the database for its users and roles, on
/// each role's database to grant or revoke it, on the cluster for every
/// database; a user may view itself, and changes its own password and
/// custom data with the `changeOwn*` actions; changing anyone's roles takes
/// `revokeRole` on every database (users `db_userAdmin`, `c_ownPwd`,
/// `c_anyNormalRevoke`, `c_grantAdmin`, `none`).
#[test]
fn user_and_role_commands() {
    with_wt(|c| {
        seed(c);
        role(
            c,
            "ownPwd",
            vec![privilege(
                "admin",
                "",
                &["changeOwnPassword", "changeOwnCustomData"],
            )],
            vec![],
        );
        role(
            c,
            "anyNormalRevoke",
            vec![
                privilege("", "", &["revokeRole"]),
                privilege(DB, "", &["grantRole"]),
            ],
            vec![],
        );
        role(
            c,
            "grantAdmin",
            vec![
                privilege("admin", "", &["grantRole"]),
                privilege(DB, "", &["createRole", "grantRole"]),
            ],
            vec![],
        );
        user(c, "ua", vec![held("userAdmin", DB)]);
        user(c, "own", vec![held("ownPwd", "admin")]);
        user(c, "revoker", vec![held("anyNormalRevoke", "admin")]);
        user(c, "granter", vec![held("grantAdmin", "admin")]);
        user(c, "none", vec![]);
        let fresh = |c: &mut CommandContext| {
            as_nobody(c);
            on(c, DB, doc! {"dropUser": "victim"});
            on(c, DB, doc! {"dropRole": "vrole"});
            on(
                c,
                DB,
                doc! {"createUser": "victim", "pwd": "x", "roles": []},
            );
            on(
                c,
                DB,
                doc! {"createRole": "vrole", "privileges": [], "roles": []},
            );
        };

        fresh(c);
        as_user(c, "ua");
        assert_eq!(run(c, DB, doc! {"usersInfo": 1}), None);
        assert_eq!(run(c, DB, doc! {"usersInfo": "victim"}), None);
        assert_eq!(
            run(c, "admin", doc! {"usersInfo": {"forAllDBs": true}}),
            Some(13)
        );
        assert_eq!(run(c, DB, doc! {"rolesInfo": 1}), None);
        assert_eq!(
            run(c, DB, doc! {"rolesInfo": {"role": "read", "db": DB2}}),
            Some(13)
        );
        assert_eq!(run(c, DB, doc! {"updateUser": "victim", "pwd": "y"}), None);
        assert_eq!(
            run(c, DB, doc! {"updateUser": "victim", "customData": {"k": 1}}),
            None
        );
        assert_eq!(
            run(c, DB, doc! {"updateUser": "victim", "roles": ["read"]}),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"grantRolesToUser": "victim", "roles": ["read"]}
            ),
            None
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"grantRolesToUser": "victim", "roles": [held("read", DB2)]}
            ),
            Some(13)
        );
        assert_eq!(
            run(c, DB, doc! {"updateRole": "vrole", "roles": ["read"]}),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"grantPrivilegesToRole": "vrole", "privileges": [privilege(DB, "c1", &["find"])]}
            ),
            None
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"grantPrivilegesToRole": "vrole", "privileges": [cluster(&["serverStatus"])]}
            ),
            Some(13)
        );
        assert_eq!(
            run(c, "ae_scratch", doc! {"dropAllUsersFromDatabase": 1}),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"createUser": "w", "pwd": "x", "roles": [held("read", DB2)]}
            ),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"createUser": "w", "pwd": "x", "roles": ["read"]}
            ),
            None
        );
        assert_eq!(run(c, DB, doc! {"dropUser": "w"}), None);
        assert_eq!(run(c, DB, doc! {"dropRole": "vrole"}), None);

        fresh(c);
        as_user(c, "none");
        assert_eq!(run(c, "admin", doc! {"usersInfo": "none"}), None);
        assert_eq!(
            run(
                c,
                "admin",
                doc! {"usersInfo": "none", "showPrivileges": true}
            ),
            None
        );
        assert_eq!(run(c, "admin", doc! {"usersInfo": "own"}), Some(13));
        assert_eq!(
            run(
                c,
                "admin",
                doc! {"updateUser": "none", "customData": {"k": 1}}
            ),
            Some(13)
        );
        assert_eq!(
            run(c, "admin", doc! {"updateUser": "none", "pwd": "pw"}),
            Some(13)
        );

        as_user(c, "own");
        assert_eq!(
            run(
                c,
                "admin",
                doc! {"updateUser": "own", "customData": {"k": 1}}
            ),
            None
        );
        assert_eq!(
            run(c, "admin", doc! {"updateUser": "own", "pwd": "pw"}),
            None
        );
        assert_eq!(
            run(c, "admin", doc! {"updateUser": "none", "pwd": "pw"}),
            Some(13)
        );

        as_user(c, "revoker");
        assert_eq!(
            run(c, DB, doc! {"updateUser": "victim", "roles": ["read"]}),
            None
        );
        assert_eq!(
            run(c, DB, doc! {"updateRole": "vrole", "roles": ["read"]}),
            None
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"revokeRolesFromUser": "victim", "roles": ["read"]}
            ),
            None
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"createRole": "x", "privileges": [], "roles": []}
            ),
            Some(13)
        );

        as_user(c, "granter");
        assert_eq!(
            run(
                c,
                DB,
                doc! {"createRole": "x", "privileges": [privilege(DB, "c1", &["find"])], "roles": []}
            ),
            None
        );
        // Allowed, then refused by the command itself: a role of `ae`
        // cannot hold a cluster privilege.
        assert_ne!(
            run(
                c,
                DB,
                doc! {"createRole": "y", "privileges": [cluster(&["serverStatus"])], "roles": []}
            ),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"createRole": "z", "privileges": [privilege(DB2, "", &["find"])], "roles": []}
            ),
            Some(13)
        );
        assert_eq!(
            run(
                c,
                DB,
                doc! {"revokeRolesFromUser": "victim", "roles": ["read"]}
            ),
            Some(13)
        );
    });
}

/// Renaming within a database takes `renameCollectionSameDB` on the
/// database, or the collection privileges of a move; dropping the target
/// takes `dropCollection` on it either way (users `c_dbRename`,
/// `c_collRename`, `adm_readWriteAnyDatabase`, `db_readWrite`).
#[test]
fn rename_collection_rules() {
    with_wt(|c| {
        role(
            c,
            "dbRename",
            vec![privilege(DB, "", &["renameCollectionSameDB"])],
            vec![],
        );
        role(
            c,
            "collRename",
            vec![
                privilege(DB, "tmp", &["find", "dropCollection"]),
                privilege(DB, "renamed", &["insert", "createIndex", "dropCollection"]),
            ],
            vec![],
        );
        user(c, "db", vec![held("dbRename", "admin")]);
        user(c, "coll", vec![held("collRename", "admin")]);
        user(c, "rwany", vec![held("readWriteAnyDatabase", "admin")]);
        user(c, "rw", vec![held("readWrite", DB)]);
        let fresh = |c: &mut CommandContext| {
            as_nobody(c);
            on(c, DB, doc! {"drop": "tmp"});
            on(c, DB, doc! {"drop": "renamed"});
            on(c, DB2, doc! {"drop": "renamed"});
            on(c, DB, doc! {"insert": "tmp", "documents": [{"_id": 1}]});
            on(c, DB, doc! {"insert": "renamed", "documents": [{"_id": 1}]});
        };
        let same = doc! {"renameCollection": "ae.tmp", "to": "ae.renamed"};
        let drop_target =
            doc! {"renameCollection": "ae.tmp", "to": "ae.renamed", "dropTarget": true};
        let across = doc! {"renameCollection": "ae.tmp", "to": "ae2.renamed"};

        fresh(c);
        as_user(c, "db");
        assert_eq!(run(c, "admin", drop_target.clone()), Some(13));
        as_nobody(c);
        on(c, DB, doc! {"drop": "renamed"});
        as_user(c, "db");
        assert_eq!(run(c, "admin", same.clone()), None);
        fresh(c);
        as_user(c, "coll");
        assert_eq!(run(c, "admin", drop_target.clone()), None);
        fresh(c);
        as_user(c, "rw");
        assert_eq!(run(c, "admin", across.clone()), Some(13));
        assert_eq!(run(c, "admin", drop_target.clone()), None);
        fresh(c);
        as_user(c, "rwany");
        assert_eq!(run(c, "admin", across), None);
    });
}

/// Reading the profiling level takes `find` on `system.profile`; setting
/// it, or reading with `slowms`, takes `enableProfiler` on the database
/// (users `c_sysProfileFind`, `c_dbProfiler`).
#[test]
fn profile_rules() {
    with_wt(|c| {
        role(
            c,
            "sysProfileFind",
            vec![privilege(DB, "system.profile", &["find"])],
            vec![],
        );
        role(
            c,
            "dbProfiler",
            vec![privilege(DB, "", &["enableProfiler"])],
            vec![],
        );
        user(c, "readsit", vec![held("sysProfileFind", "admin")]);
        user(c, "setsit", vec![held("dbProfiler", "admin")]);
        as_user(c, "readsit");
        assert_eq!(run(c, DB, doc! {"profile": -1}), None);
        assert_eq!(run(c, DB, doc! {"profile": -1, "slowms": 100}), Some(13));
        assert_eq!(run(c, DB, doc! {"profile": 0}), Some(13));
        as_user(c, "setsit");
        assert_eq!(run(c, DB, doc! {"profile": -1}), None);
        assert_eq!(run(c, DB, doc! {"profile": -1, "slowms": 100}), None);
        assert_eq!(run(c, DB, doc! {"profile": 0}), None);
    });
}

/// What any authenticated user runs, and what a user with no role does not:
/// the handshake, its own session, `configureFailPoint` (gated by test
/// commands, not a role), `listDatabases`; not `replSetGetStatus`, `getLog`
/// or `getParameter`. An unknown command is still `CommandNotFound`, and an
/// unauthenticated connection is refused before any of this.
#[test]
fn open_and_closed_by_default() {
    with_wt(|c| {
        seed(c);
        user(c, "none", vec![]);
        c.failpoints = Some(Arc::new(FailPointRegistry::new()));
        as_user(c, "none");
        for command in [
            doc! {"ping": 1},
            doc! {"hello": 1},
            doc! {"buildInfo": 1},
            doc! {"connectionStatus": 1},
            doc! {"listCommands": 1},
            doc! {"whatsmyuri": 1},
            doc! {"startSession": 1},
            doc! {"killSessions": []},
            doc! {"endSessions": []},
            doc! {"refreshSessions": []},
            doc! {"listDatabases": 1, "nameOnly": true},
            doc! {"configureFailPoint": "failCommand", "mode": "off"},
            doc! {"aggregate": 1, "pipeline": [{"$documents": [{"a": 1}]}], "cursor": {}},
            doc! {"aggregate": 1, "pipeline": [{"$listLocalSessions": {}}], "cursor": {}},
        ] {
            assert_eq!(run(c, "admin", command.clone()), None, "{command}");
        }
        for command in [
            doc! {"serverStatus": 1},
            doc! {"replSetGetStatus": 1},
            doc! {"replSetGetConfig": 1},
            doc! {"getLog": "global"},
            doc! {"getParameter": 1, "logLevel": 1},
            doc! {"setParameter": 1, "logLevel": 0},
            doc! {"getCmdLineOpts": 1},
            doc! {"hostInfo": 1},
            doc! {"top": 1},
            doc! {"fsync": 1},
            doc! {"fsyncUnlock": 1},
            doc! {"killOp": 1, "op": 987654321},
            doc! {"killAllSessions": []},
            doc! {"getDefaultRWConcern": 1},
            doc! {"shardingState": 1},
            doc! {"replSetStepDown": 60},
            doc! {"aggregate": 1, "pipeline": [{"$listLocalSessions": {"allUsers": true}}], "cursor": {}},
            doc! {"secantusAdmin.pruneTtl": 1},
        ] {
            assert_eq!(run(c, "admin", command.clone()), Some(13), "{command}");
        }
        assert_eq!(run(c, DB, doc! {"noSuchCommand": 1}), Some(59));
        assert_eq!(run(c, DB, find("c1")), Some(13));

        c.conn_auth = Some(Arc::new(Mutex::new(ConnectionAuth::new())));
        assert_eq!(run(c, DB, doc! {"ping": 1}), None);
        let refused = on(c, DB, find("c1"));
        assert_eq!(code(&refused), Some(13));
        assert_eq!(
            refused.get_str("errmsg").unwrap(),
            "Command find requires authentication"
        );
        assert_eq!(run(c, DB, doc! {"noSuchCommand": 1}), Some(59));
    });
}
