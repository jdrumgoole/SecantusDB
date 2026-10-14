//! `logRotate`, `connPoolStats`, `dbHash`, `getDefaultRWConcern` /
//! `setDefaultRWConcern` and `setFeatureCompatibilityVersion` over real
//! WiredTiger. Every expectation is what mongod 8.2.11 answered, as a
//! one-member replica set, to the same command and the same documents
//! (`tools/probes/admin_commands.py`, 2026-10-14): a `dbHash` digest here
//! is the digest mongod gave for these documents, not one this server gave.

mod common;

use std::sync::{Arc, Mutex};

use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::auth::ConnectionAuth;
use secantus_commands::CommandContext;

/// Dispatch on `db`.
fn on(c: &mut CommandContext, db: &str, command: Document) -> Document {
    let was = std::mem::replace(&mut c.db_name, db.to_string());
    let reply = dispatch_full(&command, c);
    c.db_name = was;
    reply
}

fn admin(c: &mut CommandContext, command: Document) -> Document {
    on(c, "admin", command)
}

fn code(reply: &Document) -> Option<i32> {
    if reply.get_f64("ok") == Ok(1.0) {
        None
    } else {
        Some(reply.get_i32("code").unwrap_or(-1))
    }
}

fn errmsg(reply: &Document) -> &str {
    reply.get_str("errmsg").unwrap_or("")
}

fn keys(reply: &Document) -> Vec<&str> {
    reply.keys().map(String::as_str).collect()
}

// ------------------------------------------------------------- logRotate

#[test]
fn log_rotate_answers_as_a_mongod_without_a_log_file() {
    with_wt(|c| {
        for value in [
            Bson::Int32(1),
            Bson::Int32(0),
            Bson::Double(1.5),
            Bson::Boolean(true),
            Bson::String("server".into()),
        ] {
            let reply = admin(c, doc! {"logRotate": value.clone()});
            assert_eq!(code(&reply), None, "{value}: {reply}");
        }
        for name in ["audit", "bogus", "", "SERVER"] {
            let reply = admin(c, doc! {"logRotate": name});
            assert_eq!(code(&reply), Some(4), "{name}: {reply}");
            assert_eq!(
                errmsg(&reply),
                "Log rotation failed due to one or more errors"
            );
        }
        let reply = admin(c, doc! {"logRotate": {"a": 1}});
        assert_eq!(code(&reply), Some(14));
        assert_eq!(
            errmsg(&reply),
            "BSON field 'logRotate.logRotate' is the wrong type 'object', expected types \
             '[bool, string]'"
        );
        let reply = admin(c, doc! {"logRotate": Bson::Null});
        assert_eq!(code(&reply), Some(14));
        assert!(errmsg(&reply).contains("wrong type 'null'"), "{reply}");
        let reply = admin(c, doc! {"logRotate": 1, "bogus": 1});
        assert_eq!(code(&reply), Some(40415));
        assert_eq!(
            errmsg(&reply),
            "BSON field 'logRotate.bogus' is an unknown field."
        );
        let reply = admin(c, doc! {"logRotate": "server", "writeConcern": {"w": 1}});
        assert_eq!(code(&reply), Some(72));
        assert_eq!(errmsg(&reply), "Command does not support writeConcern");
        let reply = on(c, "t", doc! {"logRotate": 1});
        assert_eq!(code(&reply), Some(13));
        assert_eq!(
            errmsg(&reply),
            "logRotate may only be run against the admin database."
        );
        let reply = admin(c, doc! {"logrotate": 1});
        assert_eq!(code(&reply), Some(59));
    });
}

// --------------------------------------------------------- connPoolStats

#[test]
fn conn_pool_stats_has_mongod_shape_with_no_pools() {
    with_wt(|c| {
        for db in ["admin", "t"] {
            let reply = on(c, db, doc! {"connPoolStats": 1});
            assert_eq!(code(&reply), None, "{reply}");
            assert_eq!(
                keys(&reply),
                [
                    "numClientConnections",
                    "numAScopedConnections",
                    "totalInUse",
                    "totalAvailable",
                    "totalLeased",
                    "totalCreated",
                    "totalRefreshing",
                    "totalRefreshed",
                    "totalWasNeverUsed",
                    "totalRejectedRequests",
                    "totalPendingRequests",
                    "acquisitionWaitTimes",
                    "pools",
                    "hosts",
                    "numReplicaSetMonitorsCreated",
                    "replicaSets",
                    "ok"
                ]
            );
            assert_eq!(reply.get("totalCreated"), Some(&Bson::Int32(0)));
            let waits = reply.get_document("acquisitionWaitTimes").unwrap();
            assert_eq!(waits.len(), 23);
            assert_eq!(
                waits.keys().next().map(String::as_str),
                Some("(-inf, 0 ms)")
            );
            assert_eq!(
                waits.get_document("[950 ms, 1000 ms)").unwrap(),
                &doc! {"count": 0_i64}
            );
            assert_eq!(waits.get("totalCount"), Some(&Bson::Int64(0)));
            assert_eq!(reply.get_document("pools").unwrap(), &Document::new());
            assert_eq!(reply.get_document("replicaSets").unwrap(), &Document::new());
        }
        let reply = admin(c, doc! {"connPoolStats": 1, "bogus": 1});
        assert_eq!(code(&reply), None);
        let reply = admin(c, doc! {"connPoolStats": 1, "writeConcern": {"w": 1}});
        assert_eq!(code(&reply), Some(72));
    });
}

// ---------------------------------------------------------------- dbHash

/// `hashdb`: the database name is part of a view's `system.views` entry.
const HASHDB: &str = "hashdb";

fn seed_hashdb(c: &mut CommandContext) {
    let h = |c: &mut CommandContext, command: Document| {
        let reply = on(c, HASHDB, command.clone());
        assert_eq!(code(&reply), None, "{command}: {reply}");
    };
    h(
        c,
        doc! {"insert": "a", "documents": [{"_id": 3, "x": "c"}, {"_id": 1, "x": "a"}, {"_id": 2, "x": "b"}]},
    );
    h(
        c,
        doc! {"insert": "b", "documents": [{"y": 1, "_id": "later"}]},
    );
    h(
        c,
        doc! {"insert": "b", "documents": [{"_id": {"k": 1}, "n": [1, 2.0, 3_i64]}]},
    );
    h(
        c,
        doc! {"update": "b", "updates": [{"q": {"_id": "later"}, "u": {"$set": {"z": 2}}}]},
    );
    h(c, doc! {"create": "cap", "capped": true, "size": 4096});
    h(
        c,
        doc! {"insert": "cap", "documents": [{"_id": 2}, {"_id": 1}]},
    );
    h(c, doc! {"create": "v", "viewOn": "a", "pipeline": []});
    h(
        c,
        doc! {"create": "coll", "collation": {"locale": "en", "strength": 2}},
    );
    h(
        c,
        doc! {"insert": "coll", "documents": [{"_id": "b"}, {"_id": "C"}, {"_id": "a"}]},
    );
    h(
        c,
        doc! {"insert": "mixed", "documents": [
            {"_id": "s"},
            {"_id": 1},
            {"_id": Bson::Null},
            {"_id": 1.5},
            {"_id": true},
            {"_id": bson::DateTime::from_millis(1_577_836_800_000)},
            {"_id": bson::oid::ObjectId::parse_str("5f0000000000000000000000").unwrap()},
            {"_id": {"a": 1}},
            {"_id": Binary {subtype: BinarySubtype::Generic, bytes: b"x".to_vec()}},
            {"_id": bson::Timestamp {time: 1, increment: 1}},
            {"_id": 2_i64},
        ]},
    );
    h(c, doc! {"insert": "empty", "documents": [{"_id": 1}]});
    h(
        c,
        doc! {"delete": "empty", "deletes": [{"q": {}, "limit": 0}]},
    );
    h(c, doc! {"create": "emptyc"});
    h(c, doc! {"profile": 2});
    h(c, doc! {"find": "a", "filter": {"_id": 1}});
    h(c, doc! {"profile": 0});
    let big: Vec<Bson> = (0..2000)
        .map(|i| Bson::Document(doc! {"_id": i, "p": "x".repeat(100)}))
        .collect();
    h(c, doc! {"insert": "big", "documents": big});
}

#[test]
fn db_hash_digests_are_mongods() {
    with_wt(|c| {
        seed_hashdb(c);
        let reply = on(c, HASHDB, doc! {"dbHash": 1});
        assert_eq!(code(&reply), None, "{reply}");
        assert_eq!(
            keys(&reply),
            [
                "host",
                "collections",
                "capped",
                "uuids",
                "md5",
                "timeMillis",
                "ok"
            ]
        );
        let collections = reply.get_document("collections").unwrap();
        // Each digest is mongod's for the same documents: `_id` order, a
        // collated collection in its collation, a capped one in insertion
        // order, a view left out and `system.views` hashed.
        assert_eq!(
            collections,
            &doc! {
                "a": "c0639a6631701a8748804a117985afb5",
                "b": "41142a697e69b953f4f542cdd4701463",
                "big": "6a7431be080c175be340c71b37b231e0",
                "cap": "090903a378558fbf1d70a0bb25eaee36",
                "coll": "84ec7f21196c174f007736dd41d77e07",
                "empty": "d41d8cd98f00b204e9800998ecf8427e",
                "emptyc": "d41d8cd98f00b204e9800998ecf8427e",
                "mixed": "238c35f072d44d4dcb07c0cb45492c75",
                "system.views": "aad469306a8b657db4298afca12705c0",
            }
        );
        assert_eq!(
            reply.get_array("capped").unwrap(),
            &vec![Bson::String("cap".into())]
        );
        let uuids = reply.get_document("uuids").unwrap();
        assert_eq!(
            uuids.keys().collect::<Vec<_>>(),
            collections.keys().collect::<Vec<_>>()
        );
        for (_, uuid) in uuids {
            assert!(matches!(
                uuid,
                Bson::Binary(Binary { subtype: BinarySubtype::Uuid, bytes }) if bytes.len() == 16
            ));
        }
        assert!(matches!(reply.get("timeMillis"), Some(Bson::Int32(_))));
        assert!(reply.get_str("host").unwrap().ends_with(":27017"));

        // The whole: the digests' hex, in name order.
        let reply = on(c, HASHDB, doc! {"dbHash": 1, "collections": ["b", "a"]});
        assert_eq!(
            reply
                .get_document("collections")
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            reply.get_str("md5").unwrap(),
            "ca2989b379c9c52940db33c1dd0802bb"
        );
        assert_eq!(reply.get_array("capped").unwrap(), &Vec::<Bson>::new());
        let reply = on(
            c,
            HASHDB,
            doc! {"dbHash": 1, "collections": ["a", "nosuch", "v", "system.views", "system.profile"]},
        );
        assert_eq!(
            reply
                .get_document("collections")
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["a", "system.views"]
        );
        assert_eq!(
            reply.get_str("md5").unwrap(),
            "4b4c2532dbd2ebc4759c5c3daf613cc3"
        );
    });
}

#[test]
fn db_hash_arguments_and_system_databases() {
    with_wt(|c| {
        seed_hashdb(c);
        let everything = on(c, HASHDB, doc! {"dbHash": 1});
        // A `collections` that is not an array, or is empty, is every
        // collection; any other field is ignored; `dbhash` is the same command.
        for command in [
            doc! {"dbHash": 1, "collections": "x"},
            doc! {"dbHash": 1, "collections": []},
            doc! {"dbHash": 1, "collections": {}},
            doc! {"dbHash": 1, "collections": Bson::Null},
            doc! {"dbHash": 1, "bogus": 1},
            doc! {"dbHash": "x"},
            doc! {"dbhash": 1},
            doc! {"dbHash": 1, "comment": "hi"},
        ] {
            let reply = on(c, HASHDB, command.clone());
            assert_eq!(reply.get_str("md5"), everything.get_str("md5"), "{command}");
        }
        let reply = on(c, HASHDB, doc! {"dbHash": 1, "collections": [1]});
        assert_eq!(code(&reply), Some(2));
        assert_eq!(errmsg(&reply), "collections entries have to be strings");
        let reply = on(c, HASHDB, doc! {"dbHash": 1, "writeConcern": {"w": 1}});
        assert_eq!(code(&reply), Some(72));

        // A database with nothing, and `local`, whose collections mongod
        // does not replicate and so does not hash.
        on(
            c,
            "local",
            doc! {"insert": "stuff", "documents": [{"_id": 1}]},
        );
        for db in ["nosuchdb_zz", "local"] {
            let reply = on(c, db, doc! {"dbHash": 1});
            assert_eq!(reply.get_document("collections").unwrap(), &Document::new());
            assert_eq!(
                reply.get_str("md5").unwrap(),
                "d41d8cd98f00b204e9800998ecf8427e"
            );
        }
        // After `setFeatureCompatibilityVersion`, `admin.system.version`
        // holds mongod's own document and hashes as mongod's does.
        admin(
            c,
            doc! {"setFeatureCompatibilityVersion": "8.2", "confirm": true},
        );
        let reply = admin(c, doc! {"dbHash": 1});
        assert_eq!(
            reply
                .get_document("collections")
                .unwrap()
                .get_str("system.version"),
            Ok("dfa15704aeebc24e910e8ee0e1061af8")
        );
    });
}

// --------------------------------------------------- default read/write concern

const SET: &str = "setDefaultRWConcern";
const RC: &str = "defaultReadConcern";
const WC: &str = "defaultWriteConcern";

#[test]
fn default_rw_concern_implicit_until_set() {
    with_wt(|c| {
        let reply = admin(c, doc! {"getDefaultRWConcern": 1});
        assert_eq!(
            reply,
            doc! {
                "defaultReadConcern": {"level": "local"},
                "defaultWriteConcern": {"w": "majority", "wtimeout": 0_i32},
                "defaultWriteConcernSource": "implicit",
                "defaultReadConcernSource": "implicit",
                "localUpdateWallClockTime": reply.get("localUpdateWallClockTime").cloned().unwrap(),
                "ok": 1.0,
            }
        );
        assert!(matches!(
            reply.get("localUpdateWallClockTime"),
            Some(Bson::DateTime(_))
        ));
        let reply = admin(c, doc! {"getDefaultRWConcern": 1, "inMemory": true});
        assert_eq!(reply.get("inMemory"), Some(&Bson::Boolean(true)));
        let reply = admin(c, doc! {"getDefaultRWConcern": 1, "inMemory": false});
        assert!(!reply.contains_key("inMemory"));
        let reply = admin(c, doc! {"getDefaultRWConcern": 1, "inMemory": 1});
        assert_eq!(code(&reply), Some(14));
        assert_eq!(
            errmsg(&reply),
            "BSON field 'getDefaultRWConcern.inMemory' is the wrong type 'int', expected type \
             'bool'"
        );
        let reply = admin(c, doc! {"getDefaultRWConcern": 1, "bogus": 1});
        assert_eq!(code(&reply), Some(40415));
        let reply = on(c, "t", doc! {"getDefaultRWConcern": 1});
        assert_eq!(code(&reply), Some(13));
        let reply = on(c, "t", doc! {SET: 1, RC: {"level": "local"}});
        assert_eq!(code(&reply), Some(13));
        assert_eq!(
            errmsg(&reply),
            "setDefaultRWConcern may only be run against the admin database."
        );
    });
}

#[test]
fn set_default_rw_concern_refusals() {
    with_wt(|c| {
        let cases: Vec<(Document, i32, &str)> = vec![
            (
                doc! {SET: 1},
                2,
                "At least one of the \"defaultReadConcern\" or \"defaultWriteConcern\" fields must be present",
            ),
            (doc! {SET: 1, RC: Bson::Null, WC: Bson::Null}, 2, "At least one of the \"defaultReadConcern\" or \"defaultWriteConcern\" fields must be present"),
            (doc! {SET: 1, "bogus": 1}, 40415, "BSON field 'setDefaultRWConcern.bogus' is an unknown field."),
            (doc! {SET: 1, RC: {"level": "bogus"}}, 2, "Enumeration value 'bogus' for field 'readConcern.level' is not a valid value."),
            (doc! {SET: 1, RC: {"level": "snapshot"}}, 2, "level: 'snapshot' is not suitable for the default read concern"),
            (doc! {SET: 1, RC: {"level": "linearizable"}}, 2, "level: 'linearizable' is not suitable for the default read concern"),
            (doc! {SET: 1, RC: {"level": "majority", "afterClusterTime": bson::Timestamp {time: 1, increment: 1}}}, 2, "'afterClusterTime' is not suitable for the default read concern"),
            (doc! {SET: 1, RC: {"level": "local", "atClusterTime": bson::Timestamp {time: 1, increment: 1}}}, 72, "atClusterTime field can be set only if level is equal to snapshot"),
            (doc! {SET: 1, RC: {"level": 1}}, 14, "BSON field 'readConcern.level' is the wrong type 'int', expected type 'string'"),
            (doc! {SET: 1, RC: {"bogus": 1}}, 40415, "BSON field 'readConcern.bogus' is an unknown field."),
            (doc! {SET: 1, RC: "x"}, 14, "BSON field 'setDefaultRWConcern.defaultReadConcern' is the wrong type 'string', expected type 'object'"),
            (doc! {SET: 1, RC: {"level": "majority", "provenance": "clientSupplied"}}, 2, "'provenance' must be unset in default read concern"),
            (doc! {SET: 1, WC: {"w": 0}}, 2, "Unacknowledged write concern is not suitable for the default write concern"),
            (doc! {SET: 1, WC: {"w": -1}}, 9, "w has to be a non-negative number and not greater than 50; found: -1"),
            (doc! {SET: 1, WC: {"w": 51}}, 9, "w has to be a non-negative number and not greater than 50; found: 51"),
            (doc! {SET: 1, WC: {"w": "bogus"}}, 79, "No write concern mode named 'bogus' found in replica set configuration"),
            (doc! {SET: 1, WC: {"w": Bson::Null}}, 79, "No write concern mode named '' found in replica set configuration"),
            (doc! {SET: 1, WC: {"w": true}}, 9, "w has to be a number, string, or object; found: bool"),
            (doc! {SET: 1, WC: {"w": {}}}, 9, "tagged write concern requires tags"),
            (doc! {SET: 1, WC: {"w": 1, "bogus": 1}}, 40415, "BSON field 'WriteConcernOptions.bogus' is an unknown field."),
            (doc! {SET: 1, WC: {"j": true}}, 2, "Default write concern must have 'w' field."),
            (doc! {SET: 1, WC: {"w": 1, "j": true, "fsync": true}}, 9, "fsync and j options cannot be used together"),
            (doc! {SET: 1, WC: {"w": 1, "j": "x"}}, 14, "BSON field 'WriteConcernOptions.j' is the wrong type 'string', expected types '[bool, double, long, decimal, int]'"),
            (doc! {SET: 1, WC: {"w": 1, "wtimeout": 1_i64 << 40}}, 9, "wtimeout must be a 32-bit integer"),
            (doc! {SET: 1, WC: {"w": 1, "provenance": "clientSupplied"}}, 2, "'provenance' must be unset in default write concern"),
            (doc! {SET: 1, WC: "x"}, 14, "BSON field 'setDefaultRWConcern.defaultWriteConcern' is the wrong type 'string', expected type 'object'"),
            // The fields are read in the order sent: a write concern that
            // does not parse is reported before a read concern that is
            // unsuitable, and the other way about.
            (doc! {SET: 1, RC: {"level": "snapshot"}, WC: {"w": true}}, 9, "w has to be a number, string, or object; found: bool"),
            (doc! {SET: 1, RC: {"level": "snapshot"}, WC: {"w": "bogus"}}, 2, "level: 'snapshot' is not suitable for the default read concern"),
            (doc! {SET: 1, WC: {"w": 1, "bogus": 1}, RC: {"bogus": 1}}, 40415, "BSON field 'WriteConcernOptions.bogus' is an unknown field."),
            (doc! {SET: 1, WC: {"w": 1, "j": true, "fsync": true}, "bogus": 1}, 9, "fsync and j options cannot be used together"),
        ];
        for (command, want_code, want_msg) in cases {
            let reply = admin(c, command.clone());
            assert_eq!(code(&reply), Some(want_code), "{command}: {reply}");
            assert_eq!(errmsg(&reply), want_msg, "{command}");
        }
        // None of that stored anything.
        let reply = admin(c, doc! {"getDefaultRWConcern": 1});
        assert_eq!(reply.get_str("defaultReadConcernSource"), Ok("implicit"));
        assert!(!reply.contains_key("updateOpTime"));
    });
}

#[test]
fn set_default_rw_concern_stores_and_reads_back() {
    with_wt(|c| {
        // Unsetting a write concern that was never set is allowed, and
        // leaves a settings document that only says when.
        let reply = admin(c, doc! {SET: 1, WC: {}});
        assert_eq!(code(&reply), None, "{reply}");
        assert_eq!(reply.get_str("defaultWriteConcernSource"), Ok("implicit"));
        assert!(matches!(
            reply.get("updateOpTime"),
            Some(Bson::Timestamp(_))
        ));

        let reply = admin(c, doc! {SET: 1, RC: {"level": "available"}});
        assert_eq!(
            keys(&reply),
            [
                "defaultReadConcern",
                "defaultWriteConcern",
                "updateOpTime",
                "updateWallClockTime",
                "defaultWriteConcernSource",
                "defaultReadConcernSource",
                "localUpdateWallClockTime",
                "ok"
            ]
        );
        assert_eq!(
            reply.get_document(RC).unwrap(),
            &doc! {"level": "available"}
        );
        assert_eq!(
            reply.get_document(WC).unwrap(),
            &doc! {"w": "majority", "wtimeout": 0_i32}
        );
        assert_eq!(reply.get_str("defaultWriteConcernSource"), Ok("implicit"));
        assert_eq!(reply.get_str("defaultReadConcernSource"), Ok("global"));
        let again = admin(c, doc! {"getDefaultRWConcern": 1});
        assert_eq!(again.get("updateOpTime"), reply.get("updateOpTime"));
        assert_eq!(
            again.get("updateWallClockTime"),
            reply.get("updateWallClockTime")
        );

        // The write concern as mongod stores it: `w` an int, `j` only when
        // given, `wtimeout` an int (0 when not a number), `fsync` when true.
        let stored: Vec<(Document, Document)> = vec![
            (
                doc! {"w": 1, "wtimeout": "x"},
                doc! {"w": 1_i32, "wtimeout": 0_i32},
            ),
            (doc! {"w": 1.5}, doc! {"w": 1_i32, "wtimeout": 0_i32}),
            (
                doc! {"w": "majority", "wtimeout": 500, "j": false},
                doc! {"w": "majority", "j": false, "wtimeout": 500_i32},
            ),
            (
                doc! {"w": 1, "j": 1},
                doc! {"w": 1_i32, "j": true, "wtimeout": 0_i32},
            ),
            (
                doc! {"w": 1, "wtimeout": 2.7},
                doc! {"w": 1_i32, "wtimeout": 2_i32},
            ),
            (
                doc! {"w": 1, "wtimeout": -5},
                doc! {"w": 1_i32, "wtimeout": -5_i32},
            ),
            (doc! {"w": 1_i64}, doc! {"w": 1_i32, "wtimeout": 0_i32}),
            (
                doc! {"w": 1, "fsync": true},
                doc! {"w": 1_i32, "wtimeout": 0_i32, "fsync": true},
            ),
            (
                doc! {"w": 1, "fsync": false},
                doc! {"w": 1_i32, "wtimeout": 0_i32},
            ),
            (
                doc! {"w": 1, "getLastError": 1},
                doc! {"w": 1_i32, "wtimeout": 0_i32},
            ),
            (
                doc! {"w": {"dc": 1}},
                doc! {"w": {"dc": 1_i64}, "wtimeout": 0_i32},
            ),
            (doc! {"w": 50}, doc! {"w": 50_i32, "wtimeout": 0_i32}),
        ];
        for (given, want) in stored {
            let reply = admin(
                c,
                doc! {SET: 1, WC: given.clone(), "writeConcern": {"w": 1}},
            );
            assert_eq!(code(&reply), None, "{given}: {reply}");
            assert_eq!(reply.get_document(WC).unwrap(), &want, "{given}");
            assert_eq!(reply.get_str("defaultWriteConcernSource"), Ok("global"));
        }
        // The read concern keeps its level through a write-only set, and
        // is unset by an empty document or a null level.
        let reply = admin(c, doc! {SET: 1, WC: {"w": 1}, "writeConcern": {"w": 1}});
        assert_eq!(
            reply.get_document(RC).unwrap(),
            &doc! {"level": "available"}
        );
        let reply = admin(c, doc! {SET: 1, RC: {}});
        assert_eq!(reply.get_document(RC).unwrap(), &doc! {"level": "local"});
        assert_eq!(reply.get_str("defaultReadConcernSource"), Ok("implicit"));
        assert_eq!(
            reply.get_document(WC).unwrap(),
            &doc! {"w": 1_i32, "wtimeout": 0_i32}
        );
        // A write concern once set cannot be unset.
        for command in [doc! {SET: 1, WC: {}}, doc! {SET: 1, RC: {}, WC: {}}] {
            let reply = admin(c, command.clone());
            assert_eq!(code(&reply), Some(20), "{command}: {reply}");
            assert_eq!(
                errmsg(&reply),
                "The global default write concern cannot be unset once it is set."
            );
        }
        // What `config.settings` holds is mongod's document.
        let reply = on(c, "config", doc! {"find": "settings"});
        let batch = reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        let settings = batch[0].as_document().unwrap();
        assert_eq!(
            keys(settings),
            [
                "_id",
                "defaultWriteConcern",
                "updateOpTime",
                "updateWallClockTime"
            ]
        );
        assert_eq!(settings.get_str("_id"), Ok("ReadWriteConcernDefaults"));
    });
}

#[test]
fn default_rw_concern_survives_a_restart() {
    use secantus_commands::{CursorRegistry, Storage as CmdStorage};
    use secantus_storage::Storage as WtStorage;
    use secantus_storage_adapter::StorageAdapter;

    let dir = std::env::temp_dir().join(format!("secantus-rwc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let open = |dir: &std::path::Path| {
        let wt = Arc::new(WtStorage::open(dir.to_str().unwrap()).unwrap());
        let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(wt));
        let mut ctx = CommandContext::new(1)
            .with_storage(adapter)
            .with_cursors(Arc::new(CursorRegistry::new()));
        ctx.db_name = "admin".into();
        ctx
    };
    let set_at;
    {
        let mut c = open(&dir);
        let reply = admin(
            &mut c,
            doc! {SET: 1, RC: {"level": "majority"}, WC: {"w": 1, "wtimeout": 250}},
        );
        assert_eq!(code(&reply), None, "{reply}");
        set_at = reply.get("updateWallClockTime").cloned();
        admin(
            &mut c,
            doc! {"setFeatureCompatibilityVersion": "8.0", "confirm": true},
        );
        drop(c);
    }
    {
        let mut c = open(&dir);
        let reply = admin(&mut c, doc! {"getDefaultRWConcern": 1});
        assert_eq!(reply.get_document(RC).unwrap(), &doc! {"level": "majority"});
        assert_eq!(
            reply.get_document(WC).unwrap(),
            &doc! {"w": 1_i32, "wtimeout": 250_i32}
        );
        assert_eq!(reply.get_str("defaultWriteConcernSource"), Ok("global"));
        assert_eq!(reply.get("updateWallClockTime"), set_at.as_ref());
        let reply = admin(
            &mut c,
            doc! {"getParameter": 1, "featureCompatibilityVersion": 1},
        );
        assert_eq!(
            reply.get_document("featureCompatibilityVersion").unwrap(),
            &doc! {"version": "8.0"}
        );
        drop(c);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_default_write_concern_no_node_satisfies() {
    with_wt(|c| {
        let reply = admin(c, doc! {SET: 1, WC: {"w": 3}});
        assert_eq!(code(&reply), None, "{reply}");
        // A write with no write concern of its own is held to the default,
        // and says so.
        let reply = on(c, "t", doc! {"insert": "c", "documents": [{"_id": 1}]});
        assert_eq!(reply.get_i32("n"), Ok(1));
        assert_eq!(
            reply.get_document("writeConcernError").unwrap(),
            &doc! {
                "code": 100_i32,
                "codeName": "UnsatisfiableWriteConcern",
                "errmsg": "Not enough data-bearing nodes",
                "errInfo": {"writeConcern": {"w": 3_i32, "wtimeout": 0_i32, "provenance": "customDefault"}},
            }
        );
        assert_eq!(keys(&reply).last(), Some(&"ok"));
        // One with its own is not; a read is not.
        let reply = on(
            c,
            "t",
            doc! {"insert": "c", "documents": [{"_id": 2}], "writeConcern": {"w": 1}},
        );
        assert!(!reply.contains_key("writeConcernError"));
        let reply = on(c, "t", doc! {"find": "c"});
        assert!(!reply.contains_key("writeConcernError"));
        for command in [
            doc! {"create": "d"},
            doc! {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
            doc! {"collMod": "c", "validationLevel": "off"},
            doc! {"aggregate": "c", "pipeline": [{"$out": "e"}], "cursor": {}},
            doc! {"drop": "d"},
        ] {
            let reply = on(c, "t", command.clone());
            assert_eq!(code(&reply), None, "{command}: {reply}");
            assert!(
                reply.contains_key("writeConcernError"),
                "{command}: {reply}"
            );
        }
        let reply = on(
            c,
            "t",
            doc! {"aggregate": "c", "pipeline": [], "cursor": {}},
        );
        assert!(!reply.contains_key("writeConcernError"));
        let reply = admin(c, doc! {"ping": 1});
        assert!(!reply.contains_key("writeConcernError"));
        // `setDefaultRWConcern`'s own write is held to the default it
        // replaces: it fails, and the new value is stored all the same.
        let reply = admin(c, doc! {SET: 1, WC: {"w": 1}});
        assert_eq!(
            keys(&reply),
            ["writeConcernError", "ok", "errmsg", "code", "codeName"]
        );
        assert_eq!(code(&reply), Some(100));
        assert_eq!(
            errmsg(&reply),
            "Not enough data-bearing nodes; Error details: { writeConcern: { w: 3, wtimeout: 0, \
             provenance: \"customDefault\" } }"
        );
        let reply = on(c, "t", doc! {"insert": "c", "documents": [{"_id": 3}]});
        assert!(!reply.contains_key("writeConcernError"), "{reply}");
        // A tag set names a tag the one-member set has no key for.
        admin(c, doc! {SET: 1, WC: {"w": {"dc": 1}}});
        let reply = on(c, "t", doc! {"insert": "c", "documents": [{"_id": 4}]});
        let wce = reply.get_document("writeConcernError").unwrap();
        assert_eq!(wce.get_i32("code"), Ok(4));
        assert_eq!(
            wce.get_str("errmsg"),
            Ok("No replica set tag key dc in config")
        );
        let reply = admin(c, doc! {SET: 1, WC: {"w": 1}});
        assert_eq!(code(&reply), Some(4));
        assert_eq!(
            errmsg(&reply),
            "No replica set tag key dc in config; Error details: { writeConcern: { w: { dc: 1 }, \
             wtimeout: 0, provenance: \"customDefault\" } }"
        );
        // `dropDatabase` fails too, naming what it waited for.
        admin(c, doc! {SET: 1, WC: {"w": 3}});
        let reply = on(c, "t", doc! {"dropDatabase": 1});
        assert_eq!(code(&reply), Some(100), "{reply}");
        let msg = errmsg(&reply);
        assert!(
            msg.starts_with("dropDatabase t failed waiting for ")
                && msg.contains(" collection drop(s) (most recent drop optime: { ts: Timestamp(")
                && msg.ends_with(
                    "), t: 1 }) to replicate. :: caused by :: Not enough data-bearing nodes"
                ),
            "{msg}"
        );
    });
}

// ------------------------------------------ setFeatureCompatibilityVersion

const FCV: &str = "setFeatureCompatibilityVersion";

#[test]
fn set_feature_compatibility_version_arguments() {
    with_wt(|c| {
        let downgrade = "Once you have downgraded the FCV, if you choose to downgrade the binary \
                         version, it will require support assistance. Please re-run this command \
                         with 'confirm: true' to acknowledge this and continue with the FCV \
                         downgrade.";
        let invalid = |v: &str| {
            format!(
                "Invalid feature compatibility version value '{v}'. Expected one of the \
                 following versions: ''8.2', '8.1', '8.0'. See \
                 https://docs.mongodb.com/master/release-notes/8.0-compatibility/\
                 #feature-compatibility."
            )
        };
        let cases: Vec<(Document, Option<i32>, String)> = vec![
            (doc! {FCV: "8.2"}, Some(7369100), downgrade.into()),
            (doc! {FCV: "8.2", "confirm": false}, Some(7369100), downgrade.into()),
            (doc! {FCV: "8.2", "confirm": 0}, Some(7369100), downgrade.into()),
            (doc! {FCV: "8.2", "confirm": Bson::Null}, Some(7369100), downgrade.into()),
            (doc! {FCV: "8.2", "confirm": true}, None, String::new()),
            (doc! {FCV: "8.2", "confirm": 1}, None, String::new()),
            (doc! {FCV: "8.2", "confirm": 2.5}, None, String::new()),
            (doc! {FCV: "8.2", "confirm": true, "fromConfigServer": true}, None, String::new()),
            (doc! {FCV: "8.2", "confirm": true, "dryRun": Bson::Null}, None, String::new()),
            (doc! {FCV: "8.2", "confirm": true, "comment": {"a": 1}, "writeConcern": {"w": 1}, "maxTimeMS": 1000}, None, String::new()),
            (doc! {FCV: "8.2", "confirm": "yes"}, Some(14), "BSON field 'setFeatureCompatibilityVersion.confirm' is the wrong type 'string', expected types '[decimal, bool, int, double, long]'".into()),
            (doc! {FCV: "7.0"}, Some(4926900), invalid("7.0")),
            (doc! {FCV: "6.0", "confirm": true}, Some(4926900), invalid("6.0")),
            (doc! {FCV: "8.3", "confirm": true}, Some(4926900), invalid("8.3")),
            (doc! {FCV: "abc", "confirm": true}, Some(4926900), invalid("abc")),
            (doc! {FCV: "", "confirm": true}, Some(4926900), invalid("")),
            (doc! {FCV: " 8.2", "confirm": true}, Some(4926900), invalid(" 8.2")),
            (doc! {FCV: 1, "confirm": true}, Some(14), "BSON field 'setFeatureCompatibilityVersion.setFeatureCompatibilityVersion' is the wrong type 'int', expected type 'string'".into()),
            (doc! {FCV: Bson::Null, "confirm": true}, Some(40414), "BSON field 'setFeatureCompatibilityVersion.setFeatureCompatibilityVersion' is missing but a required field".into()),
            (doc! {FCV: "8.2", "confirm": true, "bogus": 1}, Some(40415), "BSON field 'setFeatureCompatibilityVersion.bogus' is an unknown field.".into()),
            (doc! {FCV: 1, "bogus": 1}, Some(40415), "BSON field 'setFeatureCompatibilityVersion.bogus' is an unknown field.".into()),
            (doc! {FCV: "7.0", "confirm": "x"}, Some(14), "BSON field 'setFeatureCompatibilityVersion.confirm' is the wrong type 'string', expected types '[decimal, bool, int, double, long]'".into()),
            (doc! {FCV: "8.2", "confirm": true, "fromConfigServer": "x"}, Some(14), "BSON field 'setFeatureCompatibilityVersion.fromConfigServer' is the wrong type 'string', expected types '[decimal, bool, int, double, long]'".into()),
            (doc! {FCV: "8.2", "confirm": true, "dryRun": true}, Some(72), "dry-run mode is not enabled".into()),
            (doc! {FCV: "8.2", "confirm": true, "dryRun": false}, Some(72), "dry-run mode is not enabled".into()),
            (doc! {FCV: "8.2", "dryRun": true}, Some(72), "dry-run mode is not enabled".into()),
            (doc! {FCV: "7.0", "confirm": true, "dryRun": true}, Some(4926900), invalid("7.0")),
            (doc! {FCV: "8.2", "confirm": true, "skipDryRun": true}, Some(40415), "BSON field 'setFeatureCompatibilityVersion.skipDryRun' is an unknown field.".into()),
        ];
        for (command, want_code, want_msg) in cases {
            let reply = admin(c, command.clone());
            assert_eq!(code(&reply), want_code, "{command}: {reply}");
            assert_eq!(errmsg(&reply), want_msg, "{command}");
        }
        let reply = on(c, "t", doc! {FCV: "8.2"});
        assert_eq!(code(&reply), Some(13));
        assert_eq!(
            errmsg(&reply),
            "setFeatureCompatibilityVersion may only be run against the admin database."
        );
    });
}

#[test]
fn set_feature_compatibility_version_transitions() {
    with_wt(|c| {
        let fcv = |c: &mut CommandContext| {
            admin(
                c,
                doc! {"getParameter": 1, "featureCompatibilityVersion": 1},
            )
            .get_document("featureCompatibilityVersion")
            .unwrap()
            .get_str("version")
            .unwrap()
            .to_string()
        };
        assert_eq!(fcv(c), "8.2");
        // 8.1, the last continuous version, is reachable from neither.
        let reply = admin(c, doc! {FCV: "8.1", "confirm": true});
        assert_eq!(code(&reply), Some(5147403));
        assert_eq!(
            errmsg(&reply),
            "cannot set featureCompatibilityVersion to '8.1' while featureCompatibilityVersion \
             is '8.2'"
        );
        let reply = admin(c, doc! {FCV: "8.0", "confirm": true});
        assert_eq!(code(&reply), None, "{reply}");
        assert_eq!(fcv(c), "8.0");
        let reply = admin(c, doc! {FCV: "8.1"});
        assert_eq!(code(&reply), Some(7369100));
        assert_eq!(
            errmsg(&reply),
            "Once you have upgraded to 8.1, you will not be able to downgrade FCV and binary \
             version without support assistance. Please re-run this command with 'confirm: \
             true' to acknowledge this and continue with the FCV upgrade."
        );
        let reply = admin(c, doc! {FCV: "8.1", "confirm": true});
        assert_eq!(code(&reply), Some(5147403));
        assert_eq!(
            errmsg(&reply),
            "cannot set featureCompatibilityVersion to '8.1' while featureCompatibilityVersion \
             is '8.0'"
        );
        let reply = admin(c, doc! {FCV: "8.0", "confirm": true});
        assert_eq!(code(&reply), None);
        let reply = admin(c, doc! {FCV: "8.2"});
        assert_eq!(code(&reply), Some(7369100));
        assert!(errmsg(&reply).starts_with("Once you have upgraded to 8.2,"));
        let reply = admin(c, doc! {FCV: "8.2", "confirm": true});
        assert_eq!(code(&reply), None);
        assert_eq!(fcv(c), "8.2");
        // Where mongod keeps it.
        let reply = admin(
            c,
            doc! {"find": "system.version", "filter": {"_id": "featureCompatibilityVersion"}},
        );
        let batch = reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert_eq!(
            batch,
            &vec![Bson::Document(
                doc! {"_id": "featureCompatibilityVersion", "version": "8.2"}
            )]
        );
    });
}

// --------------------------------------------------------------- authorization

/// Access control on, with `name` (a user of `admin`) authenticated.
fn as_user(c: &mut CommandContext, name: &str) {
    let auth = Arc::new(Mutex::new(ConnectionAuth::new()));
    auth.lock()
        .unwrap()
        .authenticated
        .push(("admin".to_string(), name.to_string()));
    c.conn_auth = Some(auth);
    c.require_auth = true;
}

fn as_nobody(c: &mut CommandContext) {
    c.conn_auth = None;
    c.require_auth = false;
}

#[test]
fn who_may_run_them() {
    with_wt(|c| {
        for (name, role) in [
            ("monitor", "clusterMonitor"),
            ("host", "hostManager"),
            ("manager", "clusterManager"),
            ("reader", "read"),
            ("rw", "readWrite"),
        ] {
            let db = if role == "read" || role == "readWrite" {
                "t"
            } else {
                "admin"
            };
            let reply = admin(
                c,
                doc! {"createUser": name, "pwd": "pw", "roles": [{"role": role, "db": db}]},
            );
            assert_eq!(code(&reply), None, "{reply}");
        }
        on(c, "t", doc! {"insert": "c", "documents": [{"_id": 1}]});
        on(c, "t2", doc! {"insert": "c", "documents": [{"_id": 1}]});
        // mongod's built-in roles: `clusterMonitor` has `connPoolStats`
        // on the cluster, `hostManager` has `logRotate`, `clusterManager`
        // has `setDefaultRWConcern` and `setFeatureCompatibilityVersion`,
        // and `read` on a database has `dbHash` on it.
        let table: Vec<(&str, &str, Document, Option<i32>)> = vec![
            ("monitor", "admin", doc! {"connPoolStats": 1}, None),
            ("monitor", "admin", doc! {"logRotate": 1}, Some(13)),
            (
                "monitor",
                "admin",
                doc! {SET: 1, RC: {"level": "local"}},
                Some(13),
            ),
            (
                "monitor",
                "admin",
                doc! {FCV: "8.2", "confirm": true},
                Some(13),
            ),
            ("monitor", "t", doc! {"dbHash": 1}, Some(13)),
            ("host", "admin", doc! {"logRotate": 1}, None),
            ("host", "admin", doc! {"connPoolStats": 1}, Some(13)),
            (
                "host",
                "admin",
                doc! {SET: 1, RC: {"level": "local"}},
                Some(13),
            ),
            (
                "manager",
                "admin",
                doc! {SET: 1, RC: {"level": "local"}},
                None,
            ),
            ("manager", "admin", doc! {FCV: "8.2", "confirm": true}, None),
            ("manager", "admin", doc! {"logRotate": 1}, Some(13)),
            ("manager", "admin", doc! {"connPoolStats": 1}, Some(13)),
            ("reader", "t", doc! {"dbHash": 1}, None),
            ("reader", "t2", doc! {"dbHash": 1}, Some(13)),
            ("reader", "admin", doc! {"connPoolStats": 1}, Some(13)),
            ("rw", "t", doc! {"dbHash": 1, "collections": ["c"]}, None),
            ("rw", "admin", doc! {FCV: "8.2", "confirm": true}, Some(13)),
        ];
        for (user, db, command, want) in table {
            as_user(c, user);
            let reply = on(c, db, command.clone());
            assert_eq!(code(&reply), want, "{user} on {db}: {command}: {reply}");
        }
        as_nobody(c);
    });
}
