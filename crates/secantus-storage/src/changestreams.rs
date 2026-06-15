//! Change-stream projection: oplog entries → MongoDB change events.
//!
//! A faithful port of `secantus.changestreams` (Phase 4 sub-phase 3e). The shape
//! of a change event matches what real `mongod` emits over a change stream: `_id`
//! is an opaque resume token, plus `operationType`, `clusterTime`, `wallTime`,
//! `ns`, `documentKey`, and (per-op) `fullDocument` / `fullDocumentBeforeChange`
//! / `updateDescription`.
//!
//! Resume tokens are `{"_data": "<hex>"}` where the hex bytes are a BSON document
//! carrying `{s: seq, t: ts, n: ns, k: documentKey._id}` — opaque to drivers,
//! enough state for us to resume / detect the invalidate boundary.
//!
//! Pre-image / post-image lookup is the only I/O: [`project`] takes a [`Storage`]
//! and may call `find_matching` for `fullDocument: "updateLookup"` or
//! `read_preimage` for `fullDocumentBeforeChange`.

use bson::{Bson, Document, Timestamp};

use crate::{decode_doc, encode_doc, Result, Storage, StorageError};

/// `fullDocument` / `fullDocumentBeforeChange` modes. Default = no full doc.
pub const FULL_DOC_DEFAULT: &str = "default";
pub const FULL_DOC_UPDATE_LOOKUP: &str = "updateLookup";
pub const FULL_DOC_REQUIRED: &str = "required";
pub const FULL_DOC_WHEN_AVAILABLE: &str = "whenAvailable";
pub const FULL_DOC_OFF: &str = "off";

/// The scope of a watch — which oplog namespaces it surfaces.
#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    /// Whole-cluster change stream (all dbs / collections).
    Cluster,
    /// A single database.
    Db(String),
    /// A single collection.
    Coll { db: String, coll: String },
}

/// The decoded contents of a resume token.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeTokenData {
    pub seq: i64,
    pub ts: Timestamp,
    pub ns: String,
    pub document_key: Document,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Build a `{"_data": "<hex>"}` resume token. Mirrors `make_resume_token`.
pub fn make_resume_token(data: &ResumeTokenData) -> Result<Document> {
    let mut inner = Document::new();
    inner.insert("s", data.seq);
    inner.insert("t", Bson::Timestamp(data.ts));
    inner.insert("n", data.ns.clone());
    inner.insert("k", Bson::Document(data.document_key.clone()));
    let bytes = encode_doc(&inner)?;
    let mut out = Document::new();
    out.insert("_data", hex_encode(&bytes));
    Ok(out)
}

/// Decode a `{"_data": "<hex>"}` resume token. Mirrors `parse_resume_token`.
pub fn parse_resume_token(token: &Document) -> Result<ResumeTokenData> {
    let raw = token
        .get_str("_data")
        .map_err(|_| StorageError::Bson("resume token missing _data".into()))?;
    let bytes =
        hex_decode(raw).ok_or_else(|| StorageError::Bson("resume token _data not hex".into()))?;
    let inner = decode_doc(&bytes)?;
    let ts = match inner.get("t") {
        Some(Bson::Timestamp(t)) => *t,
        _ => {
            return Err(StorageError::Bson(
                "resume token has invalid timestamp".into(),
            ))
        }
    };
    Ok(ResumeTokenData {
        seq: inner
            .get_i64("s")
            .or_else(|_| inner.get_i32("s").map(i64::from))
            .unwrap_or(0),
        ts,
        ns: inner.get_str("n").unwrap_or("").to_string(),
        document_key: inner.get_document("k").cloned().unwrap_or_default(),
    })
}

fn split_ns(ns: &str) -> (String, String) {
    match ns.split_once('.') {
        Some((db, coll)) => (db.to_string(), coll.to_string()),
        None => (ns.to_string(), String::new()),
    }
}

fn scope_matches(ns: &str, scope: &Scope) -> bool {
    match scope {
        Scope::Cluster => true,
        Scope::Db(db) => &split_ns(ns).0 == db,
        Scope::Coll { db, coll } => {
            let (d, c) = split_ns(ns);
            &d == db && &c == coll
        }
    }
}

fn ns_doc(ns: &str) -> Document {
    let (db, coll) = split_ns(ns);
    let mut out = Document::new();
    out.insert("db", db);
    if !coll.is_empty() && coll != "$cmd" {
        out.insert("coll", coll);
    }
    out
}

fn do_lookup(storage: &Storage, db: &str, coll: &str, doc_id: &Bson) -> Result<Option<Document>> {
    let mut filter = Document::new();
    filter.insert("_id", doc_id.clone());
    let docs = storage.find_matching(db, coll, &filter)?;
    match docs.first() {
        Some(blob) => Ok(Some(decode_doc(blob)?)),
        None => Ok(None),
    }
}

fn attach_full_document(
    event: &mut Document,
    op: &str,
    oplog_entry: &Document,
    storage: &Storage,
    mode: &str,
) -> Result<()> {
    if op == "i" {
        if mode != FULL_DOC_OFF {
            if let Ok(o) = oplog_entry.get_document("o") {
                event.insert("fullDocument", Bson::Document(o.clone()));
            }
        }
        return Ok(());
    }
    if event.get_str("operationType") == Ok("replace") {
        // Replacement-style updates carry the full new doc as `o`; the change
        // stream always surfaces it as fullDocument — no updateLookup needed.
        if let Ok(o) = oplog_entry.get_document("o") {
            event.insert("fullDocument", Bson::Document(o.clone()));
        }
        return Ok(());
    }
    if op == "u"
        && (mode == FULL_DOC_UPDATE_LOOKUP
            || mode == FULL_DOC_REQUIRED
            || mode == FULL_DOC_WHEN_AVAILABLE)
    {
        let ns = oplog_entry.get_str("ns").unwrap_or("");
        let (db, coll) = split_ns(ns);
        let doc_id = oplog_entry
            .get_document("o2")
            .ok()
            .and_then(|o2| o2.get("_id").cloned());
        let looked_up = match &doc_id {
            Some(id) => do_lookup(storage, &db, &coll, id)?,
            None => None,
        };
        match looked_up {
            Some(d) => {
                event.insert("fullDocument", Bson::Document(d));
            }
            None if mode == FULL_DOC_REQUIRED => {
                return Err(StorageError::ChangeStreamFatal(
                    "fullDocument required but document not found".into(),
                ));
            }
            None => {
                event.insert("fullDocument", Bson::Null);
            }
        }
    }
    Ok(())
}

fn attach_full_document_before_change(
    event: &mut Document,
    seq: i64,
    storage: &Storage,
    mode: &str,
) -> Result<()> {
    if mode == FULL_DOC_DEFAULT || mode == FULL_DOC_OFF || mode.is_empty() {
        return Ok(());
    }
    match storage.read_preimage(seq)? {
        Some(pre) => {
            event.insert(
                "fullDocumentBeforeChange",
                Bson::Document(decode_doc(&pre)?),
            );
        }
        None if mode == FULL_DOC_REQUIRED => {
            return Err(StorageError::ChangeStreamFatal(
                "fullDocumentBeforeChange required but pre-image not stored".into(),
            ));
        }
        None if mode == FULL_DOC_WHEN_AVAILABLE => {
            event.insert("fullDocumentBeforeChange", Bson::Null);
        }
        None => {}
    }
    Ok(())
}

/// Project one oplog row into a change event, returning `(event,
/// invalidates_after)`. `event` is `None` when the row isn't surfaced (scope
/// mismatch, a `noop` heartbeat, or an "expanded" DDL event the user didn't
/// opt into). `invalidates_after` is `true` when the cursor should emit a final
/// `invalidate` after this event (drop on a watched collection, etc.). Mirrors
/// `changestreams.project`.
pub fn project(
    seq: i64,
    oplog_entry: &Document,
    storage: &Storage,
    full_document_mode: &str,
    full_document_before_change_mode: &str,
    scope: &Scope,
    show_expanded_events: bool,
) -> Result<(Option<Document>, bool)> {
    let op = oplog_entry.get_str("op").unwrap_or("");
    let ns = oplog_entry.get_str("ns").unwrap_or("").to_string();
    let ts = match oplog_entry.get("ts") {
        Some(Bson::Timestamp(t)) => *t,
        _ => return Ok((None, false)),
    };
    let wall = oplog_entry.get("wall").cloned();
    // Periodic noop heartbeats advance cluster time but don't surface as events.
    if op == "n" {
        return Ok((None, false));
    }

    if op == "i" || op == "u" || op == "d" {
        if !scope_matches(&ns, scope) {
            return Ok((None, false));
        }
        let document_key = oplog_entry.get_document("o2").cloned().unwrap_or_else(|_| {
            let mut d = Document::new();
            d.insert("_id", Bson::Null);
            d
        });
        // `op:"u"` is either a `$v:2` diff or a full replacement doc; the change
        // stream distinguishes them as `update` vs `replace`. A diff-shaped `o`
        // always has `$v`, so its absence signals a replacement.
        let mut op_type = match op {
            "i" => "insert",
            "u" => "update",
            _ => "delete",
        };
        if op == "u" {
            if let Ok(o) = oplog_entry.get_document("o") {
                if !o.contains_key("$v") && !o.contains_key("diff") {
                    op_type = "replace";
                }
            }
        }
        let token = make_resume_token(&ResumeTokenData {
            seq,
            ts,
            ns: ns.clone(),
            document_key: document_key.clone(),
        })?;
        let mut event = Document::new();
        event.insert("_id", Bson::Document(token));
        event.insert("operationType", op_type);
        event.insert("clusterTime", Bson::Timestamp(ts));
        event.insert("ns", Bson::Document(ns_doc(&ns)));
        event.insert("documentKey", Bson::Document(document_key));
        if let Some(w) = &wall {
            event.insert("wallTime", w.clone());
        }
        if op == "u" && op_type == "update" {
            let diff = oplog_entry
                .get_document("o")
                .ok()
                .and_then(|o| o.get_document("diff").ok().cloned());
            match diff {
                Some(d) => {
                    event.insert("updateDescription", Bson::Document(d));
                }
                None => {
                    let mut ud = Document::new();
                    ud.insert("updatedFields", Bson::Document(Document::new()));
                    ud.insert("removedFields", Bson::Array(vec![]));
                    ud.insert("truncatedArrays", Bson::Array(vec![]));
                    event.insert("updateDescription", Bson::Document(ud));
                }
            }
        }
        attach_full_document(&mut event, op, oplog_entry, storage, full_document_mode)?;
        attach_full_document_before_change(
            &mut event,
            seq,
            storage,
            full_document_before_change_mode,
        )?;
        return Ok((Some(event), false));
    }

    if op == "c" {
        return project_command(seq, oplog_entry, &ns, ts, wall, scope, show_expanded_events);
    }
    Ok((None, false))
}

/// The `op: "c"` (command) branch of [`project`] — DDL events.
fn project_command(
    seq: i64,
    oplog_entry: &Document,
    ns: &str,
    ts: Timestamp,
    wall: Option<Bson>,
    scope: &Scope,
    show_expanded_events: bool,
) -> Result<(Option<Document>, bool)> {
    let cmd = oplog_entry.get_document("o").cloned().unwrap_or_default();
    let cmd_db = split_ns(ns).0;

    let base = |op_type: &str, affected_ns: &str, extra: &[(&str, Bson)]| -> Result<Document> {
        let token = make_resume_token(&ResumeTokenData {
            seq,
            ts,
            ns: affected_ns.to_string(),
            document_key: Document::new(),
        })?;
        let mut event = Document::new();
        event.insert("_id", Bson::Document(token));
        event.insert("operationType", op_type);
        event.insert("clusterTime", Bson::Timestamp(ts));
        for (k, v) in extra {
            event.insert(*k, v.clone());
        }
        if let Some(w) = &wall {
            event.insert("wallTime", w.clone());
        }
        Ok(event)
    };

    if let Ok(coll) = cmd.get_str("drop") {
        let affected_ns = format!("{cmd_db}.{coll}");
        if !scope_matches(&affected_ns, scope) {
            return Ok((None, false));
        }
        let event = base(
            "drop",
            &affected_ns,
            &[("ns", Bson::Document(ns_doc(&affected_ns)))],
        )?;
        let invalidates = matches!(scope, Scope::Coll { .. });
        return Ok((Some(event), invalidates));
    }
    if cmd.contains_key("dropDatabase") {
        match scope {
            Scope::Coll { .. } => return Ok((None, false)),
            Scope::Db(db) if db != &cmd_db => return Ok((None, false)),
            _ => {}
        }
        let mut nsd = Document::new();
        nsd.insert("db", cmd_db.clone());
        let event = base("dropDatabase", ns, &[("ns", Bson::Document(nsd))])?;
        let invalidates = matches!(scope, Scope::Db(_));
        return Ok((Some(event), invalidates));
    }
    if let Ok(from_ns) = cmd.get_str("renameCollection") {
        if !scope_matches(from_ns, scope) {
            return Ok((None, false));
        }
        let to_ns = cmd.get_str("to").unwrap_or("");
        let (to_db, to_coll) = split_ns(to_ns);
        let mut to = Document::new();
        to.insert("db", to_db);
        if !to_coll.is_empty() {
            to.insert("coll", to_coll);
        }
        let event = base(
            "rename",
            from_ns,
            &[
                ("ns", Bson::Document(ns_doc(from_ns))),
                ("to", Bson::Document(to)),
            ],
        )?;
        let invalidates = matches!(scope, Scope::Coll { .. });
        return Ok((Some(event), invalidates));
    }
    if let Ok(coll) = cmd.get_str("createIndexes") {
        if !show_expanded_events {
            return Ok((None, false));
        }
        let affected_ns = format!("{cmd_db}.{coll}");
        if !scope_matches(&affected_ns, scope) {
            return Ok((None, false));
        }
        let spec = cmd
            .get_array("indexes")
            .ok()
            .and_then(|a| a.first())
            .and_then(|b| b.as_document());
        let indexes = match spec {
            Some(s) => {
                let mut idx = Document::new();
                idx.insert("v", s.get_i32("v").unwrap_or(2));
                idx.insert(
                    "key",
                    Bson::Document(s.get_document("key").cloned().unwrap_or_default()),
                );
                idx.insert("name", s.get_str("name").unwrap_or(""));
                vec![Bson::Document(idx)]
            }
            None => vec![],
        };
        let mut op_desc = Document::new();
        op_desc.insert("indexes", Bson::Array(indexes));
        let event = base(
            "createIndexes",
            &affected_ns,
            &[
                ("ns", Bson::Document(ns_doc(&affected_ns))),
                ("operationDescription", Bson::Document(op_desc)),
            ],
        )?;
        return Ok((Some(event), false));
    }
    if let Ok(coll) = cmd.get_str("dropIndexes") {
        if !show_expanded_events {
            return Ok((None, false));
        }
        let affected_ns = format!("{cmd_db}.{coll}");
        if !scope_matches(&affected_ns, scope) {
            return Ok((None, false));
        }
        let mut idx = Document::new();
        idx.insert("name", cmd.get_str("index").unwrap_or(""));
        let mut op_desc = Document::new();
        op_desc.insert("indexes", Bson::Array(vec![Bson::Document(idx)]));
        let event = base(
            "dropIndexes",
            &affected_ns,
            &[
                ("ns", Bson::Document(ns_doc(&affected_ns))),
                ("operationDescription", Bson::Document(op_desc)),
            ],
        )?;
        return Ok((Some(event), false));
    }
    Ok((None, false))
}

/// The final `invalidate` event a cursor emits after a drop / rename /
/// dropDatabase on its watched scope. Mirrors `changestreams.invalidate_event`.
pub fn invalidate_event(seq: i64, oplog_entry: &Document) -> Result<Document> {
    let ts = match oplog_entry.get("ts") {
        Some(Bson::Timestamp(t)) => *t,
        _ => Timestamp {
            time: 0,
            increment: 0,
        },
    };
    let ns = oplog_entry.get_str("ns").unwrap_or("");
    let cmd = oplog_entry.get_document("o").cloned().unwrap_or_default();
    let affected_ns = if let Ok(coll) = cmd.get_str("drop") {
        format!("{}.{}", split_ns(ns).0, coll)
    } else if let Ok(from) = cmd.get_str("renameCollection") {
        from.to_string()
    } else {
        ns.to_string()
    };
    let token = make_resume_token(&ResumeTokenData {
        seq,
        ts,
        ns: affected_ns,
        document_key: Document::new(),
    })?;
    let mut event = Document::new();
    event.insert("_id", Bson::Document(token));
    event.insert("operationType", "invalidate");
    event.insert("clusterTime", Bson::Timestamp(ts));
    if let Some(w) = oplog_entry.get("wall") {
        event.insert("wallTime", w.clone());
    }
    Ok(event)
}

const SPLIT_THRESHOLD_BYTES: usize = 16 * 1024 * 1024;
const HEAVY_FIELD_BYTES: usize = 1024 * 1024;

/// Split `event` into 16 MB-bounded fragments when over the BSON limit, tagging
/// each with `splitEvent: {fragment, of}`. Always returns at least one event; a
/// small event gets a single `{fragment: 1, of: 1}` fragment (the user's opt-in
/// is honoured by the field's presence). Mirrors `changestreams.stamp_split_event`.
pub fn stamp_split_event(mut event: Document) -> Result<Vec<Document>> {
    let encoded_size = encode_doc(&event)?.len();
    let split_one = |mut e: Document| -> Document {
        let mut se = Document::new();
        se.insert("fragment", 1i32);
        se.insert("of", 1i32);
        e.insert("splitEvent", Bson::Document(se));
        e
    };
    if encoded_size <= SPLIT_THRESHOLD_BYTES {
        return Ok(vec![split_one(event)]);
    }
    // Classify each top-level field by its own BSON size.
    let mut heavy: Vec<String> = Vec::new();
    let mut light: Vec<String> = Vec::new();
    for (k, v) in &event {
        let mut one = Document::new();
        one.insert(k.clone(), v.clone());
        if encode_doc(&one)?.len() > HEAVY_FIELD_BYTES {
            heavy.push(k.clone());
        } else {
            light.push(k.clone());
        }
    }
    if heavy.is_empty() {
        return Ok(vec![split_one(event)]);
    }
    let mut light_meta = Document::new();
    for k in &light {
        light_meta.insert(k.clone(), event.remove(k).unwrap());
    }
    let total = heavy.len() as i32;
    let mut fragments = Vec::with_capacity(heavy.len());
    for (i, hf) in heavy.iter().enumerate() {
        let mut frag = light_meta.clone();
        frag.insert(hf.clone(), event.remove(hf).unwrap());
        let mut se = Document::new();
        se.insert("fragment", (i as i32) + 1);
        se.insert("of", total);
        frag.insert("splitEvent", Bson::Document(se));
        fragments.push(frag);
    }
    Ok(fragments)
}
