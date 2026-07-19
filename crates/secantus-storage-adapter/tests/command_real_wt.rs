//! Real-WiredTiger integration tests for a focused set of command-layer fixes.
//!
//! The command crate has no in-memory storage double any more: every command
//! test drives the real `dispatch` path over a real `WtStorage` (via
//! `StorageAdapter`), so the handler logic AND the storage semantics are
//! exercised together — without the wire/TCP layer. The full per-module command
//! coverage lives alongside this file in `command_{find,crud,findandmodify,
//! admin,aggregate,distinct}_wt.rs`; these are the cross-cutting fixes.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bson::{doc, Bson, Document};
use secantus_commands::{dispatch, CommandContext, CursorRegistry, Storage as CmdStorage};
use secantus_storage::Storage as WtStorage;
use secantus_storage_adapter::StorageAdapter;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Dispatch, then merge any raw-BSON cursor batch (`ctx.pending_batch`, set by a
/// no-projection `find` / non-tailable `getMore`) back into the reply's
/// `cursor.<field>` so in-process tests can read it inline. The real server
/// splices those blobs onto the wire instead.
fn dispatch_full(cmd: &Document, c: &mut CommandContext) -> Document {
    let mut reply = dispatch(cmd, c);
    if let Some(pb) = c.pending_batch.take() {
        let arr: Vec<Bson> = pb
            .batch
            .iter()
            .map(|b| Bson::Document(Document::from_reader(&mut &b[..]).unwrap()))
            .collect();
        if let Ok(cursor) = reply.get_document_mut("cursor") {
            let mut rebuilt = Document::new();
            rebuilt.insert(pb.batch_field, arr);
            for (k, v) in cursor.iter() {
                rebuilt.insert(k, v.clone());
            }
            *cursor = rebuilt;
        }
    }
    reply
}

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-cmdwt-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `body` with a `CommandContext` (db `t`) backed by a fresh real-WT store.
fn with_wt(body: impl FnOnce(&mut CommandContext)) {
    let dir = temp_home();
    let wt = Arc::new(WtStorage::open(dir.to_str().unwrap()).unwrap());
    let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(wt));
    let mut ctx = CommandContext::new(1)
        .with_storage(adapter)
        .with_cursors(Arc::new(CursorRegistry::new()));
    ctx.db_name = "t".into();
    body(&mut ctx);
    drop(ctx); // release the Arc<WtStorage> so the WT connection closes
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dbstats_lowercase_alias_real_wt() {
    with_wt(|c| {
        let r = dispatch(&doc! {"dbstats": 1}, c);
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        assert_eq!(r.get_str("db").unwrap(), "t");
    });
}

#[test]
fn long_db_name_invalid_namespace_real_wt() {
    with_wt(|c| {
        c.db_name = "d".repeat(64);
        let r = dispatch(&doc! {"ping": 1}, c);
        assert_eq!(r.get_i32("code").unwrap(), 73);
        assert_eq!(r.get_str("codeName").unwrap(), "InvalidNamespace");
    });
}

#[test]
fn find_non_numeric_batch_size_is_type_mismatch_real_wt() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        let r = dispatch_full(&doc! {"find": "c", "batchSize": "foo"}, c);
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_i32("code").unwrap(), 14);
    });
}

#[test]
fn decimal128_batch_size_opens_cursor_real_wt() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}, {"_id": 3}]},
            c,
        );
        let bs = Bson::Decimal128(bson::Decimal128::from_str("2").unwrap());
        let r = dispatch_full(&doc! {"find": "c", "batchSize": bs}, c);
        let cur = r.get_document("cursor").unwrap();
        assert_eq!(cur.get_array("firstBatch").unwrap().len(), 2);
        assert_ne!(cur.get_i64("id").unwrap(), 0, "remaining doc ⇒ live cursor");
    });
}

#[test]
fn list_indexes_batch_size_opens_cursor_real_wt() {
    with_wt(|c| {
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 1}]}, c);
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"a": 1}, "name": "a_1"},
                {"key": {"b": 1}, "name": "b_1"},
            ]},
            c,
        );
        // _id_ + a_1 + b_1 = 3 indexes; batchSize 2 ⇒ 2 + a live cursor.
        let r = dispatch(&doc! {"listIndexes": "c", "cursor": {"batchSize": 2}}, c);
        let cur = r.get_document("cursor").unwrap();
        assert_eq!(cur.get_array("firstBatch").unwrap().len(), 2);
        assert_ne!(cur.get_i64("id").unwrap(), 0);
    });
}

#[test]
fn drop_kills_open_cursor_real_wt() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [
                {"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 4}, {"_id": 5}
            ]},
            c,
        );
        let r = dispatch_full(&doc! {"find": "c", "batchSize": 2}, c);
        let cid = r.get_document("cursor").unwrap().get_i64("id").unwrap();
        assert_ne!(cid, 0);
        dispatch(&doc! {"drop": "c"}, c);
        // The dropped collection's cursor is gone ⇒ getMore is CursorNotFound (43).
        let r = dispatch_full(&doc! {"getMore": cid, "collection": "c"}, c);
        assert_eq!(r.get_i32("code").unwrap(), 43);
    });
}

#[test]
fn insert_validation_failure_carries_err_info_real_wt() {
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "validator": {"x": {"$type": "string"}}},
            c,
        );
        let r = dispatch(&doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}]}, c);
        let we = r.get_array("writeErrors").unwrap();
        let e = we[0].as_document().unwrap();
        assert_eq!(e.get_i32("code").unwrap(), 121);
        let info = e.get_document("errInfo").unwrap();
        assert_eq!(info.get_i32("failingDocumentId").unwrap(), 1);
        assert!(info.get_document("details").is_ok());
    });
}

#[test]
fn out_stage_enforces_target_validator_real_wt() {
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "src", "documents": [{"_id": 1, "n": 5}]},
            c,
        );
        dispatch(&doc! {"create": "dst", "validator": {"n": {"$gt": 100}}}, c);
        let r = dispatch(
            &doc! {"aggregate": "src", "pipeline": [{"$out": "dst"}], "cursor": {}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_i32("code").unwrap(), 121);
        // bypassDocumentValidation lets the write through.
        let r = dispatch(
            &doc! {"aggregate": "src", "pipeline": [{"$out": "dst"}],
            "bypassDocumentValidation": true, "cursor": {}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
    });
}

#[test]
fn find_and_modify_validation_failure_carries_err_info_real_wt() {
    with_wt(|c| {
        dispatch(
            &doc! {"create": "c", "validator": {"x": {"$type": "string"}}},
            c,
        );
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": "foo"}]},
            c,
        );
        let r = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"x": 1}}},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 121);
        let info = r.get_document("errInfo").unwrap();
        assert_eq!(info.get_i32("failingDocumentId").unwrap(), 1);
    });
}

#[test]
fn collmod_prepare_unique_then_unique_conversion_real_wt() {
    // Full end-to-end on real WT, resolving the index by *key pattern* against
    // the real stored key (the FakeStorage unit test could only resolve by name).
    with_wt(|c| {
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 1}]},
            c,
        );
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [{"key": {"x": 1}, "name": "x_1"}]},
            c,
        );
        // prepareUnique arms the index.
        let r = dispatch(
            &doc! {"collMod": "c", "index": {"keyPattern": {"x": 1}, "prepareUnique": true}},
            c,
        );
        assert_eq!(r.get_f64("ok").unwrap(), 1.0);
        // A new duplicate is now rejected with 11000.
        let r = dispatch(&doc! {"insert": "c", "documents": [{"_id": 3, "x": 1}]}, c);
        let we = r.get_array("writeErrors").unwrap();
        assert_eq!(we[0].as_document().unwrap().get_i32("code").unwrap(), 11000);
        // unique:true over the pre-existing duplicates is refused with violations.
        let r = dispatch(
            &doc! {"collMod": "c", "index": {"keyPattern": {"x": 1}, "unique": true}},
            c,
        );
        assert_eq!(r.get_i32("code").unwrap(), 359);
        let v = r.get_array("violations").unwrap();
        assert_eq!(
            v[0].as_document().unwrap().get_array("ids").unwrap(),
            &vec![Bson::Int32(1), Bson::Int32(2)]
        );
    });
}
