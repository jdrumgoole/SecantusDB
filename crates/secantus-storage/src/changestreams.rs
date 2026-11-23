//! Change-stream projection: oplog entries → MongoDB change events.
//!
//! A faithful port of `secantus.changestreams` (Phase 4 sub-phase 3e). The shape
//! of a change event matches what real `mongod` emits over a change stream: `_id`
//! is an opaque resume token, plus `operationType`, `clusterTime`, `wallTime`,
//! `ns`, `documentKey`, and (per-op) `fullDocument` / `fullDocumentBeforeChange`
//! / `updateDescription`.
//!
//! Resume tokens are mongod 8.2.11's own: `{_data: "<hex>", _typeBits?}`, the
//! KeyString of `{clusterTime, version, tokenType, txnOpIndex, fromInvalidate,
//! uuid, eventIdentifier, fragmentNum?}` ([`ResumeTokenData`]). A position in
//! the oplog is a timestamp, so `clusterTime` is the position, and a resume
//! finds the event by it, `txnOpIndex`, the collection and the identifier.
//!
//! Pre-image / post-image lookup is the only I/O: [`project`] takes a [`Storage`]
//! and may call `find_matching` for `fullDocument: "updateLookup"` or
//! `read_pre_image_of` (`config.system.preimages`) for
//! `fullDocumentBeforeChange`.

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

/// `tokenType` of the token of an event: it names a place in the stream, and
/// the event there must be the first one a resumed stream finds.
pub const TOKEN_TYPE_EVENT: i32 = 128;
/// `tokenType` of a high-water mark: only a time (`postBatchResumeToken` of a
/// batch that ended on no event of the stream).
pub const TOKEN_TYPE_HIGH_WATER_MARK: i32 = 0;
/// The version of the tokens mongod 8.2 makes.
pub const TOKEN_VERSION: i32 = 2;

/// The contents of a resume token, as mongod 8.2.11's `ResumeTokenData`
/// (`src/mongo/db/pipeline/resume_token.cpp`): its `_data` is the KeyString
/// (V1, all ascending) of these fields in this order, and `_typeBits` beside
/// it is that KeyString's TypeBits when they are not all zero.
///
/// ```text
/// clusterTime  Timestamp   the oplog entry's `ts`: a stream position
/// version      int         2
/// tokenType    int         128 an event, 0 a high-water mark
/// txnOpIndex   int         the event's place in its `applyOps` entry
/// fromInvalidate  bool     true for the token of an `invalidate`
/// uuid         BinData 4   the collection's UUID, or null (none for a
///                          high-water mark or `dropDatabase`)
/// eventIdentifier  object  {operationType, documentKey | operationDescription},
///                          of an event token only
/// fragmentNum  int         a fragment of a split event, numbered from 0
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeTokenData {
    pub cluster_time: Timestamp,
    pub version: i32,
    pub token_type: i32,
    pub op_index: i64,
    pub from_invalidate: bool,
    pub uuid: Option<Vec<u8>>,
    pub event_identifier: Option<Document>,
    pub fragment: Option<i32>,
}

impl ResumeTokenData {
    /// The token of an event of the entry `cluster_time`.
    pub fn event(
        cluster_time: Timestamp,
        op_index: Option<i64>,
        uuid: Option<Vec<u8>>,
        event_identifier: Document,
    ) -> ResumeTokenData {
        ResumeTokenData {
            cluster_time,
            version: TOKEN_VERSION,
            token_type: TOKEN_TYPE_EVENT,
            op_index: op_index.unwrap_or(0),
            from_invalidate: false,
            uuid,
            event_identifier: Some(event_identifier),
            fragment: None,
        }
    }

    /// A high-water mark at `cluster_time`.
    pub fn high_water_mark(cluster_time: Timestamp) -> ResumeTokenData {
        ResumeTokenData {
            cluster_time,
            version: TOKEN_VERSION,
            token_type: TOKEN_TYPE_HIGH_WATER_MARK,
            op_index: 0,
            from_invalidate: false,
            uuid: None,
            event_identifier: None,
            fragment: None,
        }
    }

    /// Whether this is the token of an event (not a high-water mark).
    pub fn is_event(&self) -> bool {
        self.token_type == TOKEN_TYPE_EVENT
    }

    /// The stream position of the oplog entry the token names: its `ts` as
    /// the oplog's RecordId.
    pub fn position(&self) -> i64 {
        ts_position(self.cluster_time)
    }
}

/// An oplog position (a row's RecordId, `secs << 32 | inc`) from its time.
pub fn ts_position(t: Timestamp) -> i64 {
    (((t.time as u64) << 32) | t.increment as u64).min(i64::MAX as u64) as i64
}

/// The time of an oplog position.
pub fn position_ts(position: i64) -> Timestamp {
    Timestamp {
        time: (position as u64 >> 32) as u32,
        increment: position as u32,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
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

fn token_err(msg: impl Into<String>) -> StorageError {
    StorageError::Bson(msg.into())
}

/// Build a resume token, `{_data: "<hex>", _typeBits?: BinData}`, as mongod's
/// `ResumeToken::toDocument` does.
pub fn make_resume_token(data: &ResumeTokenData) -> Result<Document> {
    use secantus_core::keystring::{Builder, Ordering, Version};
    let mut b = Builder::new(Version::V1, Ordering::all_ascending());
    let k = |e: secantus_core::keystring::Error| token_err(format!("resume token: {e}"));
    b.append_bson_element(&Bson::Timestamp(data.cluster_time))
        .map_err(k)?;
    b.append_bson_element(&Bson::Int32(data.version))
        .map_err(k)?;
    b.append_bson_element(&Bson::Int32(data.token_type))
        .map_err(k)?;
    b.append_bson_element(&Bson::Int32(data.op_index as i32))
        .map_err(k)?;
    b.append_bson_element(&Bson::Boolean(data.from_invalidate))
        .map_err(k)?;
    b.append_bson_element(&match &data.uuid {
        Some(u) => Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Uuid,
            bytes: u.clone(),
        }),
        None => Bson::Null,
    })
    .map_err(k)?;
    if let Some(ident) = &data.event_identifier {
        b.append_bson_element(&Bson::Document(ident.clone()))
            .map_err(k)?;
    }
    if let Some(f) = data.fragment {
        b.append_bson_element(&Bson::Int32(f)).map_err(k)?;
    }
    let (key, type_bits) = b.finish().map_err(k)?;
    let mut out = Document::new();
    out.insert("_data", hex_encode(&key));
    if !type_bits.is_all_zeros() {
        out.insert(
            "_typeBits",
            Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::Generic,
                bytes: type_bits.serialize(),
            }),
        );
    }
    Ok(out)
}

/// Decode a resume token in mongod 8.2's form (version 2). An older version,
/// or bytes that are not a token, are an error: mongod's own answer for those
/// is `secantus-commands`' `resume_keystring`.
pub fn parse_resume_token(token: &Document) -> Result<ResumeTokenData> {
    use secantus_core::keystring::{to_bson, Ordering, TypeBits, Version};
    let raw = token
        .get_str("_data")
        .map_err(|_| token_err("resume token missing _data"))?;
    let bytes = hex_decode(raw).ok_or_else(|| token_err("resume token _data not hex"))?;
    let type_bits = match token.get("_typeBits") {
        None => TypeBits::new(Version::V1),
        Some(Bson::Binary(b)) => TypeBits::from_buffer(Version::V1, &b.bytes)
            .map_err(|e| token_err(format!("resume token _typeBits: {e}")))?,
        Some(_) => return Err(token_err("resume token _typeBits of wrong type")),
    };
    let elems = to_bson(&bytes, Ordering::all_ascending(), &type_bits)
        .map_err(|e| token_err(format!("resume token: {e}")))?;
    let int = |e: Option<&Bson>| match e {
        Some(Bson::Int32(n)) => Some(i64::from(*n)),
        _ => None,
    };
    let mut it = elems.iter();
    let bad = || token_err("resume token is not a version 2 token");
    let Some(Bson::Timestamp(cluster_time)) = it.next() else {
        return Err(bad());
    };
    if int(it.next()) != Some(i64::from(TOKEN_VERSION)) {
        return Err(bad());
    }
    let token_type = int(it.next()).ok_or_else(bad)? as i32;
    if token_type != TOKEN_TYPE_EVENT && token_type != TOKEN_TYPE_HIGH_WATER_MARK {
        return Err(bad());
    }
    let op_index = int(it.next()).ok_or_else(bad)?;
    let Some(Bson::Boolean(from_invalidate)) = it.next() else {
        return Err(bad());
    };
    let uuid = match it.next() {
        Some(Bson::Null) => None,
        Some(Bson::Binary(b))
            if b.subtype == bson::spec::BinarySubtype::Uuid && b.bytes.len() == 16 =>
        {
            Some(b.bytes.clone())
        }
        _ => return Err(bad()),
    };
    let event_identifier = match (token_type, it.next()) {
        (TOKEN_TYPE_HIGH_WATER_MARK, None) => None,
        (TOKEN_TYPE_EVENT, Some(Bson::Document(d))) => Some(d.clone()),
        _ => return Err(bad()),
    };
    let fragment = match it.next() {
        None => None,
        Some(Bson::Int32(n)) if *n >= 0 && event_identifier.is_some() => Some(*n),
        _ => return Err(bad()),
    };
    if it.next().is_some() {
        return Err(bad());
    }
    Ok(ResumeTokenData {
        cluster_time: *cluster_time,
        version: TOKEN_VERSION,
        token_type,
        op_index,
        from_invalidate: *from_invalidate,
        uuid,
        event_identifier,
        fragment,
    })
}

/// mongod's `eventIdentifier`: the event's `operationType` and what tells
/// the event from its neighbours: the `documentKey` of a write, the
/// `operationDescription` of a command (the one `showExpandedEvents` shows,
/// which a token carries whether or not the stream does).
fn event_identifier(
    op_type: &str,
    document_key: Option<&Document>,
    op_description: Option<&Document>,
) -> Document {
    let mut d = Document::new();
    d.insert("operationType", op_type);
    if let Some(k) = document_key {
        d.insert("documentKey", Bson::Document(k.clone()));
    }
    if let Some(o) = op_description {
        d.insert("operationDescription", Bson::Document(o.clone()));
    }
    d
}

/// The collection UUID an entry names: its `ui`, as a token holds it.
fn entry_uuid(entry: &Document) -> Option<Vec<u8>> {
    match entry.get("ui") {
        Some(Bson::Binary(b)) if b.bytes.len() == 16 => Some(b.bytes.clone()),
        _ => None,
    }
}

/// A high-water-mark token at `position` (an oplog position, a time).
pub fn make_high_water_mark_token(position: i64) -> Result<Document> {
    make_resume_token(&ResumeTokenData::high_water_mark(position_ts(position)))
}

/// The place of `op` (an unwound operation) in its `applyOps` entry, when it
/// came out of one.
fn op_index_of(op: &Document) -> Option<i64> {
    op.get_bool(crate::oplog_entry::IN_APPLY_OPS)
        .unwrap_or(false)
        .then(|| op.get_i64(crate::oplog_entry::APPLY_OPS_INDEX).unwrap_or(0))
}

fn split_ns(ns: &str) -> (String, String) {
    match ns.split_once('.') {
        Some((db, coll)) => (db.to_string(), coll.to_string()),
        None => (ns.to_string(), String::new()),
    }
}

fn scope_matches(ns: &str, scope: &Scope) -> bool {
    match scope {
        // A cluster stream leaves the internal databases out.
        Scope::Cluster => !matches!(split_ns(ns).0.as_str(), "admin" | "local" | "config"),
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
    // A name that is not UTF-8, held escaped, as the string of its bytes
    // (RM-222).
    out.insert("db", secantus_core::bsondoc::key_string(db));
    if !coll.is_empty() && coll != "$cmd" {
        out.insert("coll", secantus_core::bsondoc::key_string(coll));
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

/// mongod's wording for a missing required pre-/post-image.
///
/// Verbatim from 6.0.16 (measured 2026-08-29), including the `Executor error
/// during getMore` wrapper it adds when the condition is hit while draining a
/// cursor and the shell-style rendering of the offending event. Mirrors the
/// Python server's `_required_image_message`.
fn required_image_message(event: &Document, pre: bool) -> String {
    let (secs, inc) = match event.get("clusterTime") {
        Some(Bson::Timestamp(ts)) => (ts.time, ts.increment),
        _ => (0, 0),
    };
    let ns = event.get_document("ns").ok();
    let db = ns.and_then(|d| d.get_str("db").ok()).unwrap_or("");
    let coll = ns.and_then(|d| d.get_str("coll").ok()).unwrap_or("");
    let op = event.get_str("operationType").unwrap_or("");
    let summary = format!(
        "{{operationType: \"{op}\", ns: {{db: \"{db}\", coll: \"{coll}\"}}, \
         clusterTime: Timestamp({secs}, {inc})}}"
    );
    let what = if pre {
        "a pre-image for all update, delete and replace events, but the pre-image was not found"
    } else {
        "a post-image for all update events, but the post-image was not found"
    };
    format!(
        "Executor error during getMore :: caused by :: Change stream was configured to \
         require {what} for event: {summary}"
    )
}

fn attach_full_document(
    event: &mut Document,
    seq: i64,
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
        // `required` / `whenAvailable` serve the post-image as the update left
        // it: mongod 8.2.11 applies the update to the pre-image it stored when
        // the update ran, so a later write or delete does not change it, and
        // whether images are enabled when the stream is READ does not matter.
        // With no stored pre-image, `whenAvailable` is null and `required`
        // fails the stream (47). Only `updateLookup` reads the document now.
        let looked_up = if mode == FULL_DOC_UPDATE_LOOKUP {
            let ns = oplog_entry.get_str("ns").unwrap_or("");
            let (db, coll) = split_ns(ns);
            match oplog_entry
                .get_document("o2")
                .ok()
                .and_then(|o2| o2.get("_id"))
            {
                Some(id) => do_lookup(storage, &db, &coll, id)?,
                None => None,
            }
        } else {
            stored_post_image(seq, oplog_entry, storage)?
        };
        match looked_up {
            Some(d) => {
                event.insert("fullDocument", Bson::Document(d));
            }
            None if mode == FULL_DOC_REQUIRED => {
                return Err(StorageError::ChangeStreamFatal(required_image_message(
                    event, false,
                )));
            }
            None => {
                event.insert("fullDocument", Bson::Null);
            }
        }
    }
    Ok(())
}

/// An update's post-image: the pre-image stored under `seq` with the entry's
/// update description applied, or `None` when no pre-image was stored.
fn stored_post_image(
    seq: i64,
    oplog_entry: &Document,
    storage: &Storage,
) -> Result<Option<Document>> {
    let Some(pre) = storage.read_pre_image_of(seq, oplog_entry)? else {
        return Ok(None);
    };
    let Ok(diff) = oplog_entry
        .get_document("o")
        .and_then(|o| o.get_document("diff"))
    else {
        return Ok(None);
    };
    secantus_core::diff::apply_update_description(decode_doc(&pre)?, diff)
        .map(Some)
        .map_err(|_| StorageError::Internal("change stream: post-image diff did not apply".into()))
}

fn attach_full_document_before_change(
    event: &mut Document,
    seq: i64,
    oplog_entry: &Document,
    storage: &Storage,
    mode: &str,
) -> Result<()> {
    if mode == FULL_DOC_DEFAULT || mode == FULL_DOC_OFF || mode.is_empty() {
        return Ok(());
    }
    match storage.read_pre_image_of(seq, oplog_entry)? {
        Some(pre) => {
            event.insert(
                "fullDocumentBeforeChange",
                Bson::Document(decode_doc(&pre)?),
            );
        }
        None if mode == FULL_DOC_REQUIRED => {
            return Err(StorageError::ChangeStreamFatal(required_image_message(
                event, true,
            )));
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
#[allow(clippy::too_many_arguments)]
/// mongod's field order for a change event, measured against 6.0.16 on
/// 2026-08-29 with `tools/probes/change_streams.py`. Order is invisible to a
/// document comparison, which is how nine construction sites drifted out of it
/// unnoticed. Keys not listed keep their relative position after these.
/// Mirrors the Python server's `_EVENT_FIELD_ORDER`.
const EVENT_FIELD_ORDER: &[&str] = &[
    "txnNumber",
    "lsid",
    "_id",
    "operationType",
    "clusterTime",
    "collectionUUID",
    "wallTime",
    "fullDocument",
    "ns",
    "to",
    "documentKey",
    "operationDescription",
    "updateDescription",
    "stateBeforeChange",
    "fullDocumentBeforeChange",
    "nsType",
];

/// Rebuild an event in mongod's field order. Applied once at projection's exit
/// rather than by constructing each event type in order.
fn order_event_fields(event: Document) -> Document {
    let mut out = Document::new();
    // `rename` is the one event where mongod does NOT lead with `_id`: `to`
    // comes first, before the resume token. Measured on 6.0.16 and 8.2.11, with
    // and without showExpandedEvents. Mirrors `src/secantus/changestreams.py`.
    let to_leads = event.get_str("operationType") == Ok("rename") && event.contains_key("to");
    if to_leads {
        if let Some(v) = event.get("to") {
            out.insert("to", v.clone());
        }
    }
    for key in EVENT_FIELD_ORDER {
        if to_leads && *key == "to" {
            continue;
        }
        if let Some(v) = event.get(*key) {
            out.insert(*key, v.clone());
        }
    }
    for (k, v) in event.iter() {
        if !EVENT_FIELD_ORDER.contains(&k.as_str()) {
            out.insert(k, v.clone());
        }
    }
    out
}

pub fn project(
    seq: i64,
    oplog_entry: &Document,
    storage: &Storage,
    full_document_mode: &str,
    full_document_before_change_mode: &str,
    scope: &Scope,
    show_expanded_events: bool,
) -> Result<(Option<Document>, bool)> {
    let (event, invalidates) = project_unordered(
        seq,
        oplog_entry,
        storage,
        full_document_mode,
        full_document_before_change_mode,
        scope,
        show_expanded_events,
    )?;
    let event = event.map(|mut ev| {
        // `showExpandedEvents` puts the collection's UUID on every event that
        // HAS a collection -- CRUD and the command events alike (probed on
        // 8.2.11: create / createIndexes / dropIndexes / collMod / drop /
        // rename all carry it). `invalidate` does NOT, even though it derives
        // from an entry that does, so it is excluded explicitly. The CRUD path
        // sets it earlier; this fills in the rest. Mirrors
        // `src/secantus/changestreams.py`.
        if show_expanded_events
            && ev.get_str("operationType") != Ok("invalidate")
            && !ev.contains_key("collectionUUID")
        {
            if let Some(ui) = oplog_entry.get("ui") {
                ev.insert("collectionUUID", ui.clone());
            }
        }
        order_event_fields(ev)
    });
    Ok((event, invalidates))
}

fn project_unordered(
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
    // A namespace whose name is not UTF-8 (held escaped, with NULs in it)
    // never matches mongod's namespace regex: PCRE2 refuses to match bytes
    // that are not UTF-8, so a database or cluster stream shows no event for
    // it (measured 8.2.11, 2026-10-27; RM-222). A stream on such a
    // collection is refused when it opens.
    let target = oplog_entry
        .get_document("o")
        .ok()
        .filter(|_| op == "c")
        .and_then(|o| o.values().next())
        .and_then(Bson::as_str)
        .unwrap_or("");
    if ns.contains('\0') || target.contains('\0') {
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
        let token = make_resume_token(&ResumeTokenData::event(
            ts,
            op_index_of(oplog_entry),
            entry_uuid(oplog_entry),
            event_identifier(op_type, Some(&document_key), None),
        ))?;
        let mut event = Document::new();
        // A write made in a transaction names it, ahead of the token (mongod
        // 8.2.11). A retryable write's session is not shown.
        if let (Some(n), Some(l)) = (oplog_entry.get("txnNumber"), oplog_entry.get("lsid")) {
            event.insert("txnNumber", n.clone());
            event.insert("lsid", l.clone());
        }
        event.insert("_id", Bson::Document(token));
        event.insert("operationType", op_type);
        event.insert("clusterTime", Bson::Timestamp(ts));
        // wallTime sits immediately after clusterTime, before ns (mongod
        // 6.0.16, measured 2026-08-29).
        if let Some(w) = &wall {
            event.insert("wallTime", w.clone());
        }
        event.insert("ns", Bson::Document(ns_doc(&ns)));
        event.insert("documentKey", Bson::Document(document_key));
        // showExpandedEvents surfaces the collection UUID on CRUD events (mongod
        // 6.0+). The oplog entry carries it as `ui` (BinData 4).
        if show_expanded_events {
            if let Some(ui) = oplog_entry.get("ui") {
                event.insert("collectionUUID", ui.clone());
            }
        }
        if op == "u" && op_type == "update" {
            let diff = oplog_entry
                .get_document("o")
                .ok()
                .and_then(|o| o.get_document("diff").ok().cloned());
            let mut ud = match diff {
                Some(d) => d,
                None => {
                    let mut ud = Document::new();
                    ud.insert("updatedFields", Bson::Document(Document::new()));
                    ud.insert("removedFields", Bson::Array(vec![]));
                    ud.insert("truncatedArrays", Bson::Array(vec![]));
                    ud
                }
            };
            // mongod (probed 7.0.12): under showExpandedEvents the update
            // description always carries `disambiguatedPaths` — an empty doc
            // when nothing was ambiguous (the diff writer only stores the key
            // when non-empty); without the flag the key is absent.
            if show_expanded_events {
                if !ud.contains_key("disambiguatedPaths") {
                    ud.insert("disambiguatedPaths", Bson::Document(Document::new()));
                }
            } else {
                ud.remove("disambiguatedPaths");
            }
            event.insert("updateDescription", Bson::Document(ud));
        }
        attach_full_document(
            &mut event,
            seq,
            op,
            oplog_entry,
            storage,
            full_document_mode,
        )?;
        // An insert has no "before" -- mongod omits `fullDocumentBeforeChange`
        // from insert events in every mode, `required` included (measured
        // 8.2.11, 2026-10-01). This put `fullDocumentBeforeChange: null` on
        // each insert under `whenAvailable`.
        if op != "i" {
            attach_full_document_before_change(
                &mut event,
                seq,
                oplog_entry,
                storage,
                full_document_before_change_mode,
            )?;
        }
        // Splitting (splitLargeChangeStreamEvents / $changeStreamSplitLargeEvent)
        // is applied by the caller via `stamp_split_event` so one over-16MB event
        // can expand into several fragments — mirrors `commands.py`'s producer
        // applying `stamp_split_event` to each projected event.
        return Ok((Some(event), false));
    }

    if op == "c" {
        return project_command(oplog_entry, &ns, ts, wall, scope, show_expanded_events);
    }
    Ok((None, false))
}

/// The `op: "c"` (command) branch of [`project`] — DDL events.
fn project_command(
    oplog_entry: &Document,
    ns: &str,
    ts: Timestamp,
    wall: Option<Bson>,
    scope: &Scope,
    show_expanded_events: bool,
) -> Result<(Option<Document>, bool)> {
    let cmd = oplog_entry.get_document("o").cloned().unwrap_or_default();
    let cmd_db = split_ns(ns).0;

    // `ident_description`: the `operationDescription` the token's identifier
    // names, when the event does not show it (a rename's, on a stream that
    // does not ask for expanded events); else the event's own.
    let base = |op_type: &str,
                extra: &[(&str, Bson)],
                ident_description: Option<&Document>|
     -> Result<Document> {
        let description = ident_description.or_else(|| {
            extra.iter().find_map(|(k, v)| match (*k, v) {
                ("operationDescription", Bson::Document(d)) => Some(d),
                _ => None,
            })
        });
        let token = make_resume_token(&ResumeTokenData::event(
            ts,
            op_index_of(oplog_entry),
            entry_uuid(oplog_entry),
            event_identifier(op_type, None, description),
        ))?;
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
            &[("ns", Bson::Document(ns_doc(&affected_ns)))],
            None,
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
        let event = base("dropDatabase", &[("ns", Bson::Document(nsd))], None)?;
        let invalidates = matches!(scope, Scope::Db(_));
        return Ok((Some(event), invalidates));
    }
    if let Ok(from_ns) = cmd.get_str("renameCollection") {
        let to_ns = cmd.get_str("to").unwrap_or("");
        // A rename is seen from either end: a stream on the target is
        // invalidated by a rename onto it as one on the source is.
        if !scope_matches(from_ns, scope) && !scope_matches(to_ns, scope) {
            return Ok((None, false));
        }
        let (to_db, to_coll) = split_ns(to_ns);
        let mut to = Document::new();
        to.insert("db", to_db);
        if !to_coll.is_empty() {
            to.insert("coll", to_coll);
        }
        let mut fields: Vec<(&str, Bson)> = vec![
            ("ns", Bson::Document(ns_doc(from_ns))),
            ("to", Bson::Document(to.clone())),
        ];
        // mongod 6.0+ attaches `operationDescription` to expanded rename
        // events: the `to` namespace plus `dropTarget` (the dropped target
        // collection's UUID) when the rename replaced an existing collection.
        // The token's identifier names it on every stream (measured 8.2.11).
        let mut op_desc = Document::new();
        op_desc.insert("to", Bson::Document(to));
        if let Some(dt) = cmd.get("dropTarget") {
            op_desc.insert("dropTarget", dt.clone());
        }
        if show_expanded_events {
            fields.push(("operationDescription", Bson::Document(op_desc.clone())));
        }
        let event = base("rename", &fields, Some(&op_desc))?;
        let invalidates = matches!(scope, Scope::Coll { .. });
        return Ok((Some(event), invalidates));
    }
    if let Ok(coll) = cmd.get_str("create") {
        if !show_expanded_events {
            return Ok((None, false));
        }
        let affected_ns = format!("{cmd_db}.{coll}");
        if !scope_matches(&affected_ns, scope) {
            return Ok((None, false));
        }
        // `operationDescription` carries the create options other than the name
        // (e.g. `idIndex`), matching mongod's expanded `create` event.
        let mut op_desc = Document::new();
        for (k, v) in &cmd {
            if k != "create" {
                op_desc.insert(k.clone(), v.clone());
            }
        }
        // mongod 8.1+ tags a `create` event with WHAT was created. Probed on
        // 8.2.11: "collection", "timeseries" or "view", as a TOP-LEVEL field
        // (not inside `ns`), emitted last. The unified `change-streams-nsType`
        // spec asserts all three. Mirrors `src/secantus/changestreams.py`.
        let ns_type = if cmd.contains_key("viewOn") {
            "view"
        } else if cmd.contains_key("timeseries") {
            "timeseries"
        } else {
            "collection"
        };
        let event = base(
            "create",
            &[
                ("ns", Bson::Document(ns_doc(&affected_ns))),
                ("operationDescription", Bson::Document(op_desc)),
                ("nsType", Bson::String(ns_type.to_string())),
            ],
            None,
        )?;
        return Ok((Some(event), false));
    }
    // A two-phase index build's commit is a `createIndexes` event for every
    // index it built (mongod 8.2.11); its start and abort are no event.
    if cmd.contains_key("startIndexBuild") || cmd.contains_key("abortIndexBuild") {
        return Ok((None, false));
    }
    // A commit reaches here as the `createIndexes` replay reads, marked
    // (`oplog_entry::INDEX_BUILD_COMMIT`), or as the entry itself.
    let commit = oplog_entry
        .get_bool(crate::oplog_entry::INDEX_BUILD_COMMIT)
        .unwrap_or(false);
    let created = cmd
        .get_str("createIndexes")
        .map(|c| (c, commit))
        .or_else(|_| cmd.get_str("commitIndexBuild").map(|c| (c, true)));
    if let Ok((coll, whole_build)) = created {
        if !show_expanded_events {
            return Ok((None, false));
        }
        let affected_ns = format!("{cmd_db}.{coll}");
        if !scope_matches(&affected_ns, scope) {
            return Ok((None, false));
        }
        let specs: Vec<&Document> = cmd
            .get_array("indexes")
            .map(|a| a.iter().filter_map(Bson::as_document).collect())
            .unwrap_or_default();
        let specs = if whole_build {
            specs
        } else {
            specs.into_iter().take(1).collect()
        };
        // mongod's order: `v`, the options, then `key` and `name` -- except
        // for a build's commit, whose event gives each spec as its oplog
        // entry does (`v`, `unique`, `hidden`, `key`, `name`, the rest;
        // measured 8.2.11).
        let indexes: Vec<Bson> = specs
            .into_iter()
            .map(|s| {
                if whole_build {
                    return Bson::Document(s.clone());
                }
                let mut idx = Document::new();
                idx.insert("v", s.get_i32("v").unwrap_or(2));
                for (k, v) in s {
                    if !matches!(k.as_str(), "v" | "key" | "name" | "ns") {
                        idx.insert(k.clone(), v.clone());
                    }
                }
                idx.insert(
                    "key",
                    Bson::Document(s.get_document("key").cloned().unwrap_or_default()),
                );
                idx.insert("name", s.get_str("name").unwrap_or(""));
                Bson::Document(idx)
            })
            .collect();
        let mut op_desc = Document::new();
        op_desc.insert("indexes", Bson::Array(indexes));
        let event = base(
            "createIndexes",
            &[
                ("ns", Bson::Document(ns_doc(&affected_ns))),
                ("operationDescription", Bson::Document(op_desc)),
            ],
            None,
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
        // mongod (probed 7.0.12) describes the dropped index in full
        // (`{v, key, name}`); the key spec rides in the oplog row. A legacy
        // row without it degrades to the name-only shape.
        let mut idx = Document::new();
        match (cmd.get_document("spec"), cmd.get_document("key")) {
            (Ok(spec), _) => idx = spec.clone(),
            (_, Ok(key)) if !key.is_empty() => {
                idx.insert("v", 2i32);
                idx.insert("key", key.clone());
                idx.insert("name", cmd.get_str("index").unwrap_or(""));
            }
            _ => {
                idx.insert("name", cmd.get_str("index").unwrap_or(""));
            }
        }
        let mut op_desc = Document::new();
        op_desc.insert("indexes", Bson::Array(vec![Bson::Document(idx)]));
        let event = base(
            "dropIndexes",
            &[
                ("ns", Bson::Document(ns_doc(&affected_ns))),
                ("operationDescription", Bson::Document(op_desc)),
            ],
            None,
        )?;
        return Ok((Some(event), false));
    }
    if let Ok(coll) = cmd.get_str("collMod") {
        if !show_expanded_events {
            return Ok((None, false));
        }
        let affected_ns = format!("{cmd_db}.{coll}");
        if !scope_matches(&affected_ns, scope) {
            return Ok((None, false));
        }
        // `operationDescription` is the collMod doc minus the command name.
        let mut op_desc = Document::new();
        for (k, v) in &cmd {
            if k != "collMod" {
                op_desc.insert(k.clone(), v.clone());
            }
        }
        let event = base(
            "modify",
            &[
                ("ns", Bson::Document(ns_doc(&affected_ns))),
                ("operationDescription", Bson::Document(op_desc)),
            ],
            None,
        )?;
        // The collection's options as they were BEFORE this collMod. mongod
        // keeps them in the oplog entry's `o2.collectionOptions_old` and renames
        // the key on the event to `collectionOptions` (probed 8.2.11).
        let mut event = event;
        // An index change adds what its options were, `o2.indexOptions_old`,
        // as `indexOptions`.
        let o2 = oplog_entry.get_document("o2").ok();
        if let Some(old_opts) = o2.and_then(|o2| o2.get("collectionOptions_old")) {
            let mut d = Document::new();
            d.insert("collectionOptions", old_opts.clone());
            if let Some(old_index) = o2.and_then(|o2| o2.get("indexOptions_old")) {
                d.insert("indexOptions", old_index.clone());
            }
            event.insert("stateBeforeChange", Bson::Document(d));
        }
        return Ok((Some(event), false));
    }
    Ok((None, false))
}

/// The final `invalidate` event a cursor emits after a drop / rename /
/// dropDatabase on its watched scope. Its token is the token of the event it
/// follows with `fromInvalidate` set: the same time, collection and event
/// identifier (measured 8.2.11: the `invalidate` after a `drop` has the
/// `drop`'s). `trigger` is that event.
pub fn invalidate_event(trigger: &Document, oplog_entry: &Document) -> Result<Document> {
    let ts = match oplog_entry.get("ts") {
        Some(Bson::Timestamp(t)) => *t,
        _ => Timestamp {
            time: 0,
            increment: 0,
        },
    };
    let mut data = match trigger.get("_id") {
        Some(Bson::Document(t)) => parse_resume_token(t)?,
        _ => return Err(token_err("invalidate: the event has no resume token")),
    };
    data.from_invalidate = true;
    let token = make_resume_token(&data)?;
    let mut event = Document::new();
    event.insert("_id", Bson::Document(token));
    event.insert("operationType", "invalidate");
    event.insert("clusterTime", Bson::Timestamp(ts));
    if let Some(w) = oplog_entry.get("wall") {
        event.insert("wallTime", w.clone());
    }
    Ok(event)
}

/// `kBSONObjMaxChangeEventSize`: an event (with its resume token) larger
/// than this is split.
const SPLIT_THRESHOLD_BYTES: usize = 16 * 1024 * 1024;

/// mongod's `splitChangeEvent` (`change_stream_split_event_helpers.cpp`,
/// 8.2): an event that with its resume token is over 16 MB becomes
/// fragments. Every field but `_id` goes into exactly one, taken in order of
/// (the field's BSON size, its name) and packed into a fragment until the
/// next would take it past 16 MB less the token. Each fragment leads with
/// its own `_id` -- the event's token with the fragment's number from 0 --
/// and `splitEvent: {fragment, of}`, numbered from 1. An event under the
/// limit is returned as it is: mongod 8.2.11 adds no `splitEvent` to an
/// event it did not split (measured 2026-10-11).
pub fn stamp_split_event(event: Document) -> Result<Vec<Document>> {
    let token = match event.get("_id") {
        Some(Bson::Document(t)) => t.clone(),
        _ => return Ok(vec![event]),
    };
    let token_size = encode_doc(&token)?.len();
    if encode_doc(&event)?.len() + token_size <= SPLIT_THRESHOLD_BYTES {
        return Ok(vec![event]);
    }
    let max_fragment = SPLIT_THRESHOLD_BYTES - token_size;
    // (element size, name) -> value, as mongod's `std::map` orders them.
    let mut fields: Vec<(usize, String, Bson)> = Vec::new();
    for (k, v) in event {
        if k == "_id" {
            continue;
        }
        let mut one = Document::new();
        one.insert(k.clone(), v.clone());
        // An element is the document's bytes less its length and terminator.
        let size = encode_doc(&one)?.len() - 5;
        fields.push((size, k, v));
    }
    fields.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let data = parse_resume_token(&token)?;
    let mut fragments: Vec<Document> = Vec::new();
    let mut it = fields.into_iter().peekable();
    while it.peek().is_some() {
        let mut frag_token = data.clone();
        frag_token.fragment = Some(fragments.len() as i32);
        let mut frag = Document::new();
        frag.insert("_id", Bson::Document(make_resume_token(&frag_token)?));
        let mut se = Document::new();
        se.insert("fragment", (fragments.len() as i32) + 1);
        se.insert("of", 0i32);
        frag.insert("splitEvent", Bson::Document(se));
        // mongod measures the fragment with its metadata, the token again.
        let mut size = encode_doc(&frag)?.len() + token_size;
        loop {
            let (field_size, name, value) = it.next().expect("peeked");
            frag.insert(name, value);
            size += field_size;
            match it.peek() {
                Some((next, _, _)) if size + next <= max_fragment => {}
                _ => break,
            }
        }
        if size > max_fragment {
            return Err(StorageError::Bson(format!(
                "Splitting change event failed: fragment size {size} is greater than maximum \
                 allowed fragment size {max_fragment}"
            )));
        }
        fragments.push(frag);
    }
    let total = fragments.len() as i32;
    for frag in &mut fragments {
        if let Some(Bson::Document(se)) = frag.get_mut("splitEvent") {
            se.insert("of", total);
        }
    }
    Ok(fragments)
}
