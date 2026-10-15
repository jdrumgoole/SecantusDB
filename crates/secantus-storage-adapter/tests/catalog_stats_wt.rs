//! `$listCatalog` and `$indexStats`' access counts, over real WiredTiger.
//! Every expectation is what mongod 8.2.11 answered (2026-10-15), except
//! where the catalog entry names this store's own tables (`ident`,
//! `idxIdent`), which Joe decided on 2026-10-15.

mod common;

use std::sync::Arc;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::{CommandContext, CursorRegistry, Storage as CmdStorage};
use secantus_storage::Storage as WtStorage;
use secantus_storage_adapter::StorageAdapter;

/// Run `cmd` on `db`.
fn on(db: &str, cmd: &Document, c: &mut CommandContext) -> Document {
    let previous = std::mem::replace(&mut c.db_name, db.into());
    let reply = dispatch_full(cmd, c);
    c.db_name = previous;
    reply
}

fn error(reply: &Document) -> (i32, String) {
    (
        reply.get_i32("code").unwrap_or_else(|_| panic!("{reply}")),
        reply.get_str("errmsg").unwrap_or_default().to_string(),
    )
}

fn batch(reply: &Document) -> Vec<Document> {
    reply
        .get_document("cursor")
        .and_then(|c| c.get_array("firstBatch"))
        .unwrap_or_else(|_| panic!("{reply}"))
        .iter()
        .filter_map(|d| d.as_document().cloned())
        .collect()
}

fn catalog(db: &str, coll: Bson, c: &mut CommandContext) -> Document {
    on(
        db,
        &doc! {"aggregate": coll, "pipeline": [{"$listCatalog": {}}], "cursor": {}},
        c,
    )
}

fn keys(d: &Document) -> Vec<&str> {
    d.keys().map(String::as_str).collect()
}

fn seed(c: &mut CommandContext) {
    on(
        "t",
        &doc! {"insert": "c", "documents": [{"_id": 1, "a": [1, 2], "b": 1}]},
        c,
    );
    on(
        "t",
        &doc! {"createIndexes": "c", "indexes": [
            {"key": {"a": 1}, "name": "a_1"},
            {"key": {"b": 1}, "name": "b_1"},
        ]},
        c,
    );
    on(
        "t",
        &doc! {"create": "v", "viewOn": "c", "pipeline": [{"$match": {"a": 1}}]},
        c,
    );
    on(
        "t",
        &doc! {"create": "ts", "timeseries": {"timeField": "t", "metaField": "m"}},
        c,
    );
}

/// Collectionless on `admin`: every collection, then every view, each in
/// mongod's shape, with this store's tables as `ident` / `idxIdent`.
#[test]
fn list_catalog_lists_collections_then_views() {
    with_wt(|c| {
        seed(c);
        let all = batch(&catalog("admin", Bson::Int32(1), c));
        let entry = |name: &str| {
            all.iter()
                .find(|e| e.get_str("db") == Ok("t") && e.get_str("name") == Ok(name))
                .cloned()
                .unwrap_or_else(|| panic!("no t.{name} in {all:?}"))
        };

        let coll = entry("c");
        assert_eq!(
            keys(&coll),
            ["db", "name", "type", "md", "idxIdent", "ident", "ns"]
        );
        assert_eq!(coll.get_str("type"), Ok("collection"));
        assert_eq!(coll.get_str("ns"), Ok("t.c"));
        let md = coll.get_document("md").unwrap();
        assert_eq!(keys(md), ["ns", "options", "indexes"]);
        assert!(matches!(
            md.get_document("options").unwrap().get("uuid"),
            Some(Bson::Binary(b)) if b.subtype == bson::spec::BinarySubtype::Uuid
        ));
        let indexes: Vec<(String, bool, Vec<&str>)> = md
            .get_array("indexes")
            .unwrap()
            .iter()
            .map(|ix| {
                let ix = ix.as_document().unwrap();
                assert_eq!(ix.get_bool("ready"), Ok(true));
                (
                    ix.get_document("spec")
                        .unwrap()
                        .get_str("name")
                        .unwrap()
                        .to_string(),
                    ix.get_bool("multikey").unwrap(),
                    keys(ix),
                )
            })
            .collect();
        assert_eq!(
            indexes,
            [
                ("_id_".to_string(), false, vec!["spec", "ready", "multikey"]),
                ("a_1".to_string(), true, vec!["spec", "ready", "multikey"]),
                ("b_1".to_string(), false, vec!["spec", "ready", "multikey"]),
            ]
        );
        assert_eq!(
            coll.get_document("idxIdent").unwrap(),
            &doc! {
                "_id_": "table:secantus_natural_seq",
                "a_1": "table:secantus_index_entries",
                "b_1": "table:secantus_index_entries",
            }
        );
        assert_eq!(
            coll.get_str("ident"),
            Ok(secantus_storage::collection_table("t", "c").as_str())
        );

        // The time-series collection's buckets are kept in its own table.
        let buckets = entry("system.buckets.ts");
        let md = buckets.get_document("md").unwrap();
        assert_eq!(
            keys(md),
            [
                "ns",
                "options",
                "indexes",
                "timeseriesBucketsMayHaveMixedSchemaData"
            ]
        );
        let options = md.get_document("options").unwrap();
        assert_eq!(
            keys(options),
            ["uuid", "validator", "clusteredIndex", "timeseries"]
        );
        let bucket_index = md.get_array("indexes").unwrap()[0]
            .as_document()
            .unwrap()
            .get_document("spec")
            .unwrap()
            .clone();
        assert_eq!(
            bucket_index.get_document("key").unwrap(),
            &doc! {"meta": 1, "control.min.t": 1, "control.max.t": 1}
        );
        assert_eq!(
            buckets.get_str("ident"),
            Ok(secantus_storage::collection_table("t", "ts").as_str())
        );
        // View definitions are rows of the collection registry.
        assert_eq!(
            entry("system.views").get_str("ident"),
            Ok("table:secantus_collections")
        );

        // Views come after every collection.
        let first_view = all
            .iter()
            .position(|e| e.get_str("type") != Ok("collection"))
            .unwrap();
        assert!(all[first_view..]
            .iter()
            .all(|e| e.get_str("type") != Ok("collection")));
        assert_eq!(
            entry("v"),
            doc! {
                "db": "t", "name": "v", "type": "view", "_id": "t.v", "viewOn": "c",
                "pipeline": [{"$match": {"a": 1}}], "ns": "t.v",
            }
        );
        assert_eq!(
            entry("ts"),
            doc! {
                "db": "t", "name": "ts", "type": "timeseries", "_id": "t.ts",
                "viewOn": "system.buckets.ts",
                "pipeline": [{"$_internalUnpackBucket": {
                    "timeField": "t", "metaField": "m", "bucketMaxSpanSeconds": 3600,
                }}],
                "ns": "t.ts",
            }
        );
    });
}

/// A set member's oplog is spread over the oplog's shard tables: no one
/// table holds it, so it names none.
#[test]
fn list_catalog_oplog_has_no_single_table() {
    with_wt(|c| {
        c.replica_set_name = Some("rs0".into());
        let oplog = batch(&catalog("local", Bson::String("oplog.rs".into()), c));
        assert_eq!(oplog.len(), 1);
        assert_eq!(
            keys(&oplog[0]),
            ["db", "name", "type", "md", "idxIdent", "ns"]
        );
        assert_eq!(
            oplog[0]
                .get_document("md")
                .unwrap()
                .get_array("indexes")
                .unwrap(),
            &Vec::<Bson>::new()
        );
    });
}

/// On one collection: its entry alone; through a view with no stages, the
/// collection beneath it; none for a collection that does not exist.
#[test]
fn list_catalog_on_one_collection() {
    with_wt(|c| {
        seed(c);
        on(
            "t",
            &doc! {"create": "v0", "viewOn": "c", "pipeline": []},
            c,
        );
        let names = |reply: &Document| -> Vec<String> {
            batch(reply)
                .iter()
                .map(|e| e.get_str("name").unwrap().to_string())
                .collect()
        };
        assert_eq!(names(&catalog("t", "c".into(), c)), ["c"]);
        assert_eq!(names(&catalog("t", "v0".into(), c)), ["c"]);
        assert_eq!(
            names(&catalog("t", "system.views".into(), c)),
            ["system.views"]
        );
        assert_eq!(
            names(&catalog("t", "system.buckets.ts".into(), c)),
            ["system.buckets.ts"]
        );
        assert!(names(&catalog("t", "nosuch".into(), c)).is_empty());
    });
}

/// What mongod refuses, and in which order.
#[test]
fn list_catalog_refusals() {
    with_wt(|c| {
        seed(c);
        let run = |db: &str, coll: Bson, pipeline: Vec<Bson>, c: &mut CommandContext| {
            error(&on(
                db,
                &doc! {"aggregate": coll, "pipeline": pipeline, "cursor": {}},
                c,
            ))
        };
        let spec = (
            6200600,
            "The $listCatalog stage specification must be an empty object".to_string(),
        );
        let first = (
            40602,
            "$listCatalog is only valid as the first stage in a pipeline".to_string(),
        );
        // The specification is read before where the stage stands.
        assert_eq!(
            run(
                "t",
                Bson::Int32(1),
                vec![doc! {"$listCatalog": {"x": 1}}.into()],
                c
            ),
            spec
        );
        assert_eq!(
            run(
                "admin",
                Bson::Int32(1),
                vec![doc! {"$listCatalog": 1}.into()],
                c
            ),
            spec
        );
        assert_eq!(
            run(
                "t",
                Bson::Int32(1),
                vec![doc! {"$listCatalog": {}}.into()],
                c
            ),
            (
                73,
                "Collectionless $listCatalog must be run against the 'admin' database with \
                 {aggregate: 1}"
                    .to_string()
            )
        );
        assert_eq!(
            run(
                "admin",
                Bson::Int32(1),
                vec![
                    doc! {"$listCatalog": {}}.into(),
                    doc! {"$listCatalog": {}}.into()
                ],
                c
            ),
            first
        );
        assert_eq!(
            run(
                "t",
                "c".into(),
                vec![doc! {"$match": {}}.into(), doc! {"$listCatalog": {}}.into()],
                c
            ),
            first
        );
        // A view's own stages, and a time-series collection's, go first.
        assert_eq!(
            run("t", "v".into(), vec![doc! {"$listCatalog": {}}.into()], c),
            first
        );
        assert_eq!(
            run("t", "ts".into(), vec![doc! {"$listCatalog": {}}.into()], c),
            first
        );
        assert_eq!(
            run(
                "t",
                "c".into(),
                vec![doc! {"$facet": {"x": [{"$listCatalog": {}}]}}.into()],
                c
            ),
            (
                40600,
                "$listCatalog is not allowed to be used within a $facet stage".to_string()
            )
        );
    });
}

/// A geo index is multikey only for a document with several locations in
/// its geo field, or an array on the way to another key field.
#[test]
fn list_catalog_geo_multikey() {
    with_wt(|c| {
        let make = |coll: &str, key: Document, docs: Vec<Document>, c: &mut CommandContext| {
            on(
                "t",
                &doc! {"createIndexes": coll, "indexes": [{"key": key, "name": "g"}]},
                c,
            );
            on("t", &doc! {"insert": coll, "documents": docs}, c);
        };
        make(
            "point",
            doc! {"g": "2dsphere"},
            vec![doc! {"g": {"type": "Point", "coordinates": [1, 2]}}],
            c,
        );
        make("pair", doc! {"g": "2d"}, vec![doc! {"g": [1, 2]}], c);
        make(
            "pairs",
            doc! {"g": "2d"},
            vec![doc! {"g": [[1, 2], [3, 4]]}],
            c,
        );
        make(
            "compound",
            doc! {"g": "2dsphere", "t": 1},
            vec![doc! {"g": {"type": "Point", "coordinates": [1, 2]}, "t": [1, 2]}],
            c,
        );
        let multikey = |coll: &str, c: &mut CommandContext| {
            let entry = &batch(&catalog("t", coll.into(), c))[0];
            entry
                .get_document("md")
                .unwrap()
                .get_array("indexes")
                .unwrap()[1]
                .as_document()
                .unwrap()
                .get_bool("multikey")
                .unwrap()
        };
        assert!(!multikey("point", c));
        assert!(!multikey("pair", c));
        assert!(multikey("pairs", c));
        assert!(multikey("compound", c));
    });
}

fn index_ops(c: &mut CommandContext) -> Vec<(String, i64)> {
    let mut got: Vec<(String, i64)> = batch(&on(
        "t",
        &doc! {"aggregate": "c", "pipeline": [{"$indexStats": {}}], "cursor": {}},
        c,
    ))
    .iter()
    .map(|s| {
        (
            s.get_str("name").unwrap().to_string(),
            s.get_document("accesses").unwrap().get_i64("ops").unwrap(),
        )
    })
    .collect();
    got.sort();
    got
}

fn counts(a: i64, b: i64, id: i64) -> Vec<(String, i64)> {
    vec![("_id_".into(), id), ("a_1".into(), a), ("b_1".into(), b)]
}

/// Each plan that reads an index counts once, whatever the command; a
/// `getMore`, an `explain` and a read of everything count nothing.
#[test]
fn index_stats_counts_each_plan_that_reads_an_index() {
    with_wt(|c| {
        seed(c);
        let docs: Vec<Document> = (2..12)
            .map(|i| doc! {"_id": i, "a": i % 3, "b": i})
            .collect();
        on("t", &doc! {"insert": "c", "documents": docs}, c);
        assert_eq!(index_ops(c), counts(0, 0, 0));

        on("t", &doc! {"find": "c", "filter": {"a": 1}}, c);
        on("t", &doc! {"find": "c", "filter": {"a": 1}}, c);
        assert_eq!(index_ops(c), counts(2, 0, 0));

        // Batches of one plan are one use.
        let reply = on(
            "t",
            &doc! {"find": "c", "filter": {"b": {"$gte": 0}}, "batchSize": 1},
            c,
        );
        let id = reply.get_document("cursor").unwrap().get_i64("id").unwrap();
        on("t", &doc! {"getMore": id, "collection": "c"}, c);
        assert_eq!(index_ops(c), counts(2, 1, 0));

        on("t", &doc! {"find": "c", "filter": {"_id": 3}}, c);
        on("t", &doc! {"find": "c", "filter": {}}, c);
        on("t", &doc! {"count": "c"}, c);
        assert_eq!(index_ops(c), counts(2, 1, 1));

        on("t", &doc! {"count": "c", "query": {"a": 1}}, c);
        on("t", &doc! {"distinct": "c", "key": "a"}, c);
        on(
            "t",
            &doc! {"aggregate": "c", "pipeline": [{"$match": {"a": 1}}], "cursor": {}},
            c,
        );
        assert_eq!(index_ops(c), counts(5, 1, 1));

        on(
            "t",
            &doc! {"update": "c", "updates": [{"q": {"a": 1}, "u": {"$set": {"x": 1}}}]},
            c,
        );
        on(
            "t",
            &doc! {"findAndModify": "c", "query": {"b": 5}, "update": {"$set": {"x": 2}}},
            c,
        );
        on(
            "t",
            &doc! {"delete": "c", "deletes": [{"q": {"a": 2}, "limit": 0}]},
            c,
        );
        assert_eq!(index_ops(c), counts(7, 2, 1));

        on(
            "t",
            &doc! {"explain": {"find": "c", "filter": {"a": 1}}, "verbosity": "executionStats"},
            c,
        );
        on(
            "t",
            &doc! {
                "explain": {"aggregate": "c", "pipeline": [{"$match": {"a": 1}}], "cursor": {}},
                "verbosity": "executionStats",
            },
            c,
        );
        assert_eq!(index_ops(c), counts(7, 2, 1));
    });
}

/// An index made again counts from nothing, from when it was made.
#[test]
fn index_stats_start_again_for_a_new_index() {
    with_wt(|c| {
        seed(c);
        on("t", &doc! {"find": "c", "filter": {"a": 1}}, c);
        let since = |c: &mut CommandContext| {
            batch(&on(
                "t",
                &doc! {"aggregate": "c", "pipeline": [{"$indexStats": {}}], "cursor": {}},
                c,
            ))
            .into_iter()
            .find(|s| s.get_str("name") == Ok("a_1"))
            .unwrap()
            .get_document("accesses")
            .unwrap()
            .get_datetime("since")
            .copied()
            .unwrap()
        };
        let before = since(c);
        assert_eq!(index_ops(c), counts(1, 0, 0));
        on("t", &doc! {"dropIndexes": "c", "index": "a_1"}, c);
        std::thread::sleep(std::time::Duration::from_millis(5));
        on(
            "t",
            &doc! {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
            c,
        );
        assert_eq!(index_ops(c), counts(0, 0, 0));
        assert!(since(c) > before);
    });
}

/// The counts are kept in memory, as mongod keeps them: a restart starts
/// them again.
#[test]
fn index_stats_restart_from_nothing() {
    let dir = std::env::temp_dir().join(format!("secantus-catalog-stats-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let open = || {
        let wt = Arc::new(WtStorage::open(dir.to_str().unwrap()).unwrap());
        let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(wt));
        let mut ctx = CommandContext::new(1)
            .with_storage(adapter)
            .with_cursors(Arc::new(CursorRegistry::new()));
        ctx.db_name = "t".into();
        ctx
    };
    {
        let mut c = open();
        seed(&mut c);
        on("t", &doc! {"find": "c", "filter": {"a": 1}}, &mut c);
        assert_eq!(index_ops(&mut c), counts(1, 0, 0));
    }
    {
        let mut c = open();
        assert_eq!(index_ops(&mut c), counts(0, 0, 0));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
