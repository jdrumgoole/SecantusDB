//! Reading what is stored in `local.oplog.rs`.
//!
//! The oplog holds mongod's entries (phase 4b of
//! `tasks/mongod-storage-model-plan.md`): a batched insert or delete and a
//! transaction are one `applyOps` entry, an update's `o` is mongod's `$v: 2`
//! delta, a delete has no `o2`, and a retryable write names its session,
//! statement and image in mongod's fields. The readers of the oplog -- change
//! streams, replay and crash recovery, the pre-image lookup -- work on one
//! operation at a time, in the shape the engine was built around: that is what
//! [`unwind`] gives them, an operation per element of an `applyOps` (or the
//! entry itself, when it is not one), with the entry's own fields and the
//! operation's place among them (`applyOpsIndex`).

use bson::{Bson, Document};

/// The field an unwound operation carries its place in its entry in: mongod's
/// `applyOpsIndex` (0 for an entry that is not an `applyOps`).
pub const APPLY_OPS_INDEX: &str = "applyOpsIndex";

/// The field of an unwound operation that says it came out of an `applyOps`
/// entry (a transaction's or a batch's): change streams give such an event a
/// `txnOpIndex` of its own in the resume token.
pub const IN_APPLY_OPS: &str = "inApplyOps";

/// Whether `entry` is an `applyOps` entry: a command on `admin.$cmd` whose
/// `o.applyOps` is an array.
pub fn is_apply_ops(entry: &Document) -> bool {
    entry.get_str("op") == Ok("c")
        && entry
            .get_document("o")
            .is_ok_and(|o| matches!(o.get("applyOps"), Some(Bson::Array(_))))
}

/// The operations of a stored entry: one per element of an `applyOps`, else
/// the entry itself. Each is in the engine's own shape: `op`, `ns`, `ui`, `o`,
/// `o2` of the operation; `ts`, `t`, `v` and `wall` of its entry; the
/// session fields of a transaction's writes (`lsid`, `txnNumber`) or, for a
/// retryable write, `rw: {lsid, txnNumber, stmtId, img}`; and
/// [`APPLY_OPS_INDEX`]. An update's `o.diff` is the `updateDescription` its
/// delta describes (the delta stays as `o.md`), a delete has `o2` (the `_id`
/// its `o` names), and a DDL entry is as the engine writes it.
pub fn unwind(entry: &Document) -> Vec<Document> {
    if !is_apply_ops(entry) {
        let mut op = entry.clone();
        normalise(&mut op);
        add_session(&mut op, entry, None);
        op.insert(APPLY_OPS_INDEX, 0i64);
        return vec![op];
    }
    // `prepare` and `partialTxn` entries and the commit / abort that follow
    // them hold the same elements; a commit or abort holds none.
    let Some(ops) = entry
        .get_document("o")
        .ok()
        .and_then(|o| o.get_array("applyOps").ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(ops.len());
    for (i, el) in ops.iter().enumerate() {
        let Some(el) = el.as_document() else { continue };
        let mut op = Document::new();
        for (k, v) in el {
            if k != "stmtId" && k != "needsRetryImage" {
                op.insert(k.clone(), v.clone());
            }
        }
        for k in ["ts", "t", "v", "wall"] {
            if let Some(v) = entry.get(k) {
                op.insert(k, v.clone());
            }
        }
        normalise(&mut op);
        add_session(&mut op, entry, Some(el));
        op.insert(APPLY_OPS_INDEX, i as i64);
        op.insert(IN_APPLY_OPS, true);
        out.push(op);
    }
    out
}

/// A transaction's write names its session; a retryable write's names its
/// statement (`rw`).
fn add_session(op: &mut Document, entry: &Document, element: Option<&Document>) {
    let (Ok(lsid), Ok(txn)) = (entry.get_document("lsid"), entry.get_i64("txnNumber")) else {
        return;
    };
    let stmt = element
        .and_then(|e| e.get("stmtId"))
        .or_else(|| entry.get("stmtId"));
    let retryable = stmt.is_some() || entry.get_i32("multiOpType").is_ok();
    // The engine's own shape names a retryable write by `rw` alone; the
    // session fields of the entry are not an event's.
    for k in [
        "lsid",
        "txnNumber",
        "stmtId",
        "needsRetryImage",
        "prevOpTime",
        "multiOpType",
    ] {
        if element.is_none() && (retryable || k == "prevOpTime" || k == "multiOpType") {
            op.remove(k);
        }
    }
    if retryable {
        let mut rw = Document::new();
        rw.insert("lsid", lsid.clone());
        rw.insert("txnNumber", txn);
        if let Some(stmt) = stmt {
            rw.insert("stmtId", stmt.clone());
        }
        if let Some(img) = entry.get("needsRetryImage") {
            rw.insert("img", img.clone());
        }
        op.insert("rw", rw);
    } else {
        op.insert("lsid", lsid.clone());
        op.insert("txnNumber", txn);
    }
}

/// A name the oplog holds as the bytes mongod holds -- a database or
/// collection name that is not UTF-8 -- as the engine holds it: escaped
/// (`secantus_core::bsondoc::escape_key`).
fn escape_name(v: &mut Bson) {
    if let Some(bytes) = secantus_core::bsondoc::as_invalid_utf8(v) {
        *v = Bson::String(secantus_core::bsondoc::escape_key(bytes));
    }
}

/// [`escape_name`] for the names of an operation: `ns`, and, in a command,
/// the collection it names and a rename's `to`.
fn escape_names(op: &mut Document) {
    if let Some(v) = op.get_mut("ns") {
        escape_name(v);
    }
    if op.get_str("op") != Ok("c") {
        return;
    }
    let Some(Bson::Document(o)) = op.get_mut("o") else {
        return;
    };
    if let Some(first) = o.keys().next().cloned() {
        if let Some(v) = o.get_mut(&first) {
            escape_name(v);
        }
    }
    if let Some(v) = o.get_mut("to") {
        escape_name(v);
    }
}

/// An operation in the shape the engine reads: see [`unwind`].
fn normalise(op: &mut Document) {
    escape_names(op);
    match op.get_str("op") {
        Ok("u") => {
            let Some(Bson::Document(o)) = op.get("o") else {
                return;
            };
            if o.get_i32("$v") != Ok(2) {
                return;
            }
            let Ok(diff) = o.get_document("diff") else {
                return;
            };
            if let Some(desc) = secantus_core::diff::update_description_from_oplog_diff(diff) {
                let md = diff.clone();
                op.insert("o", bson::doc! {"$v": 2i32, "diff": desc, "md": md});
            }
        }
        Ok("d") => {
            if !op.contains_key("o2") {
                if let Some(id) = op
                    .get_document("o")
                    .ok()
                    .and_then(|o| o.get("_id"))
                    .cloned()
                {
                    op.insert("o2", bson::doc! {"_id": id});
                }
            }
        }
        Ok("c") => normalise_command(op),
        _ => {}
    }
}

/// A command entry as mongod writes it, back to the shape replay reads: a
/// one-index `createIndexes` holds the spec itself, a `dropIndexes` keeps its
/// spec in `o2`.
fn normalise_command(op: &mut Document) {
    let Some(Bson::Document(o)) = op.get("o").cloned() else {
        return;
    };
    // A two-phase index build: mongod logs `startIndexBuild`, then
    // `commitIndexBuild` once the index is ready, and replay builds it at the
    // commit. The engine reads the commit as the one `createIndexes` it was,
    // and the start as nothing.
    if o.contains_key("startIndexBuild") {
        op.insert("op", "n");
        op.insert("o", bson::doc! {"msg": "startIndexBuild"});
        return;
    }
    if let (Ok(coll), Ok(indexes)) = (o.get_str("commitIndexBuild"), o.get_array("indexes")) {
        op.insert(
            "o",
            bson::doc! {"createIndexes": coll, "indexes": indexes.clone()},
        );
        return;
    }
    if let Ok(coll) = o.get_str("createIndexes") {
        if !o.contains_key("indexes") {
            let mut spec = Document::new();
            for (k, v) in &o {
                if k != "createIndexes" {
                    spec.insert(k.clone(), v.clone());
                }
            }
            op.insert(
                "o",
                bson::doc! {"createIndexes": coll, "indexes": [Bson::Document(spec)]},
            );
        }
    } else if let Ok(coll) = o.get_str("dropIndexes") {
        if let Ok(spec) = op.get_document("o2").cloned() {
            let mut n = bson::doc! {"dropIndexes": coll};
            if let Some(index) = o.get("index") {
                n.insert("index", index.clone());
            }
            n.insert("spec", Bson::Document(spec));
            op.insert("o", n);
            op.remove("o2");
        }
    }
}
