//! Database and collection names whose bytes are not UTF-8 (RM-222).
//!
//! mongod takes a namespace as bytes: a collection or database name holding
//! `0xff` is created, listed, renamed, dropped and named in errors and in
//! the oplog like any other (measured 8.2.11, 2026-10-27). Such a name
//! arrives as a string whose bytes are not UTF-8, which the codec decodes
//! into its carrier (`bsondoc::invalid_utf8`). Inside the server a name is a
//! Rust `String`, so a name that is not UTF-8 is held ESCAPED, as a field
//! name that is not UTF-8 already is ([`bsondoc::escape_key`]: each such byte
//! as a NUL and two lowercase hex digits), and storage writes the escaped
//! name to the engine as its bytes (`secantus_wt::name_bytes`).
//!
//! A namespace can never hold a NUL (mongod refuses one: 73 "namespaces
//! cannot have embedded null characters", and so does `dispatch`), so the
//! escape cannot collide with a real name -- provided a client's NUL is
//! refused before a name is read. [`from_bytes`] therefore refuses a NUL in
//! every name field it reads and turns carriers into escaped names; the first
//! field and `$db` keep their own refusal in `dispatch_inner`.
//!
//! On the way out, [`to_bytes`] turns the reply fields that hold a name back
//! into the strings of the bytes, and writes the bytes of an escaped name
//! (or string) an error message quotes.

use std::borrow::Cow;

use bson::{Bson, Document};

use crate::bsondoc;
use crate::CommandError;

/// A name as a reply value: the string of its bytes.
pub fn name_value(name: &str) -> Bson {
    bsondoc::key_string(name.to_string())
}

fn nul_refused() -> CommandError {
    CommandError::new(
        73,
        "InvalidNamespace",
        "namespaces cannot have embedded null characters".to_string(),
    )
}

/// Turn the name in `slot` from a carrier into an escaped string. A client
/// string with a NUL in it is refused (the escape would otherwise forge a
/// name).
fn fix(slot: &mut Bson, changed: &mut bool, check_nul: bool) -> Result<(), CommandError> {
    match slot {
        Bson::String(s) if check_nul && s.contains('\0') => Err(nul_refused()),
        Bson::String(_) => Ok(()),
        other => {
            if let Some(b) = bsondoc::as_invalid_utf8(other) {
                *other = Bson::String(bsondoc::escape_key(b));
                *changed = true;
            }
            Ok(())
        }
    }
}

/// `{db, coll}` or a plain name, as a stage names a collection.
fn fix_target(v: &mut Bson, changed: &mut bool) -> Result<(), CommandError> {
    match v {
        Bson::Document(d) if bsondoc::carrier_bytes(d).is_none() => {
            for k in ["db", "coll", "into"] {
                if let Some(x) = d.get_mut(k) {
                    fix_target(x, changed)?;
                }
            }
            Ok(())
        }
        other => fix(other, changed, true),
    }
}

fn fix_pipeline(p: &mut Bson, changed: &mut bool) -> Result<(), CommandError> {
    let Bson::Array(stages) = p else {
        return Ok(());
    };
    for stage in stages {
        let Bson::Document(stage) = stage else {
            continue;
        };
        for (op, spec) in stage.iter_mut() {
            match op.as_str() {
                "$out" | "$merge" => fix_target(spec, changed)?,
                "$unionWith" => match spec {
                    Bson::Document(d) => {
                        if let Some(c) = d.get_mut("coll") {
                            fix(c, changed, true)?;
                        }
                        if let Some(sub) = d.get_mut("pipeline") {
                            fix_pipeline(sub, changed)?;
                        }
                    }
                    other => fix(other, changed, true)?,
                },
                "$lookup" | "$graphLookup" => {
                    if let Bson::Document(d) = spec {
                        if let Some(from) = d.get_mut("from") {
                            fix_target(from, changed)?;
                        }
                        if let Some(sub) = d.get_mut("pipeline") {
                            fix_pipeline(sub, changed)?;
                        }
                    }
                }
                "$facet" => {
                    if let Bson::Document(d) = spec {
                        for (_, sub) in d.iter_mut() {
                            fix_pipeline(sub, changed)?;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn fix_command(doc: &mut Document, changed: &mut bool) -> Result<(), CommandError> {
    let first = doc.keys().next().cloned();
    if let Some(first) = &first {
        if let Some(v) = doc.get_mut(first) {
            // `dispatch_inner` refuses a NUL in the first field itself, in
            // the words each command uses.
            fix(v, changed, false)?;
        }
    }
    if let Some(v) = doc.get_mut("$db") {
        fix(v, changed, false)?;
    }
    for k in ["to", "viewOn", "toCollection", "collection"] {
        if let Some(v) = doc.get_mut(k) {
            fix(v, changed, true)?;
        }
    }
    if let Some(p) = doc.get_mut("pipeline") {
        fix_pipeline(p, changed)?;
    }
    if first.as_deref() == Some("explain") {
        if let Some(Bson::Document(inner)) = doc.get_mut("explain") {
            fix_command(inner, changed)?;
        }
    }
    Ok(())
}

/// The command with every name that is not UTF-8 escaped, or `None` when it
/// names nothing of the kind.
pub fn from_bytes(doc: &Document) -> Result<Option<Document>, CommandError> {
    from_bytes_named(doc).map(|r| r.map(|(d, _)| d))
}

/// [`from_bytes`], with the escaped names it made: the reply's messages
/// write exactly those back as their bytes.
pub fn from_bytes_named(doc: &Document) -> Result<Option<(Document, Vec<String>)>, CommandError> {
    // A client's NUL in the database or the command's collection is refused
    // here, before any escape exists to be mistaken for it, in mongod's two
    // wordings (`nul_in_namespace`).
    if let Ok(db) = doc.get_str("$db") {
        if let Some(e) = crate::nul_in_namespace("database name", db) {
            return Err(e);
        }
    }
    if let Some(Bson::String(first)) = doc.values().next() {
        if let Some(e) = crate::nul_in_namespace("collection name", first) {
            return Err(e);
        }
    }
    // Nothing to escape and no name field but the first (which
    // `dispatch_inner` checks): the common case, with no copy.
    let other_names = [
        "to",
        "viewOn",
        "toCollection",
        "collection",
        "pipeline",
        "explain",
    ]
    .iter()
    .any(|k| doc.contains_key(*k));
    if !other_names && !has_carrier(doc) {
        return Ok(None);
    }
    let mut out = doc.clone();
    let mut changed = false;
    fix_command(&mut out, &mut changed)?;
    if !changed {
        return Ok(None);
    }
    let mut names = Vec::new();
    collect_escaped(&out, &mut names);
    Ok(Some((out, names)))
}

/// The escaped names in a command `fix_command` rewrote.
fn collect_escaped(doc: &Document, out: &mut Vec<String>) {
    fn walk(v: &Bson, out: &mut Vec<String>) {
        match v {
            Bson::String(s) if s.contains('\0') && bsondoc::key_bytes(s).is_some() => {
                out.push(s.clone())
            }
            Bson::Document(d) => d.values().for_each(|x| walk(x, out)),
            Bson::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    doc.values().for_each(|v| walk(v, out));
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out.dedup();
}

/// Whether `doc` could hold a name that is not UTF-8 anywhere `from_bytes`
/// looks: any carrier in it. Cheap enough to run on every command.
pub fn has_carrier(doc: &Document) -> bool {
    fn any(v: &Bson) -> bool {
        match v {
            Bson::Document(d) => bsondoc::carrier_bytes(d).is_some() || d.values().any(any),
            Bson::Array(a) => a.iter().any(any),
            _ => false,
        }
    }
    doc.values().any(any)
}

/// An escaped name's string value: the carrier of its bytes.
fn name_back(v: &mut Bson) {
    if let Bson::String(s) = v {
        if s.contains('\0') {
            if let Some(Cow::Owned(b)) = bsondoc::key_bytes(s) {
                *v = bsondoc::invalid_utf8(b);
            }
        }
    }
}

/// The bytes of a message: each two NULs and two lowercase hex digits that
/// stand for a byte outside UTF-8 (`bsondoc::message_escape`, as a renderer
/// quotes a string that is not UTF-8) written as that byte, as mongod writes
/// the bytes it quotes. Anything else is left as it is: a client's own NUL
/// in a quoted string is one NUL, never two.
pub fn message_bytes(text: &str) -> Option<Vec<u8>> {
    if !text.contains("\0\0") {
        return None;
    }
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut any = false;
    while i < b.len() {
        if b[i] == 0 && b.get(i + 1) == Some(&0) {
            if let Some(byte) = b
                .get(i + 2..i + 4)
                .filter(|h| {
                    h.iter()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
                })
                .and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())
                .filter(|byte| *byte >= 0x80)
            {
                out.push(byte);
                i += 4;
                any = true;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    any.then_some(out)
}

fn message_back(v: &mut Bson, names: &[String]) {
    if let Bson::String(s) = v {
        // The names the request carried as bytes, wherever the message
        // quotes them, then every quoted string that is not UTF-8.
        let mut text = Cow::Borrowed(s.as_str());
        for name in names {
            if text.contains(name.as_str()) {
                let bytes = bsondoc::key_bytes(name)
                    .map(|b| b.into_owned())
                    .unwrap_or_default();
                text = Cow::Owned(text.replace(name.as_str(), &bsondoc::message_escape(&bytes)));
            }
        }
        if let Some(b) = message_bytes(&text) {
            *v = bsondoc::string_from_bytes(b);
        }
    }
}

fn errmsgs_back(d: &mut Document, names: &[String]) {
    if let Some(m) = d.get_mut("errmsg") {
        message_back(m, names);
    }
    if let Some(Bson::Array(errors)) = d.get_mut("writeErrors") {
        for e in errors {
            if let Bson::Document(e) = e {
                if let Some(m) = e.get_mut("errmsg") {
                    message_back(m, names);
                }
            }
        }
    }
    if let Some(Bson::Document(w)) = d.get_mut("writeConcernError") {
        if let Some(m) = w.get_mut("errmsg") {
            message_back(m, names);
        }
    }
}

/// The reply's name fields as the strings of their bytes, and its error
/// messages with the bytes they quote. `command` is the command that ran;
/// `names` the escaped names its request carried ([`from_bytes_named`]).
pub fn to_bytes(reply: &mut Document, command: &str, names: &[String]) {
    errmsgs_back(reply, names);
    for k in ["ns", "db"] {
        if let Some(v) = reply.get_mut(k) {
            name_back(v);
        }
    }
    if let Some(Bson::Document(q)) = reply.get_mut("queryPlanner") {
        if let Some(v) = q.get_mut("namespace") {
            name_back(v);
        }
    }
    if let Some(Bson::Document(cursor)) = reply.get_mut("cursor") {
        if let Some(v) = cursor.get_mut("ns") {
            name_back(v);
        }
        if matches!(command, "listCollections" | "listIndexes") {
            for batch in ["firstBatch", "nextBatch"] {
                if let Some(Bson::Array(items)) = cursor.get_mut(batch) {
                    for item in items {
                        if let Bson::Document(item) = item {
                            if command == "listCollections" {
                                if let Some(v) = item.get_mut("name") {
                                    name_back(v);
                                }
                                if let Some(Bson::Document(o)) = item.get_mut("options") {
                                    if let Some(v) = o.get_mut("viewOn") {
                                        name_back(v);
                                    }
                                }
                            } else if let Some(v) = item.get_mut("ns") {
                                name_back(v);
                            }
                        }
                    }
                }
            }
        }
    }
    if command == "listDatabases" {
        if let Some(Bson::Array(dbs)) = reply.get_mut("databases") {
            for d in dbs {
                if let Bson::Document(d) = d {
                    if let Some(v) = d.get_mut("name") {
                        name_back(v);
                    }
                }
            }
        }
    }
    if command == "currentOp" {
        if let Some(Bson::Array(ops)) = reply.get_mut("inprog") {
            for op in ops {
                if let Bson::Document(op) = op {
                    if let Some(v) = op.get_mut("ns") {
                        name_back(v);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn names_go_in_escaped_and_come_out_as_their_bytes() {
        let carrier = bsondoc::invalid_utf8(b"c\xff".to_vec());
        let cmd = doc! { "find": carrier.clone(), "$db": bsondoc::invalid_utf8(b"d\xfe".to_vec()) };
        let fixed = from_bytes(&cmd).unwrap().unwrap();
        assert_eq!(fixed.get_str("find").unwrap(), "c\u{0}ff");
        assert_eq!(fixed.get_str("$db").unwrap(), "d\u{0}fe");
        assert_eq!(from_bytes(&doc! { "find": "c" }).unwrap(), None);
        // A NUL in a name field other than the first is refused.
        assert!(from_bytes(&doc! { "renameCollection": "a.b", "to": "a.c\u{0}ff" }).is_err());

        let mut reply = doc! {
            "cursor": { "ns": "d\u{0}fe.c\u{0}ff", "id": 0_i64 },
            "errmsg": "E11000 collection: d\u{0}fe.c\u{0}ff and a NUL\u{0}zz, a\u{0}ff \u{0}\u{0}fe",
        };
        to_bytes(&mut reply, "find", &["d\u{0}fe".into(), "c\u{0}ff".into()]);
        assert_eq!(
            bsondoc::str_bytes(reply.get_document("cursor").unwrap().get("ns").unwrap()),
            Some(&b"d\xfe.c\xff"[..])
        );
        assert_eq!(
            bsondoc::str_bytes(reply.get("errmsg").unwrap()),
            // A name the request carried, and a quoted string that is not
            // UTF-8, come out as bytes; a client's own NUL stays a NUL.
            Some(&b"E11000 collection: d\xfe.c\xff and a NUL\x00zz, a\x00ff \xfe"[..])
        );
    }
}
