//! mongod's text for a request it quotes back in an error: the request as
//! its IDL parser serialises it again (`op.toBSON()`), printed by
//! `BSONObj::toString`.
//!
//! Two families of error quote one: a write whose statement ids do not fit
//! its batch ("... Write command: { insert: "c", bypassDocumentValidation:
//! false, ordered: true, ... }"), and a `findAndModify` retry that does not
//! fit the write it retries ("findAndModify retry request: { ... }"). The
//! serialisation is not the request as sent: fields come in the IDL's order,
//! defaults are filled in (`bypassDocumentValidation`, `ordered`, `multi`,
//! `upsert`, a write concern's `w` and `wtimeout`), a string `hint` becomes
//! `{ $hint: <name> }`, and `$db` is dropped. Every order and default below
//! was measured against mongod 8.2.11 (2026-11-09).

use bson::{Bson, Document};

/// A value as `BSONElement::toString` prints it without its name (the
/// non-`full` form an error message uses): a string of 160 bytes or more,
/// counting its terminating nul, keeps its first 150 bytes and ends `..."`,
/// and a binary of subtype 4 and 16 bytes prints as `UUID("...")`. Every
/// other value as [`crate::explain::debug_string::value`] prints it.
pub(crate) fn value(v: &Bson) -> String {
    match v {
        Bson::String(s) if s.len() + 1 > 160 => {
            let head = String::from_utf8_lossy(&s.as_bytes()[..150]);
            format!("\"{head}...\"")
        }
        Bson::Binary(b) if u8::from(b.subtype) == 4 && b.bytes.len() == 16 => {
            let hex: String = b.bytes.iter().map(|x| format!("{x:02x}")).collect();
            format!(
                "UUID(\"{}-{}-{}-{}-{}\")",
                &hex[0..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..32]
            )
        }
        Bson::Document(d) => object(d),
        Bson::Array(a) if a.is_empty() => "[]".to_string(),
        Bson::Array(a) => format!("[ {} ]", a.iter().map(value).collect::<Vec<_>>().join(", ")),
        other => crate::explain::debug_string::value(other),
    }
}

/// A document as `BSONObj::toString` prints it.
pub(crate) fn object(d: &Document) -> String {
    if d.is_empty() {
        return "{}".to_string();
    }
    let fields: Vec<String> = d
        .iter()
        .map(|(k, v)| format!("{k}: {}", value(v)))
        .collect();
    format!("{{ {} }}", fields.join(", "))
}

/// The generic arguments, in the order mongod 8.2 serialises them after a
/// command's own fields. Measured: `apiVersion` .. `startTransaction`,
/// `comment`, `$readPreference`, `$clusterTime`, `$client`,
/// `maxTimeMSOpOnly`, `mayBypassWriteBlocking`; the others sit where the
/// IDL lists them. A generic field not named here follows them, in the
/// order the request has it.
const GENERIC_ORDER: &[&str] = &[
    "apiVersion",
    "apiStrict",
    "apiDeprecationErrors",
    "maxTimeMS",
    "readConcern",
    "writeConcern",
    "lsid",
    "clientOperationKey",
    "txnNumber",
    "autocommit",
    "startTransaction",
    "comment",
    "$readPreference",
    "$clusterTime",
    "$audit",
    "$client",
    "$configServerState",
    "allowImplicitCollectionCreation",
    "$oplogQueryData",
    "$queryOptions",
    "$replData",
    "databaseVersion",
    "shardVersion",
    "txnRetryCounter",
    "maxTimeMSOpOnly",
    "$configTime",
    "$topologyTime",
    "mayBypassWriteBlocking",
];

/// A number field the IDL holds as an integer, as it prints back.
fn integer(v: &Bson) -> Bson {
    match v {
        Bson::Double(d) => Bson::Int64(*d as i64),
        Bson::Decimal128(_) => Bson::Int64(crate::util::as_i64(v).unwrap_or(0)),
        other => other.clone(),
    }
}

/// A `safeBool` field: a number reads as its truth.
fn safe_bool(v: &Bson) -> Bson {
    Bson::Boolean(crate::util::truthy(v))
}

/// A write concern as `WriteConcernOptions` serialises it: `w` (1 when
/// absent), `j` when given, `wtimeout` (0 when absent), `fsync` when true.
fn write_concern(wc: &Document) -> Bson {
    let mut out = Document::new();
    let w = match wc.get("w") {
        None | Some(Bson::Null) => Bson::Int32(1),
        Some(w @ (Bson::String(_) | Bson::Document(_))) => w.clone(),
        Some(w) => integer(w),
    };
    out.insert("w", w);
    if let Some(j) = wc.get("j").filter(|v| !matches!(v, Bson::Null)) {
        out.insert("j", safe_bool(j));
    }
    out.insert(
        "wtimeout",
        wc.get("wtimeout")
            .filter(|v| !matches!(v, Bson::Null))
            .map_or(Bson::Int32(0), integer),
    );
    if wc.get("fsync").is_some_and(crate::util::truthy) {
        out.insert("fsync", true);
    }
    Bson::Document(out)
}

/// A read concern, its known fields in the IDL's order (`level` before
/// `afterClusterTime`, measured), anything else after them as given.
fn read_concern(rc: &Document) -> Bson {
    const ORDER: &[&str] = &["level", "afterOpTime", "afterClusterTime", "atClusterTime"];
    let mut out = Document::new();
    for key in ORDER {
        if let Some(v) = rc.get(*key) {
            out.insert(*key, v.clone());
        }
    }
    for (k, v) in rc {
        if !ORDER.contains(&k.as_str()) {
            out.insert(k.clone(), v.clone());
        }
    }
    Bson::Document(out)
}

/// Append the request's generic arguments to `out`, in mongod's order.
fn generic_arguments(doc: &Document, own: &[&str], out: &mut Document) {
    let mut put = |key: &str, v: &Bson| {
        let v = match (key, v) {
            ("writeConcern", Bson::Document(wc)) => write_concern(wc),
            ("readConcern", Bson::Document(rc)) => read_concern(rc),
            ("maxTimeMS" | "maxTimeMSOpOnly" | "txnNumber", v) => integer(v),
            _ => v.clone(),
        };
        out.insert(key, v);
    };
    for key in GENERIC_ORDER {
        if let Some(v) = doc.get(*key) {
            put(key, v);
        }
    }
    let command = doc.keys().next().map(String::as_str);
    for (k, v) in doc {
        if Some(k.as_str()) == command
            || k == "$db"
            || own.contains(&k.as_str())
            || GENERIC_ORDER.contains(&k.as_str())
        {
            continue;
        }
        put(k, v);
    }
}

/// A `hint` as the IDL holds it: a name is `{ $hint: <name> }`, an empty
/// one is no hint.
fn hint(v: &Bson) -> Option<Bson> {
    match v {
        Bson::String(name) => Some(Bson::Document(bson::doc! { "$hint": name.clone() })),
        Bson::Document(d) if d.is_empty() => None,
        Bson::Null => None,
        other => Some(other.clone()),
    }
}

/// Copy the fields of `from` named in `order`, as given, skipping nulls.
fn copy_present(from: &Document, order: &[&str], out: &mut Document) {
    for key in order {
        match from.get(*key) {
            None | Some(Bson::Null) => {}
            Some(v) => {
                out.insert(*key, v.clone());
            }
        }
    }
}

/// An `update` statement as `UpdateOpEntry` serialises it.
fn update_statement(s: &Document) -> Bson {
    let mut out = Document::new();
    copy_present(s, &["q", "u", "c", "sort", "arrayFilters"], &mut out);
    if let Some(h) = s.get("hint").and_then(hint) {
        out.insert("hint", h);
    }
    for key in ["multi", "upsert"] {
        out.insert(
            key,
            s.get(key)
                .filter(|v| !matches!(v, Bson::Null))
                .map_or(Bson::Boolean(false), safe_bool),
        );
    }
    copy_present(s, &["upsertSupplied", "collation", "sampleId"], &mut out);
    Bson::Document(out)
}

/// A `delete` statement as `DeleteOpEntry` serialises it.
fn delete_statement(s: &Document) -> Bson {
    let mut out = Document::new();
    copy_present(s, &["q", "limit"], &mut out);
    if let Some(h) = s.get("hint").and_then(hint) {
        out.insert("hint", h);
    }
    copy_present(s, &["collation", "sampleId"], &mut out);
    Bson::Document(out)
}

/// The statement ids of a write as its IDL holds them: `stmtIds` without
/// its nulls.
pub(crate) fn statement_id_list(doc: &Document) -> Option<Vec<Bson>> {
    match doc.get("stmtIds") {
        Some(Bson::Array(ids)) => Some(
            ids.iter()
                .filter(|v| !matches!(v, Bson::Null))
                .cloned()
                .collect(),
        ),
        _ => None,
    }
}

/// An `insert`, `update` or `delete` command as mongod serialises it.
/// `statements` are its documents or statements, nulls already dropped (a
/// driver's document sequence included).
pub(crate) fn write_command(command: &str, doc: &Document, statements: &[Bson]) -> String {
    let (array, after): (&str, &[&str]) = match command {
        "insert" => ("documents", &[]),
        "update" => ("updates", &["let", "runtimeConstants"]),
        _ => ("deletes", &["let", "runtimeConstants"]),
    };
    let mut out = Document::new();
    out.insert(command, doc.get(command).cloned().unwrap_or(Bson::Null));
    out.insert(
        "bypassDocumentValidation",
        doc.get("bypassDocumentValidation")
            .filter(|v| !matches!(v, Bson::Null))
            .map_or(Bson::Boolean(false), safe_bool),
    );
    out.insert(
        "ordered",
        doc.get("ordered")
            .filter(|v| !matches!(v, Bson::Null))
            .cloned()
            .unwrap_or(Bson::Boolean(true)),
    );
    copy_present(doc, &["stmtId"], &mut out);
    if let Some(ids) = statement_id_list(doc) {
        out.insert("stmtIds", ids);
    }
    copy_present(
        doc,
        &[
            "isTimeseriesNamespace",
            "collectionUUID",
            "encryptionInformation",
            "bypassEmptyTsReplacement",
            "rawData",
        ],
        &mut out,
    );
    let statements: Vec<Bson> = statements
        .iter()
        .map(|s| match (command, s) {
            ("update", Bson::Document(d)) => update_statement(d),
            ("delete", Bson::Document(d)) => delete_statement(d),
            _ => s.clone(),
        })
        .collect();
    out.insert(array, statements);
    for key in after {
        let source = match *key {
            "runtimeConstants" if !doc.contains_key("runtimeConstants") => "legacyRuntimeConstants",
            key => key,
        };
        if let Some(v) = doc.get(source).filter(|v| !matches!(v, Bson::Null)) {
            out.insert(*key, v.clone());
        }
    }
    const OWN: &[&str] = &[
        "bypassDocumentValidation",
        "ordered",
        "stmtId",
        "stmtIds",
        "isTimeseriesNamespace",
        "collectionUUID",
        "encryptionInformation",
        "bypassEmptyTsReplacement",
        "rawData",
        "documents",
        "updates",
        "deletes",
        "let",
        "runtimeConstants",
        "legacyRuntimeConstants",
        "originalQuery",
        "originalCollation",
        "sampleId",
    ];
    generic_arguments(doc, OWN, &mut out);
    object(&out)
}

/// A `findAndModify` command as mongod serialises it: only the fields the
/// request has, in the IDL's order, under the camel-case name whichever
/// spelling arrived.
pub(crate) fn find_and_modify(doc: &Document) -> String {
    const ORDER: &[&str] = &[
        "query",
        "fields",
        "sort",
        "hint",
        "collation",
        "arrayFilters",
        "remove",
        "update",
        "upsert",
        "new",
        "stmtId",
        "bypassDocumentValidation",
        "let",
        "isTimeseriesNamespace",
        "collectionUUID",
        "encryptionInformation",
        "sampleId",
        "bypassEmptyTsReplacement",
        "rawData",
    ];
    let mut out = Document::new();
    let coll = doc
        .get("findAndModify")
        .or_else(|| doc.get("findandmodify"))
        .cloned()
        .unwrap_or(Bson::Null);
    out.insert("findAndModify", coll);
    for key in ORDER {
        let Some(v) = doc.get(*key).filter(|v| !matches!(v, Bson::Null)) else {
            continue;
        };
        let v = match *key {
            "hint" => match hint(v) {
                Some(h) => h,
                None => continue,
            },
            "remove" | "upsert" | "new" | "bypassDocumentValidation" => safe_bool(v),
            _ => v.clone(),
        };
        out.insert(*key, v);
    }
    let mut own: Vec<&str> = ORDER.to_vec();
    own.push("findAndModify");
    own.push("findandmodify");
    generic_arguments(doc, &own, &mut out);
    object(&out)
}

/// A statement's oplog entry as `OplogEntry::toBSONForLogging` prints it,
/// with the time it names: `oplogTs: Timestamp(..), oplog: { oplogEntry:
/// { ... } }`. A statement of an insert batch is one operation of an
/// `applyOps` entry: mongod prints that operation, then the entry's own
/// session, transaction number and times (everything of it but `op`, `ns`,
/// `o` and `multiOpType`).
pub(crate) fn oplog_entry(entry: &Document, stmt_id: i32) -> Option<String> {
    let ts = entry.get("ts")?.clone();
    let shown = match entry
        .get_document("o")
        .ok()
        .and_then(|o| o.get_array("applyOps").ok())
    {
        Some(ops) if entry.get_str("op") == Ok("c") => {
            let op = ops.iter().filter_map(Bson::as_document).find(|op| {
                op.get("stmtId").and_then(crate::util::as_i64) == Some(i64::from(stmt_id))
            })?;
            let mut out = op.clone();
            for (k, v) in entry {
                if matches!(k.as_str(), "op" | "ns" | "o" | "multiOpType") || out.contains_key(k) {
                    continue;
                }
                out.insert(k.clone(), v.clone());
            }
            out
        }
        _ => entry.clone(),
    };
    Some(format!(
        "oplogTs: {}, oplog: {{ oplogEntry: {} }}",
        value(&ts),
        object(&shown)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn a_long_string_keeps_150_bytes() {
        let s159 = "a".repeat(159);
        assert_eq!(value(&Bson::String(s159.clone())), format!("\"{s159}\""));
        let s160 = "b".repeat(160);
        assert_eq!(
            value(&Bson::String(s160)),
            format!("\"{}...\"", "b".repeat(150))
        );
    }

    #[test]
    fn a_uuid_prints_as_one() {
        let b = Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Uuid,
            bytes: vec![1; 16],
        });
        assert_eq!(value(&b), "UUID(\"01010101-0101-0101-0101-010101010101\")");
        let b3 = Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::UuidOld,
            bytes: vec![1; 16],
        });
        assert_eq!(value(&b3), "BinData(3, 01010101010101010101010101010101)");
    }

    #[test]
    fn an_update_fills_its_defaults_in_mongods_order() {
        let cmd = doc! {
            "update": "c",
            "let": {"z": 1},
            "updates": [],
            "stmtIds": [1, 2],
            "maxTimeMS": 5,
            "comment": {"x": 1},
        };
        let statements = vec![Bson::Document(doc! {
            "u": {"$set": {"a": 1}}, "hint": "_id_", "upsert": true, "q": {"_id": 1},
        })];
        assert_eq!(
            write_command("update", &cmd, &statements),
            "{ update: \"c\", bypassDocumentValidation: false, ordered: true, stmtIds: [ 1, 2 ], \
             updates: [ { q: { _id: 1 }, u: { $set: { a: 1 } }, hint: { $hint: \"_id_\" }, \
             multi: false, upsert: true } ], let: { z: 1 }, maxTimeMS: 5, comment: { x: 1 } }"
        );
    }

    #[test]
    fn a_write_concern_takes_its_defaults() {
        assert_eq!(
            value(&write_concern(&doc! {"fsync": true})),
            "{ w: 1, wtimeout: 0, fsync: true }"
        );
        assert_eq!(
            value(&write_concern(
                &doc! {"wtimeout": 100, "j": false, "w": 2.0}
            )),
            "{ w: 2, j: false, wtimeout: 100 }"
        );
    }

    #[test]
    fn a_batched_insert_shows_its_own_operation() {
        let entry = doc! {
            "lsid": {"id": 1},
            "txnNumber": 1_i64,
            "op": "c",
            "ns": "admin.$cmd",
            "o": {"applyOps": [
                {"op": "i", "ns": "d.g", "o": {"_id": 1}, "stmtId": 0},
                {"op": "i", "ns": "d.g", "o": {"_id": 2}, "stmtId": 1},
            ]},
            "ts": bson::Timestamp { time: 5, increment: 6 },
            "t": 1_i64,
            "multiOpType": 1,
        };
        assert_eq!(
            oplog_entry(&entry, 1).unwrap(),
            "oplogTs: Timestamp(5, 6), oplog: { oplogEntry: { op: \"i\", ns: \"d.g\", o: { _id: 2 }, \
             stmtId: 1, lsid: { id: 1 }, txnNumber: 1, ts: Timestamp(5, 6), t: 1 } }"
        );
    }
}
