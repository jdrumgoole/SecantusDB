//! `updateDescription` diff — Rust port of
//! `secantus.diff.compute_update_description`, the sixth leaf engine (used to
//! build the change-stream `$v: 2` update events). Returns
//! `{updatedFields, removedFields, truncatedArrays}` for a pre->post image.
//!
//! Value equality reuses the expression engine's Python-`==` semantics
//! (`expressions::py_eq`); a Decimal128 / exotic value anywhere defers the whole
//! diff to pure Python.

use std::collections::{BTreeMap, BTreeSet};

use bson::{Bson, Document};

use crate::expressions;

pub use crate::fallback::Fallback;

type R<T> = Result<T, Fallback>;

struct Acc {
    updated: Document,
    removed: Vec<Bson>,
    truncated: Vec<Bson>,
    /// Array paths the UPDATE touched element-wise, mapped to the indices past
    /// the old end that may be reported (`None` = any, an append knows every
    /// index it wrote). `None` for the whole field means "no operator update":
    /// pipeline updates and callers with no spec diff the values instead.
    /// See this module's docs.
    elementwise: Option<BTreeMap<String, Option<BTreeSet<i64>>>>,
    disambiguated: Document,
    /// `$push` targets: an append onto an EMPTY one is reported as the whole
    /// array (see `walk`).
    push_targets: BTreeSet<String>,
}

/// mongod 6.1+ `disambiguatedPaths`: any reported path containing a
/// numeric-string FIELD name (a dict key like "1" that a reader could
/// mistake for an array index) maps to its typed segment list — Int32
/// for real array indices, String for field names. Mirrors
/// `diff._record_ambiguous` in the Python engine.
fn record_ambiguous(path: &str, segments: &[Bson], acc: &mut Acc) {
    let ambiguous = segments.iter().any(|s| match s {
        Bson::String(k) => !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    });
    if ambiguous {
        acc.disambiguated
            .insert(path.to_string(), Bson::Array(segments.to_vec()));
    }
}

fn eq(a: &Bson, b: &Bson) -> R<bool> {
    expressions::py_eq(a, b)
}

/// Would storing `b` where `a` is leave the document unchanged?
///
/// The CHANGE-DETECTION twin of `eq`, and deliberately not the same predicate.
/// mongod answers the two questions differently:
///
/// * EQUALITY calls them the same -- `{$eq: [0.0, -0.0]}` is true, `$cmp` is 0,
///   and `find({a: -0.0})` matches a stored `0.0`;
/// * CHANGE detection does not -- `{$set: {a: -0.0}}` over `a: 0.0` writes and
///   puts the field in a change stream's `updatedFields`, and so does a numeric
///   TYPE change: `int 1` -> `double 1.0` reports `{a: 1.0}` with
///   `nModified: 1`.
///
/// Both probed against 8.2.11 (2026-09-05). Folding this into `eq` would have
/// been the tempting one-line fix and would have broken `$eq` and query
/// matching -- one predicate cannot serve both questions.
fn same_stored_value(a: &Bson, b: &Bson) -> R<bool> {
    if !eq(a, b)? {
        return Ok(false);
    }
    Ok(same_encoding(a, b))
}

/// Do two `eq`-equal values encode to the same BSON? Distinguishes a signed
/// zero (by bit pattern) and a numeric type change (by variant), recursing so
/// that either nested in an array or a subdocument counts just the same.
pub(crate) fn same_encoding(a: &Bson, b: &Bson) -> bool {
    match (a, b) {
        (Bson::Double(x), Bson::Double(y)) => x.to_bits() == y.to_bits(),
        (Bson::Array(x), Bson::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| same_encoding(p, q))
        }
        (Bson::Document(x), Bson::Document(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && same_encoding(v1, v2))
        }
        // Values, not discriminants. This used to be
        // `discriminant(a) == discriminant(b)`, which is only sound behind
        // `eq` (it would call `"x"` and `"y"` the same). `doc_changed` calls
        // this standalone, so it compares properly; `Bson`'s `==` is exact for
        // every variant the arms above do not already special-case.
        _ => a == b,
    }
}

/// Did an update actually change the STORED BYTES of the document?
///
/// The encoding is the whole rule, and each of the two cheaper tests it
/// replaces got a different case wrong:
///
/// * `new != old` alone cannot see a signed zero. `Bson::Double`'s `f64 ==`
///   calls `-0.0` equal to `0.0`, so `{$set: {a: -0.0}}` over `a: 0.0` skipped
///   the write and silently kept the old zero (fixed 2026-09-05).
/// * `new != old` alone also fires on a document that merely CONTAINS a NaN,
///   because `NaN != NaN`. Nothing was written, yet the document was rewritten
///   and an oplog entry emitted -- a phantom write and a phantom change-stream
///   event. `{$unset: {absent: ""}}` on any document holding a NaN reported
///   `nModified: 1` (measured 8.2.11, 2026-09-06).
///
/// The encoding gets both right: a signed zero encodes differently, and two
/// NaNs with the same bits encode the same.
///
/// The encoding is not all of mongod's `nModified` rule, though. mongod also
/// counts an ARITHMETIC write whose result is a NaN -- `{$inc: {a: 1}}` over
/// `a: NaN` reports 1 with byte-identical output, while `{$min: {a: 5}}` over
/// the same document reports 0 because `$min` declined to write. That half is
/// invisible in the bytes, so it is a separate, narrow check:
/// `update::arith_wrote_nan`, which the callers apply alongside this one.
pub fn doc_changed(new: &Document, old: &Document) -> bool {
    !(new.len() == old.len()
        && new
            .iter()
            .zip(old.iter())
            .all(|((k1, v1), (k2, v2))| k1 == k2 && same_encoding(v1, v2)))
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Diff two documents field-by-field. Split out from [`walk`] so the top-level
/// call (and the nested doc-vs-doc case) operate on `&Document` directly, without
/// wrapping each side in an owned `Bson::Document` clone.
fn walk_docs(a: &Document, b: &Document, path: &str, segments: &[Bson], acc: &mut Acc) -> R<()> {
    // sorted union of keys (Python `sorted(pre_keys | post_keys)`)
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    for key in keys {
        let cp = child_path(path, key);
        let mut cs = segments.to_vec();
        cs.push(Bson::String(key.clone()));
        match (a.get(key), b.get(key)) {
            (Some(_), None) => {
                acc.removed.push(Bson::String(cp.clone()));
                record_ambiguous(&cp, &cs, acc);
            }
            (None, Some(bv)) => {
                acc.updated.insert(cp.clone(), bv.clone());
                record_ambiguous(&cp, &cs, acc);
            }
            (Some(av), Some(bv)) => walk(av, bv, &cp, &cs, acc)?,
            (None, None) => unreachable!(),
        }
    }
    Ok(())
}

fn walk(pre: &Bson, post: &Bson, path: &str, segments: &[Bson], acc: &mut Acc) -> R<()> {
    match (pre, post) {
        (Bson::Document(a), Bson::Document(b)) => walk_docs(a, b, path, segments, acc),
        (Bson::Array(a), Bson::Array(b)) => {
            if same_stored_value(pre, post)? {
                return Ok(());
            }
            // mongod reports an array by the OPERATION that changed it, not by
            // diffing the values. Mirrors `_walk` in src/secantus/diff.py.
            let Some(ew) = acc.elementwise.as_ref() else {
                // Pipeline update (or no spec): diff the values. This is the one
                // shape where mongod really does emit `truncatedArrays`.
                for i in 0..b.len() {
                    let cp = child_path(path, &i.to_string());
                    let mut cs = segments.to_vec();
                    cs.push(Bson::Int32(i as i32));
                    if i >= a.len() {
                        acc.updated.insert(cp.clone(), b[i].clone());
                        record_ambiguous(&cp, &cs, acc);
                        continue;
                    }
                    walk(&a[i], &b[i], &cp, &cs, acc)?;
                }
                if b.len() < a.len() {
                    let mut entry = Document::new();
                    entry.insert("field".to_string(), Bson::String(path.to_string()));
                    entry.insert("newSize".to_string(), Bson::Int32(b.len() as i32));
                    acc.truncated.push(Bson::Document(entry));
                    record_ambiguous(path, segments, acc);
                }
                return Ok(());
            };
            let beyond = ew.get(path);
            // Not element-wise, it shrank, or a `$push` onto an EMPTY array:
            // mongod resends the array. `$push` onto `[]` is `updatedFields:
            // {a: [x]}`, while `$addToSet` onto `[]`, or a `$set` of an index
            // past its end, is `{a.0: x}` (measured 8.2.11, 2026-10-01).
            if beyond.is_none()
                || b.len() < a.len()
                || (a.is_empty() && acc.push_targets.contains(path))
            {
                // mongod resends the array.
                acc.updated.insert(path.to_string(), post.clone());
                record_ambiguous(path, segments, acc);
                return Ok(());
            }
            let allowed = beyond.and_then(|v| v.clone());
            for i in 0..b.len() {
                let cp = child_path(path, &i.to_string());
                let mut cs = segments.to_vec();
                cs.push(Bson::Int32(i as i32));
                if i >= a.len() {
                    // An append reports every index it wrote; an indexed `$set`
                    // reports only the one it named.
                    if let Some(named) = allowed.as_ref() {
                        if !named.contains(&(i as i64)) {
                            continue;
                        }
                    }
                    acc.updated.insert(cp.clone(), b[i].clone());
                    record_ambiguous(&cp, &cs, acc);
                    continue;
                }
                walk(&a[i], &b[i], &cp, &cs, acc)?;
            }
            Ok(())
        }
        _ => {
            if !same_stored_value(pre, post)? {
                acc.updated.insert(path.to_string(), post.clone());
                record_ambiguous(path, segments, acc);
            }
            Ok(())
        }
    }
}

/// Operators whose effect on an array mongod reports element-wise, provided
/// they are a plain append (`$slice` / `$sort` / `$position` reorder or shrink
/// the array, and mongod then sends the whole thing).
const APPEND_OPS: &[&str] = &["$push", "$addToSet"];
const NON_APPEND_MODIFIERS: &[&str] = &["$slice", "$sort", "$position"];
/// Operators that write ONE named path. An indexed path under any of them makes
/// that array element-wise -- checked against mongod 8.2.11.
const PATH_WRITE_OPS: &[&str] = &[
    "$set",
    "$unset",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$currentDate",
    "$bit",
];

/// Array paths this update touches in a way mongod reports element-wise.
/// Mirrors `_elementwise_array_paths` in `src/secantus/diff.py`.
fn elementwise_array_paths(update: &Document) -> BTreeMap<String, Option<BTreeSet<i64>>> {
    let mut paths: BTreeMap<String, Option<BTreeSet<i64>>> = BTreeMap::new();
    for (op, spec) in update.iter() {
        let Some(spec) = spec.as_document() else {
            continue;
        };
        if APPEND_OPS.contains(&op.as_str()) {
            for (field, value) in spec.iter() {
                if let Some(d) = value.as_document() {
                    if NON_APPEND_MODIFIERS.iter().any(|m| d.contains_key(*m)) {
                        continue; // reorders or truncates -> whole array
                    }
                }
                paths.insert(field.clone(), None);
            }
        } else if PATH_WRITE_OPS.contains(&op.as_str()) {
            for field in spec.keys() {
                let parts: Vec<&str> = field.split('.').collect();
                for (i, part) in parts.iter().enumerate() {
                    // A positional token (`$`, `$[]`, `$[<id>]`) writes elements
                    // of the array before it, like a numeric index: mongod
                    // reports `a.1`, not the whole array (measured 8.2.11,
                    // 2026-10-01). Which elements is only known after the query,
                    // so any index is allowed.
                    if i > 0 && part.starts_with('$') {
                        paths.insert(parts[..i].join("."), None);
                        continue;
                    }
                    if i == 0 || !part.chars().all(|c| c.is_ascii_digit()) {
                        continue;
                    }
                    let prefix = parts[..i].join(".");
                    match paths.get(&prefix) {
                        Some(None) => continue, // an append already allowed any index
                        _ => {
                            let entry =
                                paths.entry(prefix).or_insert_with(|| Some(BTreeSet::new()));
                            if let Some(set) = entry.as_mut() {
                                if let Ok(n) = part.parse::<i64>() {
                                    set.insert(n);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    paths
}

/// The fields a `$push` appends to.
fn push_targets(update: &Document) -> BTreeSet<String> {
    match update.get("$push") {
        Some(Bson::Document(d)) => d.keys().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

/// `{updatedFields, removedFields, truncatedArrays}` for `pre` -> `post`.
/// `Err(Fallback::Defer)` => defer to the pure-Python implementation.
pub fn compute_update_description(pre: &Document, post: &Document) -> R<Document> {
    compute_update_description_for(pre, post, None)
}

/// As above, told which update produced `post`. `update` is the operator
/// document; pass `None` for a pipeline update or when the spec is unknown, and
/// arrays are diffed by value (which is what mongod does for pipelines).
pub fn compute_update_description_for(
    pre: &Document,
    post: &Document,
    update: Option<&Document>,
) -> R<Document> {
    let mut acc = Acc {
        updated: Document::new(),
        removed: Vec::new(),
        truncated: Vec::new(),
        disambiguated: Document::new(),
        elementwise: update.map(elementwise_array_paths),
        push_targets: update.map(push_targets).unwrap_or_default(),
    };
    walk_docs(pre, post, "", &[], &mut acc)?;
    if let Some(update) = update {
        report_whole_values(update, post, &mut acc);
    }
    let mut out = Document::new();
    out.insert("updatedFields".to_string(), Bson::Document(acc.updated));
    out.insert("removedFields".to_string(), Bson::Array(acc.removed));
    out.insert("truncatedArrays".to_string(), Bson::Array(acc.truncated));
    if !acc.disambiguated.is_empty() {
        // Only stamped when an ambiguous path exists (mirrors Python).
        out.insert(
            "disambiguatedPaths".to_string(),
            Bson::Document(acc.disambiguated),
        );
    }
    Ok(out)
}

/// mongod describes a modifier update by what each operator TOUCHED, not by
/// diffing the documents: a `$set` of a field reports the field's whole new
/// value, and so does a `$rename` target. So `$set: {a: {x: 1, z: 3}}` over
/// `a: {x: 1, y: 2}` is `updatedFields: {a: {x: 1, z: 3}}` on mongod, where a
/// value diff says `{a.z: 3}` plus `removedFields: [a.y]` -- the same change,
/// described in a shape a driver applying deltas to a cached copy reads
/// differently. A `$rename` target is reported even when the value it receives
/// equals the one it replaces (`updatedFields: {c: []}`); a `$set` that leaves
/// its field unchanged is still no change. Measured against mongod 8.2.11,
/// 2026-10-01 (`tools/probes/update_description.py`).
///
/// Applied after the value diff: every entry at or below such a path collapses
/// into one entry for the path itself. Positional (`$`, `$[]`, `$[<id>]`) paths
/// are left to the diff, which already reports the resolved element.
fn report_whole_values(update: &Document, post: &Document, acc: &mut Acc) {
    let mut targets: Vec<(String, bool)> = Vec::new();
    for (op, payload) in update {
        let Bson::Document(fields) = payload else {
            continue;
        };
        match op.as_str() {
            "$set" => {
                for path in fields.keys() {
                    if !path.split('.').any(|p| p.starts_with('$')) {
                        targets.push((path.clone(), false));
                    }
                }
            }
            "$rename" => {
                for to in fields.values() {
                    if let Bson::String(to) = to {
                        targets.push((to.clone(), true));
                    }
                }
            }
            _ => {}
        }
    }
    for (path, always) in targets {
        let Some(value) = crate::paths::get_path(post, &path) else {
            continue;
        };
        let below = |p: &str| p == path || p.starts_with(&format!("{path}."));
        let had_updated = acc.updated.keys().any(|k| below(k));
        let mut touched = had_updated;
        let keep: Vec<(String, Bson)> = acc
            .updated
            .iter()
            .filter(|(k, _)| !below(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let before_removed = acc.removed.len();
        acc.removed
            .retain(|r| !matches!(r, Bson::String(p) if below(p)));
        touched |= acc.removed.len() != before_removed;
        let before_truncated = acc.truncated.len();
        acc.truncated
            .retain(|t| !matches!(t, Bson::Document(d) if d.get_str("field").is_ok_and(below)));
        touched |= acc.truncated.len() != before_truncated;
        if touched || always {
            let mut updated = Document::new();
            for (k, v) in keep {
                updated.insert(k, v);
            }
            updated.insert(path.clone(), value.clone());
            acc.updated = updated;
            let stale: Vec<String> = acc
                .disambiguated
                .keys()
                .filter(|k| below(k))
                .cloned()
                .collect();
            for k in stale {
                acc.disambiguated.remove(&k);
            }
        }
    }
}

// --- pipeline updates: mongod's own diff --------------------------------------
//
// A PIPELINE update is not described by what operators touched (there are none):
// mongod diffs the two documents into its `$v: 2` oplog diff, and logs a full
// REPLACEMENT instead whenever that diff is not clearly smaller than the new
// document. Both halves are observable -- the event is `replace` rather than
// `update`, and an `update`'s `updatedFields` follows the diff's own choices (a
// changed sub-document is reported whole when that is smaller than its
// sub-diff). Measured against mongod 8.2.11, 2026-10-01
// (`tools/probes/update_description.py`, `change_stream_fuzz.py`):
//
// * a diff is logged iff `bsonsize(diff) + 15 < bsonsize(post)` -- the 15 is
//   the oplog entry's own overhead, and the boundary was found by growing an
//   untouched field one byte at a time on two different diff shapes;
// * fields are compared in lockstep while their names line up; once the order
//   diverges, the rest of `post` is INSERTED and the rest of `pre` that `post`
//   lacks is DELETED;
// * a document or array that changed gets a sub-diff only when the sub-diff is
//   smaller than the new value; otherwise the whole value is an update.

enum Node {
    Doc(DocDiff),
    Arr(ArrDiff),
}

#[derive(Default)]
struct DocDiff {
    deletes: Vec<String>,
    updates: Vec<(String, Bson)>,
    inserts: Vec<(String, Bson)>,
    subs: Vec<(String, Node)>,
}

#[derive(Default)]
struct ArrDiff {
    new_len: Option<usize>,
    entries: Vec<(usize, ArrEntry)>,
}

enum ArrEntry {
    Update(Bson),
    Sub(Node),
}

fn binary_equal(a: &Bson, b: &Bson) -> bool {
    let mut da = Document::new();
    da.insert("", a.clone());
    let mut db = Document::new();
    db.insert("", b.clone());
    let (mut ba, mut bb) = (Vec::new(), Vec::new());
    da.to_writer(&mut ba).is_ok() && db.to_writer(&mut bb).is_ok() && ba == bb
}

fn bson_size(v: &Bson) -> usize {
    let mut d = Document::new();
    d.insert("", v.clone());
    let mut buf = Vec::new();
    let _ = d.to_writer(&mut buf);
    // document header (4) + type byte (1) + empty name's NUL (1) + trailer (1)
    buf.len().saturating_sub(7)
}

impl Node {
    fn serialize(&self) -> Document {
        match self {
            Node::Doc(d) => {
                let mut out = Document::new();
                if !d.deletes.is_empty() {
                    let mut del = Document::new();
                    for f in &d.deletes {
                        del.insert(f.clone(), false);
                    }
                    out.insert("d", del);
                }
                if !d.updates.is_empty() {
                    let mut u = Document::new();
                    for (f, v) in &d.updates {
                        u.insert(f.clone(), v.clone());
                    }
                    out.insert("u", u);
                }
                if !d.inserts.is_empty() {
                    let mut i = Document::new();
                    for (f, v) in &d.inserts {
                        i.insert(f.clone(), v.clone());
                    }
                    out.insert("i", i);
                }
                for (f, sub) in &d.subs {
                    out.insert(format!("s{f}"), sub.serialize());
                }
                out
            }
            Node::Arr(a) => {
                let mut out = Document::new();
                out.insert("a", true);
                if let Some(n) = a.new_len {
                    out.insert("l", n as i32);
                }
                for (i, e) in &a.entries {
                    match e {
                        ArrEntry::Update(v) => {
                            out.insert(format!("u{i}"), v.clone());
                        }
                        ArrEntry::Sub(sub) => {
                            out.insert(format!("s{i}"), sub.serialize());
                        }
                    }
                }
                out
            }
        }
    }

    fn size(&self) -> usize {
        bson_size(&Bson::Document(self.serialize()))
    }
}

/// The update for one changed value: a sub-diff when both sides are documents
/// (or both arrays) and the sub-diff is smaller than the new value, else the
/// whole value.
fn changed_value(pre: &Bson, post: &Bson) -> Result<Node, Bson> {
    let sub = match (pre, post) {
        (Bson::Document(a), Bson::Document(b)) => diff_doc(a, b).map(Node::Doc),
        (Bson::Array(a), Bson::Array(b)) => diff_arr(a, b).map(Node::Arr),
        _ => None,
    };
    match sub {
        Some(node) if node.size() < bson_size(post) => Ok(node),
        _ => Err(post.clone()),
    }
}

fn diff_doc(pre: &Document, post: &Document) -> Option<DocDiff> {
    let mut d = DocDiff::default();
    let pre_items: Vec<(&String, &Bson)> = pre.iter().collect();
    let post_items: Vec<(&String, &Bson)> = post.iter().collect();
    let mut i = 0;
    while i < pre_items.len() && i < post_items.len() && pre_items[i].0 == post_items[i].0 {
        let (name, a) = pre_items[i];
        let b = post_items[i].1;
        if !binary_equal(a, b) {
            match changed_value(a, b) {
                Ok(node) => d.subs.push((name.clone(), node)),
                Err(v) => d.updates.push((name.clone(), v)),
            }
        }
        i += 1;
    }
    // Past the first name mismatch the order changed (or fields came and went):
    // everything left in `post` is inserted, and what `post` no longer has is
    // deleted.
    for (name, _) in &pre_items[i..] {
        if !post.contains_key(name.as_str()) {
            d.deletes.push((*name).clone());
        }
    }
    for (name, v) in &post_items[i..] {
        d.inserts.push(((*name).clone(), (*v).clone()));
    }
    let empty =
        d.deletes.is_empty() && d.updates.is_empty() && d.inserts.is_empty() && d.subs.is_empty();
    (!empty).then_some(d)
}

fn diff_arr(pre: &[Bson], post: &[Bson]) -> Option<ArrDiff> {
    let mut a = ArrDiff::default();
    for (i, b) in post.iter().enumerate() {
        match pre.get(i) {
            Some(p) if binary_equal(p, b) => {}
            Some(p) => match changed_value(p, b) {
                Ok(node) => a.entries.push((i, ArrEntry::Sub(node))),
                Err(v) => a.entries.push((i, ArrEntry::Update(v))),
            },
            None => a.entries.push((i, ArrEntry::Update(b.clone()))),
        }
    }
    if post.len() < pre.len() {
        a.new_len = Some(post.len());
    }
    (a.new_len.is_some() || !a.entries.is_empty()).then_some(a)
}

fn describe(node: &Node, prefix: &str, segs: &[Bson], acc: &mut Acc) {
    let path = |name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        }
    };
    match node {
        Node::Doc(d) => {
            for f in &d.deletes {
                let mut s = segs.to_vec();
                s.push(Bson::String(f.clone()));
                record_ambiguous(&path(f), &s, acc);
                acc.removed.push(Bson::String(path(f)));
            }
            for (f, v) in d.updates.iter().chain(d.inserts.iter()) {
                let mut s = segs.to_vec();
                s.push(Bson::String(f.clone()));
                record_ambiguous(&path(f), &s, acc);
                acc.updated.insert(path(f), v.clone());
            }
            for (f, sub) in &d.subs {
                let mut s = segs.to_vec();
                s.push(Bson::String(f.clone()));
                describe(sub, &path(f), &s, acc);
            }
        }
        Node::Arr(a) => {
            if let Some(n) = a.new_len {
                let mut t = Document::new();
                t.insert("field", prefix.to_string());
                t.insert("newSize", n as i32);
                acc.truncated.push(Bson::Document(t));
            }
            for (i, e) in &a.entries {
                let mut s = segs.to_vec();
                s.push(Bson::Int32(*i as i32));
                let p = path(&i.to_string());
                match e {
                    ArrEntry::Update(v) => {
                        record_ambiguous(&p, &s, acc);
                        acc.updated.insert(p, v.clone());
                    }
                    ArrEntry::Sub(sub) => describe(sub, &p, &s, acc),
                }
            }
        }
    }
}

/// mongod's per-entry overhead of a delta oplog entry (see the section docs).
const DELTA_OPLOG_OVERHEAD: usize = 15;

/// What mongod logs for a PIPELINE update of `pre` into `post`: `None` when it
/// logs a full replacement (the event is then `replace`), else the
/// `updateDescription` its delta yields. An unchanged document is an empty
/// description, as for any no-op update.
pub fn pipeline_update_description(pre: &Document, post: &Document) -> Option<Document> {
    let mut acc = Acc {
        updated: Document::new(),
        removed: Vec::new(),
        truncated: Vec::new(),
        disambiguated: Document::new(),
        elementwise: None,
        push_targets: BTreeSet::new(),
    };
    if let Some(d) = diff_doc(pre, post) {
        let node = Node::Doc(d);
        if node.size() + DELTA_OPLOG_OVERHEAD >= bson_size(&Bson::Document(post.clone())) {
            return None;
        }
        describe(&node, "", &[], &mut acc);
    }
    let mut out = Document::new();
    out.insert("updatedFields", Bson::Document(acc.updated));
    out.insert("removedFields", Bson::Array(acc.removed));
    out.insert("truncatedArrays", Bson::Array(acc.truncated));
    if !acc.disambiguated.is_empty() {
        out.insert("disambiguatedPaths", Bson::Document(acc.disambiguated));
    }
    Some(out)
}

/// Apply a `$v: 2` `updateDescription` (`{updatedFields, removedFields,
/// truncatedArrays}`) to `doc`, returning the post-image. The inverse of
/// [`compute_update_description`] and the keystone of oplog replay (PITR): it
/// rolls a document forward without re-running the original update operators.
/// Mirrors `secantus.diff.apply_update_description`.
///
/// `disambiguatedPaths` is intentionally not consulted — every path is applied
/// against the real pre-image, whose container types (map vs array) already
/// resolve the numeric-key vs array-index ambiguity that field exists to flag
/// for a blind reader. Order matches Python: updates, then removals, then array
/// truncations.
pub fn apply_update_description(mut doc: Document, diff: &Document) -> R<Document> {
    if let Ok(updated) = diff.get_document("updatedFields") {
        for (path, value) in updated {
            crate::paths::set_path(&mut doc, path.as_str(), value.clone())
                .map_err(|_| Fallback::Defer)?;
        }
    }
    if let Ok(removed) = diff.get_array("removedFields") {
        for p in removed {
            if let Bson::String(path) = p {
                crate::paths::unset_path(&mut doc, path);
            }
        }
    }
    if let Ok(truncated) = diff.get_array("truncatedArrays") {
        for entry in truncated {
            let Bson::Document(e) = entry else { continue };
            let Ok(field) = e.get_str("field") else {
                continue;
            };
            let new_size = e
                .get_i32("newSize")
                .map(|n| n as usize)
                .or_else(|_| e.get_i64("newSize").map(|n| n as usize));
            let Ok(new_size) = new_size else { continue };
            // Clone-truncate-set: release the immutable `get_path` borrow before
            // the mutable `set_path`.
            let shorter = match crate::paths::get_path(&doc, field) {
                Some(Bson::Array(arr)) if new_size < arr.len() => {
                    let mut a = arr.clone();
                    a.truncate(new_size);
                    Some(a)
                }
                _ => None,
            };
            if let Some(a) = shorter {
                crate::paths::set_path(&mut doc, field, Bson::Array(a))
                    .map_err(|_| Fallback::Defer)?;
            }
        }
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn d(pre: Document, post: Document) -> Document {
        compute_update_description(&pre, &post).expect("should not fall back")
    }

    /// A document that merely CONTAINS a NaN has not changed. The old guard
    /// asked `new != old`, and `NaN != NaN`, so an update that touched nothing
    /// rewrote the document and emitted an oplog entry -- a phantom write.
    #[test]
    fn doc_changed_ignores_an_untouched_nan() {
        assert!(!doc_changed(
            &doc! {"a": f64::NAN, "b": 1i32},
            &doc! {"a": f64::NAN, "b": 1i32}
        ));
        assert!(!doc_changed(
            &doc! {"a": {"n": f64::NAN}},
            &doc! {"a": {"n": f64::NAN}}
        ));
        assert!(!doc_changed(
            &doc! {"a": [f64::NAN]},
            &doc! {"a": [f64::NAN]}
        ));
    }

    /// `same_encoding`'s catch-all used to compare DISCRIMINANTS, which is only
    /// sound behind `eq`. `doc_changed` calls it standalone now, so an ordinary
    /// difference must still register.
    #[test]
    fn doc_changed_sees_ordinary_differences() {
        assert!(doc_changed(&doc! {"a": "x"}, &doc! {"a": "y"}));
        assert!(doc_changed(&doc! {"a": 1i32}, &doc! {"a": 2i32}));
        assert!(doc_changed(&doc! {"a": 1i32}, &doc! {"b": 1i32}));
        assert!(doc_changed(&doc! {"a": 1i32}, &doc! {"a": 1i32, "b": 2i32}));
        assert!(doc_changed(
            &doc! {"a": [1i32, 2i32]},
            &doc! {"a": [1i32, 3i32]}
        ));
        assert!(doc_changed(
            &doc! {"a": {"b": "x"}},
            &doc! {"a": {"b": "y"}}
        ));
        assert!(!doc_changed(
            &doc! {"a": 1i32, "b": "x"},
            &doc! {"a": 1i32, "b": "x"}
        ));
    }

    /// A signed zero is a CHANGE even though `==` calls the documents equal.
    /// Before this predicate the storage layer's `new != doc` guard skipped the
    /// write entirely and the caller's `-0.0` was silently never stored.
    #[test]
    fn doc_changed_sees_a_signed_zero() {
        assert!(doc_changed(&doc! {"a": -0.0}, &doc! {"a": 0.0}));
        assert!(doc_changed(&doc! {"a": 0.0}, &doc! {"a": -0.0}));
        // Nested at any depth, and inside an array.
        assert!(doc_changed(
            &doc! {"a": {"b": -0.0}},
            &doc! {"a": {"b": 0.0}}
        ));
        assert!(doc_changed(
            &doc! {"a": [1i32, -0.0]},
            &doc! {"a": [1i32, 0.0]}
        ));
        // A second, genuinely-unchanged field must not mask the changed one.
        assert!(doc_changed(
            &doc! {"a": -0.0, "b": 1i32},
            &doc! {"a": 0.0, "b": 1i32}
        ));
    }

    /// A numeric TYPE change is a change too -- `Bson`'s variant-aware `==`
    /// already catches this one, but it is the other half of what mongod
    /// reports as modified, so pin it.
    #[test]
    fn doc_changed_sees_a_numeric_type_change() {
        assert!(doc_changed(&doc! {"a": 0.0}, &doc! {"a": 0i32}));
        assert!(doc_changed(&doc! {"a": 1.0}, &doc! {"a": 1i32}));
        assert!(doc_changed(&doc! {"a": 1i64}, &doc! {"a": 1i32}));
    }

    /// The negative side: an identical document is not a change, or every
    /// no-op update would write and report `nModified: 1`.
    #[test]
    fn doc_changed_is_false_for_an_identical_document() {
        assert!(!doc_changed(&doc! {"a": 0.0}, &doc! {"a": 0.0}));
        assert!(!doc_changed(&doc! {"a": -0.0}, &doc! {"a": -0.0}));
        assert!(!doc_changed(
            &doc! {"a": {"b": [1i32, "x"]}},
            &doc! {"a": {"b": [1i32, "x"]}}
        ));
        assert!(!doc_changed(&Document::new(), &Document::new()));
    }

    /// This used to assert the OPPOSITE, on the reasoning that `!=` reports a
    /// NaN as a change and the encoding tiebreak "must not quietly reverse
    /// that", or `{$inc: {a: 1}}` over `a: NaN` -- which mongod counts as
    /// modified -- would report 0.
    ///
    /// The premise was right and the conclusion was wrong. Keeping `!=` bought
    /// that one case by making EVERY update on a document containing a NaN look
    /// like a change, including ones that touched nothing at all, which is a
    /// phantom write and a phantom change-stream event (measured 8.2.11,
    /// 2026-09-06). The `$inc`-over-NaN case is real but it is a per-OPERATOR
    /// rule, and it belongs in a per-operator check: `update::arith_wrote_nan`.
    /// Two rules, each answering the question it can actually answer.
    #[test]
    fn doc_changed_does_not_report_an_untouched_nan() {
        assert!(!doc_changed(&doc! {"a": f64::NAN}, &doc! {"a": f64::NAN}));
    }

    #[test]
    fn updated_added_removed() {
        let out = d(doc! {"a": 1, "b": 2}, doc! {"a": 9, "c": 3});
        assert_eq!(
            out.get_document("updatedFields").unwrap(),
            &doc! {"a": 9, "c": 3}
        );
        assert_eq!(
            out.get_array("removedFields").unwrap(),
            &vec![Bson::String("b".into())]
        );
    }

    #[test]
    fn nested_leaf_only() {
        let out = d(doc! {"a": {"b": 1, "c": 2}}, doc! {"a": {"b": 1, "c": 9}});
        assert_eq!(out.get_document("updatedFields").unwrap(), &doc! {"a.c": 9});
    }

    #[test]
    fn a_numeric_type_change_is_a_change() {
        // This asserted the OPPOSITE, justified by `1 == 1.0 -> no update
        // emitted` -- Python's equality rule, cited instead of the server's.
        // mongod reports the change: `{$set: {a: 1.0}}` over a stored `int` 1
        // answers `updatedFields: {a: 1.0}` with `nModified: 1` and stores a
        // double. The consumer of a change stream was never told the field's
        // TYPE had changed (probed 8.2.11, 2026-09-05).
        let out = d(doc! {"a": 1}, doc! {"a": 1.0});
        assert_eq!(out.get_document("updatedFields").unwrap(), &doc! {"a": 1.0});
    }

    #[test]
    fn a_signed_zero_flip_is_a_change() {
        // Same rule, the other shape it was blind to: `0.0` and `-0.0` are
        // EQUAL for `$eq` and for query matching, and DIFFERENT for change
        // detection. Both measured on 8.2.11 (2026-09-05).
        let out = d(doc! {"a": 0.0}, doc! {"a": -0.0});
        assert_eq!(
            out.get_document("updatedFields").unwrap(),
            &doc! {"a": -0.0}
        );
        // ...including nested in an array, which the fast path used to skip.
        let nested = d(doc! {"a": [0.0]}, doc! {"a": [-0.0]});
        assert!(!nested.get_document("updatedFields").unwrap().is_empty());
    }

    #[test]
    fn an_unchanged_value_is_still_no_change() {
        // The guard against over-reporting: same value, same type, no entry.
        let out = d(doc! {"a": 1.0, "b": "x"}, doc! {"a": 1.0, "b": "x"});
        assert!(out.get_document("updatedFields").unwrap().is_empty());
    }

    #[test]
    fn array_truncation() {
        let out = d(doc! {"a": [1, 2, 3, 4]}, doc! {"a": [1, 9, 3]});
        assert_eq!(out.get_document("updatedFields").unwrap(), &doc! {"a.1": 9});
        let trunc = out.get_array("truncatedArrays").unwrap();
        assert_eq!(
            trunc,
            &vec![Bson::Document(doc! {"field": "a", "newSize": 3})]
        );
    }

    /// Growth used to wholesale-replace here. Measured against mongod 8.2.11 it
    /// is reported positionally -- for a pipeline update (no spec) and for a
    /// `$push` alike. Only a whole-field operator `$set` resends the array.
    #[test]
    fn array_growth_reports_appended_indices() {
        let out = d(doc! {"a": [1, 2]}, doc! {"a": [1, 2, 3]});
        assert_eq!(out.get_document("updatedFields").unwrap(), &doc! {"a.2": 3});

        let pushed = compute_update_description_for(
            &doc! {"a": [1, 2]},
            &doc! {"a": [1, 2, 3]},
            Some(&doc! {"$push": {"a": 3}}),
        )
        .unwrap();
        assert_eq!(
            pushed.get_document("updatedFields").unwrap(),
            &doc! {"a.2": 3}
        );

        let whole = compute_update_description_for(
            &doc! {"a": [1, 2]},
            &doc! {"a": [1, 2, 3]},
            Some(&doc! {"$set": {"a": [1, 2, 3]}}),
        )
        .unwrap();
        assert_eq!(
            whole.get_document("updatedFields").unwrap(),
            &doc! {"a": [1, 2, 3]}
        );
    }

    /// A shrink by an OPERATOR resends the array; the same shrink with no spec
    /// (pipeline form) reports a truncation. Both measured on 8.2.11.
    #[test]
    fn array_shrink_depends_on_the_operation() {
        let popped = compute_update_description_for(
            &doc! {"a": [1, 2, 3]},
            &doc! {"a": [1, 2]},
            Some(&doc! {"$pop": {"a": 1}}),
        )
        .unwrap();
        assert_eq!(
            popped.get_document("updatedFields").unwrap(),
            &doc! {"a": [1, 2]}
        );
        assert!(popped.get_array("truncatedArrays").unwrap().is_empty());

        let pipeline = d(doc! {"a": [1, 2, 3]}, doc! {"a": [1, 2]});
        assert_eq!(pipeline.get_array("truncatedArrays").unwrap().len(), 1);
    }

    /// `apply_update_description` is the exact inverse of `compute`: rolling the
    /// pre-image forward by the computed diff reproduces the post-image. Covers
    /// scalar change, nested add, field removal, and array truncation in one go.
    #[test]
    fn apply_inverts_compute() {
        let cases = [
            (doc! {"a": 1}, doc! {"a": 2}),
            (doc! {"a": 1, "b": 2}, doc! {"a": 1}), // removal
            (doc! {"a": {"b": 1}}, doc! {"a": {"b": 1, "c": 9}}), // nested add
            (doc! {"a": [1, 2, 3]}, doc! {"a": [1, 9]}), // truncate + change
            (doc! {"a": [1, 2]}, doc! {"a": [1, 2, 3]}), // growth
            (
                doc! {"x": 1, "y": [1, 2, 3], "z": {"d": 5}, "gone": true},
                doc! {"x": 2, "y": [1, 9], "z": {"d": 5, "e": 7}},
            ),
        ];
        for (pre, post) in cases {
            let diff = compute_update_description(&pre, &post).expect("compute");
            let rolled = apply_update_description(pre.clone(), &diff).expect("apply");
            assert_eq!(rolled, post, "roundtrip failed for pre={pre:?}");
        }
    }
}
