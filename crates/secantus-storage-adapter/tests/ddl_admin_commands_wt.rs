//! The DDL and admin commands over real WiredTiger: what `create`,
//! `listCollections`, `collMod`, `renameCollection`, `drop`, `dropDatabase`
//! and the stats commands accept, refuse and report. Every expectation is
//! what mongod 8.2.11 answered to the same command
//! (`tools/probes/ddl_admin_commands.py`).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_admin, dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn refusal(reply: &Document) -> (i32, String) {
    assert_eq!(reply.get_f64("ok"), Ok(0.0), "{reply}");
    (
        reply.get_i32("code").unwrap(),
        reply.get_str("errmsg").unwrap().to_string(),
    )
}

fn ok(reply: &Document) {
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{reply}");
}

fn names(c: &mut CommandContext) -> Vec<String> {
    let reply = dispatch_full(&doc! { "listCollections": 1, "nameOnly": true }, c);
    let mut out: Vec<String> = reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|e| {
            e.as_document()
                .unwrap()
                .get_str("name")
                .unwrap()
                .to_string()
        })
        .collect();
    out.sort();
    out
}

fn entry(c: &mut CommandContext, name: &str) -> Document {
    let reply = dispatch_full(
        &doc! { "listCollections": 1, "filter": { "name": name } },
        c,
    );
    let batch = reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap();
    batch
        .first()
        .unwrap_or_else(|| panic!("{name} is not listed"))
        .as_document()
        .unwrap()
        .clone()
}

fn on_db(c: &mut CommandContext, db: &str, command: Document) -> Document {
    let saved = std::mem::replace(&mut c.db_name, db.to_string());
    let reply = dispatch_full(&command, c);
    c.db_name = saved;
    reply
}

/// A database name mongod cannot hold is refused by every command. Each of
/// these used to be accepted, and a write created the database.
#[test]
fn an_invalid_database_name_is_refused() {
    with_wt(|c| {
        let insert = doc! { "insert": "c", "documents": [{ "_id": 1 }] };
        let cases = [
            ("a.b", "'.' is an invalid character in a db name: a.b"),
            ("", "Invalid namespace specified: .c"),
            ("a b", "Invalid namespace specified: a b.c"),
            ("a/b", "Invalid namespace specified: a/b.c"),
            ("$x", "Invalid namespace: $x.c"),
        ];
        for (db, message) in cases {
            let reply = on_db(c, db, insert.clone());
            assert_eq!(refusal(&reply), (73, message.to_string()), "{db:?}");
        }
        let long = "x".repeat(64);
        assert_eq!(
            refusal(&on_db(c, &long, insert.clone())),
            (
                73,
                "db name must be at most 63 characters, found: 64".to_string()
            )
        );
        // A command that names no collection reports the database.
        assert_eq!(
            refusal(&on_db(c, "a b", doc! { "listCollections": 1 })),
            (73, "Invalid database name: 'a b'".to_string())
        );
        // A `$` is refused only for a write: a read of nothing is still nothing.
        ok(&on_db(c, "$x", doc! { "find": "c" }));
        let listed = dispatch_admin(&doc! { "listDatabases": 1, "nameOnly": true }, c);
        let found: Vec<&str> = listed
            .get_array("databases")
            .unwrap()
            .iter()
            .filter_map(|d| d.as_document()?.get_str("name").ok())
            .collect();
        for (db, _) in cases {
            assert!(!found.contains(&db), "{db:?} was created: {found:?}");
        }
    });
}

/// `create` parses its options before it creates anything.
#[test]
fn create_refuses_options_it_cannot_hold() {
    with_wt(|c| {
        let cases: Vec<(Document, i32, &str)> = vec![
            (
                doc! { "validationLevel": 1 },
                14,
                "BSON field 'create.validationLevel' is the wrong type 'int', expected type 'string'",
            ),
            (
                doc! { "changeStreamPreAndPostImages": {} },
                40414,
                "BSON field 'create.changeStreamPreAndPostImages.enabled' is missing but a required field",
            ),
            (
                doc! { "changeStreamPreAndPostImages": { "enabled": 1 } },
                14,
                "BSON field 'create.changeStreamPreAndPostImages.enabled' is the wrong type 'int', expected type 'bool'",
            ),
            (
                doc! { "autoIndexId": false },
                40415,
                "BSON field 'create.autoIndexId' is an unknown field.",
            ),
            (
                doc! { "storageEngine": { "nope": {} } },
                72,
                "nope is not a registered storage engine for this server",
            ),
            (
                doc! { "idIndex": { "key": { "a": 1 }, "name": "_id_" } },
                2,
                "The field 'key' for an _id index must be {_id: 1}, but got { a: 1 }",
            ),
            (
                doc! { "clusteredIndex": { "key": { "_id": 1 } } },
                40414,
                "BSON field 'create.clusteredIndex.unique' is missing but a required field",
            ),
            (
                doc! { "viewOn": "c", "pipeline": [1] },
                14,
                "BSON field 'create.pipeline.0' is the wrong type 'int', expected type 'object'",
            ),
        ];
        for (options, code, message) in cases {
            let mut command = doc! { "create": "n" };
            command.extend(options.clone());
            let reply = dispatch_full(&command, c);
            assert_eq!(refusal(&reply), (code, message.to_string()), "{options}");
        }
        let long = dispatch_full(&doc! { "create": "x".repeat(256) }, c);
        assert!(
            refusal(&long)
                .1
                .starts_with("Fully qualified namespace is too long."),
            "{long}"
        );
        assert!(names(c).is_empty(), "something was created: {:?}", names(c));
    });
}

/// What `listCollections` shows: mongod's fields, its option order, and no
/// option that is at its default.
#[test]
fn list_collections_shows_what_mongod_shows() {
    with_wt(|c| {
        ok(&dispatch_full(
            &doc! { "create": "k", "validator": { "a": 1 }, "capped": true, "size": 4096 },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "create": "p", "changeStreamPreAndPostImages": { "enabled": false },
            "collation": Bson::Null },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "create": "v", "viewOn": "k", "pipeline": [] },
            c,
        ));
        let k = entry(c, "k");
        // No `ns` on the `_id` index; options in mongod's order.
        assert_eq!(
            k.get_document("idIndex").unwrap(),
            &doc! { "v": 2, "key": { "_id": 1 }, "name": "_id_" }
        );
        let keys: Vec<&str> = k
            .get_document("options")
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["capped", "size", "validator"]);
        assert_eq!(entry(c, "p").get_document("options").unwrap(), &doc! {});
        // A view has no uuid.
        assert_eq!(
            entry(c, "v").get_document("info").unwrap(),
            &doc! { "readOnly": true }
        );
        // With `nameOnly` the filter sees the name and the type, nothing else.
        let capped = dispatch_full(
            &doc! { "listCollections": 1, "nameOnly": true, "filter": { "options.capped": true } },
            c,
        );
        let batch = capped
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert!(batch.is_empty(), "{capped}");
        let unknown = dispatch_full(
            &doc! { "listCollections": 1, "filter": { "name": { "$nope": 1 } } },
            c,
        );
        assert_eq!(
            refusal(&unknown),
            (2, "unknown operator: $nope".to_string())
        );
        let typed = dispatch_full(
            &doc! { "listCollections": 1, "authorizedCollections": 1 },
            c,
        );
        assert_eq!(refusal(&typed).0, 14);
    });
}

/// A collection or a view already in the way is described as mongod
/// describes it.
#[test]
fn create_describes_what_is_in_the_way() {
    with_wt(|c| {
        ok(&dispatch_full(
            &doc! { "create": "c", "capped": true, "size": 4096 },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "create": "v", "viewOn": "c", "pipeline": [] },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "create": "v", "viewOn": "c", "pipeline": [] },
            c,
        ));
        let other = dispatch_full(
            &doc! { "create": "v", "viewOn": "c", "pipeline": [{ "$match": {} }] },
            c,
        );
        assert_eq!(
            refusal(&other),
            (
                48,
                "namespace t.v already exists, but with pipeline {} rather than \
                 { 0: { $match: {} } }"
                    .to_string()
            )
        );
        let plain = dispatch_full(&doc! { "create": "v" }, c);
        assert_eq!(
            refusal(&plain).1,
            "namespace t.v already exists, but is a view on t.c rather than t"
        );
        let over = dispatch_full(&doc! { "create": "c", "viewOn": "c", "pipeline": [] }, c);
        let (code, message) = refusal(&over);
        assert_eq!(code, 48);
        assert!(
            message.contains("already exists, but with different options"),
            "{message}"
        );
    });
}

/// `collMod` checks what it is given, and its reply ends with `ok`.
#[test]
fn coll_mod_checks_its_options() {
    with_wt(|c| {
        ok(&dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1, "a": 1 }] },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "createIndexes": "c", "indexes": [{ "key": { "a": 1 }, "name": "a_1" }] },
            c,
        ));
        let sent = |c: &mut CommandContext, options: Document| {
            let mut command = doc! { "collMod": "c" };
            command.extend(options);
            dispatch_full(&command, c)
        };
        assert_eq!(
            refusal(&dispatch_full(&doc! { "collMod": "nope" }, c)),
            (26, "ns does not exist".to_string())
        );
        assert_eq!(refusal(&sent(c, doc! { "validationAction": 1 })).0, 14);
        assert_eq!(
            refusal(&sent(c, doc! { "dryRun": true, "validator": { "a": 1 } })),
            (
                72,
                "dry run mode requires an valid index modification.".to_string()
            )
        );
        // The dry run changed nothing.
        assert_eq!(entry(c, "c").get_document("options").unwrap(), &doc! {});
        assert_eq!(
            refusal(&sent(c, doc! { "expireAfterSeconds": 10 })),
            (
                72,
                "'expireAfterSeconds' option is only supported on collections clustered by _id"
                    .to_string()
            )
        );
        assert_eq!(
            refusal(&sent(
                c,
                doc! { "index": { "name": "a_1", "keyPattern": { "a": 1 }, "hidden": true } }
            )),
            (72, "Cannot specify both key pattern and name.".to_string())
        );
        // A null is an option not given.
        ok(&sent(c, doc! { "validationLevel": "off" }));
        ok(&sent(c, doc! { "validationLevel": Bson::Null }));
        let options = entry(c, "c").get_document("options").unwrap().clone();
        assert_eq!(options.get_str("validationLevel"), Ok("off"));
        let hidden = sent(c, doc! { "index": { "name": "a_1", "hidden": true } });
        let keys: Vec<&str> = hidden.keys().map(String::as_str).collect();
        assert_eq!(keys, ["hidden_old", "hidden_new", "ok"]);
    });
}

/// Converting an index to unique takes two steps, and says what it changed.
#[test]
fn an_index_becomes_unique_in_two_steps() {
    with_wt(|c| {
        ok(&dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1, "a": 1 }, { "_id": 2, "a": 2 }] },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "createIndexes": "c", "indexes": [{ "key": { "a": 1 }, "name": "a_1" }] },
            c,
        ));
        let index = |c: &mut CommandContext, change: Document| {
            let mut spec = doc! { "name": "a_1" };
            spec.extend(change);
            dispatch_full(&doc! { "collMod": "c", "index": spec }, c)
        };
        assert_eq!(
            refusal(&index(c, doc! { "unique": true })),
            (
                72,
                "Cannot make index unique with 'prepareUnique=false'. Run collMod to set it first."
                    .to_string()
            )
        );
        assert_eq!(
            index(c, doc! { "prepareUnique": true }),
            doc! { "prepareUnique_old": false, "prepareUnique_new": true, "ok": 1.0 }
        );
        // Armed: a new duplicate is refused before the conversion.
        let dup = dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 3, "a": 1 }] },
            c,
        );
        assert_eq!(dup.get_i32("n"), Ok(0), "{dup}");
        assert_eq!(
            index(c, doc! { "unique": true }),
            doc! { "unique_new": true, "ok": 1.0 }
        );
        let listed = dispatch_full(&doc! { "listIndexes": "c" }, c);
        let specs = listed
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        let a = specs[1].as_document().unwrap();
        assert_eq!(a.get_bool("unique"), Ok(true));
        assert!(!a.contains_key("prepareUnique"), "{a}");
        assert_eq!(
            refusal(&index(c, doc! { "unique": false })),
            (2, "Cannot make index non-unique".to_string())
        );
    });
}

/// `renameCollection` checks both namespaces before it looks for either.
#[test]
fn rename_checks_its_namespaces() {
    with_wt(|c| {
        ok(&dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1 }] },
            c,
        ));
        ok(&dispatch_full(&doc! { "create": "d" }, c));
        let rename = |c: &mut CommandContext, src: &str, to: &str| {
            dispatch_admin(&doc! { "renameCollection": src, "to": to }, c)
        };
        let cases = [
            ("", "t.x", 73, "Invalid source namespace: "),
            ("c", "t.x", 73, "Invalid source namespace: c"),
            ("t.", "t.x", 73, "Invalid source namespace: t"),
            ("t.c", "x", 73, "Invalid target namespace: x"),
            ("t.c", "t.c", 20, "Can't rename a collection to itself"),
            (
                "t.c",
                "t.system.x",
                20,
                "error with target namespace: Invalid system namespace: t.system.x",
            ),
            (
                "t.nope",
                "t.x",
                26,
                "Source collection t.nope does not exist",
            ),
            ("u.nope", "t.x", 26, "source namespace does not exist"),
            ("t.c", "t.d", 48, "target namespace exists"),
        ];
        for (src, to, code, message) in cases {
            let reply = rename(c, src, to);
            assert_eq!(
                refusal(&reply),
                (code, message.to_string()),
                "{src} -> {to}"
            );
        }
        let typed = dispatch_admin(&doc! { "renameCollection": 1, "to": "t.x" }, c);
        assert_eq!(refusal(&typed).0, 14);
        let extra = dispatch_admin(
            &doc! { "renameCollection": "t.c", "to": "t.x", "nope": 1 },
            c,
        );
        assert_eq!(refusal(&extra).0, 40415);
        assert_eq!(names(c), ["c", "d"]);
        ok(&rename(c, "t.c", "t.e"));
        assert_eq!(names(c), ["d", "e"]);
    });
}

/// `dropDatabase` takes `1` and nothing else. Anything else used to drop the
/// database.
#[test]
fn drop_database_takes_one() {
    with_wt(|c| {
        ok(&dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1 }] },
            c,
        ));
        let cases: Vec<(Bson, i32)> = vec![
            (Bson::String("x".into()), 14),
            (Bson::Null, 40414),
            (Bson::Int32(0), 5_255_100),
            (Bson::Double(2.5), 5_255_100),
            (Bson::Boolean(true), 14),
        ];
        for (value, code) in cases {
            let reply = dispatch_full(&doc! { "dropDatabase": value.clone() }, c);
            assert_eq!(refusal(&reply).0, code, "{value}");
        }
        assert_eq!(
            refusal(&dispatch_full(&doc! { "dropDatabase": 1, "nope": 1 }, c)).0,
            40415
        );
        assert_eq!(names(c), ["c"]);
        assert_eq!(
            refusal(&dispatch_full(&doc! { "drop": "c", "nope": 1 }, c)).0,
            40415
        );
        assert_eq!(names(c), ["c"]);
        // Dropping `system.views` drops every view.
        ok(&dispatch_full(
            &doc! { "create": "v", "viewOn": "c", "pipeline": [] },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "create": "w", "viewOn": "c", "pipeline": [] },
            c,
        ));
        assert_eq!(
            dispatch_full(&doc! { "drop": "system.views" }, c),
            doc! { "nIndexesWas": 1, "ns": "t.system.views", "ok": 1.0 }
        );
        assert_eq!(names(c), ["c"]);
        ok(&dispatch_full(&doc! { "dropDatabase": 1 }, c));
        assert!(names(c).is_empty());
    });
}

/// The stats commands report mongod's fields, in its order and number types.
#[test]
fn stats_have_mongods_shape() {
    with_wt(|c| {
        ok(&dispatch_full(
            &doc! { "insert": "c", "documents": [{ "_id": 1, "a": 1 }, { "_id": 2, "a": 2 }] },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "create": "k", "capped": true, "size": 4096, "max": 3 },
            c,
        ));
        ok(&dispatch_full(
            &doc! { "create": "v", "viewOn": "c", "pipeline": [] },
            c,
        ));
        let keys = |d: &Document| d.keys().cloned().collect::<Vec<_>>();
        let db = dispatch_full(&doc! { "dbStats": 1 }, c);
        assert_eq!(
            keys(&db),
            [
                "db",
                "collections",
                "views",
                "objects",
                "avgObjSize",
                "dataSize",
                "storageSize",
                "indexes",
                "indexSize",
                "totalSize",
                "scaleFactor",
                "fsUsedSize",
                "fsTotalSize",
                "ok"
            ]
        );
        // Two collections and `system.views`; the view itself is counted apart.
        assert_eq!(db.get_i64("collections"), Ok(3));
        assert_eq!(db.get_i64("views"), Ok(1));
        assert_eq!(db.get_i64("objects"), Ok(3));
        assert_eq!(db.get_i64("indexes"), Ok(3));
        // The filesystem holding the store, as mongod 8.2.11 reports it for
        // a database that exists; scaled like the other sizes.
        let used = db.get_f64("fsUsedSize").unwrap();
        let total = db.get_f64("fsTotalSize").unwrap();
        assert!(total > 0.0 && used > 0.0 && used <= total, "{db}");
        let kb = dispatch_full(&doc! { "dbStats": 1, "scale": 1024 }, c);
        assert_eq!(kb.get_f64("fsTotalSize").unwrap(), total / 1024.0);
        // ... and zeros for one that does not.
        c.db_name = "rm068_absent".to_string();
        let absent = dispatch_full(&doc! { "dbStats": 1 }, c);
        assert_eq!(absent.get_f64("fsUsedSize"), Ok(0.0));
        assert_eq!(absent.get_f64("fsTotalSize"), Ok(0.0));
        c.db_name = db.get_str("db").unwrap().to_string();
        assert_eq!(
            refusal(&dispatch_full(&doc! { "dbStats": 1, "scale": 0 }, c)),
            (
                2,
                "Scale factor must be greater than zero and less than or equal to 2^50".to_string()
            )
        );
        assert_eq!(
            refusal(&dispatch_full(&doc! { "dbStats": 1, "nope": 1 }, c)).0,
            40415
        );

        let coll = dispatch_full(&doc! { "collStats": "c" }, c);
        assert_eq!(
            keys(&coll),
            [
                "ns",
                "size",
                "count",
                "avgObjSize",
                "numOrphanDocs",
                "storageSize",
                "freeStorageSize",
                "capped",
                "nindexes",
                "indexBuilds",
                "totalIndexSize",
                "indexSizes",
                "totalSize",
                "scaleFactor",
                "ok"
            ]
        );
        assert_eq!(coll.get_i32("count"), Ok(2));
        // An empty capped collection still has its `_id` index.
        let capped = dispatch_full(&doc! { "collStats": "k" }, c);
        assert_eq!(capped.get_i32("nindexes"), Ok(1));
        assert_eq!(capped.get_i32("max"), Ok(3));
        assert_eq!(capped.get_i32("maxSize"), Ok(4096));
        assert!(!capped.contains_key("avgObjSize"), "{capped}");
        let missing = dispatch_full(&doc! { "collStats": "nope" }, c);
        assert_eq!(
            keys(&missing),
            [
                "ns",
                "size",
                "count",
                "numOrphanDocs",
                "storageSize",
                "totalSize",
                "nindexes",
                "totalIndexSize",
                "indexSizes",
                "scaleFactor",
                "ok"
            ]
        );
        assert_eq!(
            refusal(&dispatch_full(&doc! { "collStats": "v" }, c)).0,
            166
        );
        assert_eq!(
            refusal(&dispatch_full(&doc! { "collStats": "c", "scale": 0.5 }, c)),
            (
                2,
                "BSON field 'scale' value must be >= 1, actual value '0'".to_string()
            )
        );

        let size = dispatch_full(&doc! { "dataSize": "t.c" }, c);
        assert_eq!(size.get_i64("numObjects"), Ok(2));
        assert_eq!(size.get_bool("estimate"), Ok(false));
        assert_eq!(
            refusal(&dispatch_full(&doc! { "dataSize": "c" }, c)),
            (73, "Namespace cis not a valid collection name".to_string())
        );

        let listed = dispatch_admin(&doc! { "listDatabases": 1, "filter": { "name": "t" } }, c);
        assert_eq!(
            keys(&listed),
            ["databases", "totalSize", "totalSizeMb", "ok"]
        );
        let only = listed.get_array("databases").unwrap()[0]
            .as_document()
            .unwrap();
        assert_eq!(only.get_bool("empty"), Ok(false));
        let named = dispatch_admin(&doc! { "listDatabases": 1, "nameOnly": true }, c);
        assert_eq!(keys(&named), ["databases", "ok"]);
        assert_eq!(
            refusal(&dispatch_full(&doc! { "listDatabases": 1 }, c)),
            (
                13,
                "listDatabases may only be run against the admin database.".to_string()
            )
        );
        assert_eq!(
            refusal(&dispatch_admin(
                &doc! { "listDatabases": 1, "nameOnly": "x" },
                c
            ))
            .0,
            14
        );
    });
}

/// A fresh mongod 8.2.11 lists `config` because it sets up
/// `config.system.sessions` (with `lsidTTLIndex`) as it starts; the server
/// does the same at `bind`, and a second call leaves the collection be.
#[test]
fn sessions_collection_makes_config_a_database() {
    with_wt(|c| {
        let listed = |c: &mut CommandContext| -> Vec<String> {
            let reply = dispatch_admin(&doc! { "listDatabases": 1, "nameOnly": true }, c);
            reply
                .get_array("databases")
                .unwrap()
                .iter()
                .map(|d| {
                    d.as_document()
                        .unwrap()
                        .get_str("name")
                        .unwrap()
                        .to_string()
                })
                .collect()
        };
        assert!(!listed(c).contains(&"config".to_string()));
        for _ in 0..2 {
            secantus_commands::cluster_admin::record_sessions_collection(c.storage().unwrap())
                .unwrap();
        }
        assert!(listed(c).contains(&"config".to_string()));
        let full = dispatch_admin(&doc! { "listDatabases": 1 }, c);
        let config = full
            .get_array("databases")
            .unwrap()
            .iter()
            .map(|d| d.as_document().unwrap())
            .find(|d| d.get_str("name") == Ok("config"))
            .unwrap()
            .clone();
        assert_eq!(config.get_bool("empty"), Ok(false));
        c.db_name = "config".to_string();
        assert_eq!(names(c), ["system.sessions"]);
        let ix = dispatch_full(&doc! { "listIndexes": "system.sessions" }, c);
        let batch = ix
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .clone();
        assert_eq!(batch.len(), 2);
        assert_eq!(
            batch[1].as_document().unwrap(),
            &doc! {
                "v": 2,
                "key": { "lastUse": 1 },
                "name": "lsidTTLIndex",
                "expireAfterSeconds": 1800,
            }
        );
    });
}
