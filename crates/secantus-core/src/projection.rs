//! Projection — Rust port of `secantus.projection.apply_projection`, the fifth
//! leaf engine (`find()`'s `projection` argument). Same graceful-fallback
//! design: reproduce the common inclusion / exclusion / `$slice` / `$elemMatch`
//! shapes, and defer the whole call to Python for anything else (mixed
//! inclusion/exclusion which Python raises on, nested-document specs, unusual
//! `$slice` argument types, or a `$elemMatch` sub-filter the matcher defers).

use bson::{Bson, Document, RawDocument};

use crate::{paths, query};

pub use crate::fallback::Fallback;

type R<T> = Result<T, Fallback>;

/// Raw-BSON projection fast path for the common case: a **pure top-level
/// inclusion** spec (`{a: 1, b: 1}`, optionally with `_id: 0`) — no dotted
/// paths, no operators (`$slice` / `$elemMatch` / `$meta`), no positional, no
/// exclusion. For that shape the output has only `_id` (unless dropped) plus the
/// included fields, so we decode ONLY those from `raw` instead of materialising
/// the whole document (the return-path materialization,
/// `tasks/rust-perf-findings.md`). The result is byte-identical to
/// `apply_projection(&raw.to_document(), spec, None)` — including field order,
/// which is the stored document's, as mongod's is. Returns `None` for any spec
/// this fast path doesn't cover, so the caller falls back to the full
/// `apply_projection` on a decoded document.
pub fn apply_projection_raw(raw: &RawDocument, spec: &Document) -> Option<Document> {
    if spec.is_empty() {
        return None; // whole-doc copy — caller/splice handles it
    }
    let mut include_fields: Vec<&str> = Vec::new();
    let mut include_id = true;
    for (k, v) in spec {
        if k.contains('.') || k.ends_with(".$") {
            return None; // dotted / positional -> full projection
        }
        // Only canonical inclusion scalars; a document value is an operator
        // ($slice/$elemMatch/$meta), and a string / null / exotic `_id` spec has
        // special rules — defer all of those.
        let truthy = match v {
            Bson::Int32(n) => *n != 0,
            Bson::Int64(n) => *n != 0,
            Bson::Double(d) => *d != 0.0,
            Bson::Boolean(b) => *b,
            _ => return None,
        };
        if k == "_id" {
            include_id = truthy;
        } else if truthy {
            include_fields.push(k.as_str());
        } else {
            return None; // exclusion of a real field -> not pure inclusion
        }
    }
    if include_fields.is_empty() {
        return None; // `_id`-only / degenerate specs -> let the full path handle
    }
    // The stored document's order, as `include_doc` and mongod both emit it
    // (probed 8.2.11, 2026-10-15: `{op, ns, o}` projected on `op, ns, o`
    // stays `op, ns, o`). This path sorted the names, which matched the full
    // path only before that path was fixed to walk the document.
    let mut out = Document::new();
    for element in raw {
        let (key, value) = element.ok()?;
        let kept = if key == "_id" {
            include_id
        } else {
            include_fields.contains(&key)
        };
        if kept && !out.contains_key(key) {
            out.insert(key.to_string(), Bson::try_from(value).ok()?);
        }
    }
    Some(out)
}

fn elem_match_spec(v: &Bson) -> Option<&Bson> {
    if let Bson::Document(d) = v {
        if d.len() == 1 {
            if let Some(sub) = d.get("$elemMatch") {
                return Some(sub);
            }
        }
    }
    None
}

/// A `{$meta: <arg>}` projection value. SecantusDB doesn't compute any metadata,
/// so a recognized `$meta` field is *omitted* from the projected doc (partial —
/// graceful degradation). The two error cases (an unknown argument → Location17308,
/// `textScore` without a `$text` query → Location40218) are validated at parse
/// time in the command layer (`find::projection_meta_error`), which owns the
/// `CommandError` type; here we just recognize and drop the field.
pub fn meta_spec(v: &Bson) -> Option<&str> {
    if let Bson::Document(d) = v {
        if d.len() == 1 {
            if let Some(Bson::String(arg)) = d.get("$meta") {
                return Some(arg.as_str());
            }
        }
    }
    None
}

/// The `$meta` keywords mongod recognizes. Anything else is a Location17308.
pub const META_KEYWORDS: &[&str] = &[
    "textScore",
    "indexKey",
    "recordId",
    "sortKey",
    "searchScore",
    "searchHighlights",
    "geoNearDistance",
    "geoNearPoint",
    "vectorSearchScore",
    // Answer missing outside the stages that set them (measured 8.2.11,
    // 2026-10-22); they used to be refused as unknown (17308).
    "randVal",
    "searchScoreDetails",
    "searchSequenceToken",
];

/// The argument of a FIND `$slice`: `{$slice: n}` or `{$slice: [skip,
/// limit]}`. Any other `$slice` is the aggregation expression, a computed
/// field (mongod 8.2.11, 2026-10-24: `{y: {$slice: ["$arr", 1]}}` computes
/// `y`).
fn slice_spec(v: &Bson) -> Option<&Bson> {
    if let Bson::Document(d) = v {
        if d.len() == 1 {
            if let Some(arg) = d.get("$slice") {
                return find_slice_problem(arg).is_none().then_some(arg);
            }
        }
    }
    None
}

fn is_number(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

/// Why `arg` is not find's `$slice` syntax, as mongod's parser says it
/// (measured 8.2.11, 2026-10-24); None when it is.
pub fn find_slice_problem(arg: &Bson) -> Option<(i32, String)> {
    let Bson::Array(args) = arg else {
        return (!is_number(arg)).then(|| {
            (
                31273,
                "$slice only supports numbers and [skip, limit] arrays".to_string(),
            )
        });
    };
    let [skip, limit] = args.as_slice() else {
        return Some((
            31272,
            "$slice array argument should be of form [skip, limit]".to_string(),
        ));
    };
    if !is_number(skip) {
        return Some((
            31257,
            format!(
                "$slice expects the skip argument to be a number, got {}",
                crate::query::bson_type_name(skip)
            ),
        ));
    }
    if !is_number(limit) {
        return Some((
            31258,
            format!(
                "$slice expects the limit argument to be a number, got {}",
                crate::query::bson_type_name(limit)
            ),
        ));
    }
    let positive = match limit {
        Bson::Int32(n) => *n > 0,
        Bson::Int64(n) => *n > 0,
        Bson::Double(d) => *d > 0.0,
        Bson::Decimal128(d) => d.to_string().parse::<f64>().is_ok_and(|l| l > 0.0),
        _ => false,
    };
    (!positive).then(|| {
        (
            31259,
            format!(
                "$slice limit must be positive, got {}",
                crate::query::bson_value_repr(limit)
            ),
        )
    })
}

/// Truthiness of an inclusion/exclusion spec value. Only the literal 0/1 /
/// bool / double forms are reproduced; anything else (nested doc, string, …)
/// defers (Python's quirky `bool()` semantics).
fn spec_truthy(v: &Bson) -> R<bool> {
    match v {
        Bson::Int32(n) => Ok(*n != 0),
        Bson::Int64(n) => Ok(*n != 0),
        Bson::Boolean(b) => Ok(*b),
        Bson::Double(d) => Ok(*d != 0.0),
        // A `Decimal128` is a BSON number, so mongod treats it as a FLAG:
        // `Decimal128("1.5")` includes and `Decimal128("0")` excludes (measured
        // 8.2.11, 2026-09-07). This used to fall through to the defer arm, which
        // on the Rust server is an error rather than a fallback. Rendering to
        // text and parsing keeps every zero spelling (`0`, `0.00`, `-0`, `0E+3`)
        // falsy; `NaN` parses to NaN, which is `!= 0.0`, so it includes -- the
        // same answer Python's `_flag_truthy` gives via its explicit NaN test.
        Bson::Decimal128(d) => Ok(d
            .to_string()
            .parse::<f64>()
            .map(|f| f != 0.0)
            .unwrap_or(true)),
        _ => Err(Fallback::Defer),
    }
}

/// Whether an `_id` spec value means "drop `_id`" (Python compares `== 0`,
/// which is true for `0` / `false` / `0.0`).
fn is_drop_id(v: &Bson) -> bool {
    matches!(v, Bson::Int32(0) | Bson::Int64(0) | Bson::Boolean(false))
        || matches!(v, Bson::Double(d) if *d == 0.0)
        || matches!(v, Bson::Decimal128(_) if !flag_truthy(v))
}

fn as_slice_int(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        Bson::Double(d) if d.is_finite() => Some(*d as i64),
        _ => None,
    }
}

/// A valid projection `$slice` argument (mongod): a number, or a `[skip, limit]`
/// pair with a positive numeric limit. Returns `Some((skip, limit))` for the pair
/// form, `Some((n, i64::MIN))` sentinel for the scalar form, else `None` (Python
/// raises 28667 / 28724). The scalar sentinel keeps the two forms distinguishable.
fn slice_bounds(slice_arg: &Bson) -> Option<(i64, i64)> {
    if let Some(n) = as_slice_int(slice_arg) {
        return Some((n, i64::MIN));
    }
    if let Bson::Array(args) = slice_arg {
        if args.len() == 2 {
            if let (Some(skip), Some(limit)) = (as_slice_int(&args[0]), as_slice_int(&args[1])) {
                if limit > 0 {
                    return Some((skip, limit));
                }
            }
        }
    }
    None
}

fn apply_slice(value: Bson, slice_arg: &Bson) -> R<Bson> {
    // Validate the argument shape up front (mongod parse-time) so an invalid
    // $slice on a non-array field also defers to Python.
    let Some((first, limit)) = slice_bounds(slice_arg) else {
        return Err(Fallback::Defer);
    };
    let Bson::Array(a) = &value else {
        return Ok(value); // non-array passes through unchanged
    };
    let len = a.len() as i64;
    if limit == i64::MIN {
        // Scalar form: first / last `first` elements.
        let out = if first >= 0 {
            a[..first.min(len).max(0) as usize].to_vec()
        } else {
            a[(len + first).max(0) as usize..].to_vec()
        };
        return Ok(Bson::Array(out));
    }
    // [skip, limit] with a positive limit.
    let skip = if first < 0 {
        (len + first).max(0)
    } else {
        first.min(len)
    };
    let tail = &a[skip as usize..];
    let out = tail[..(limit as usize).min(tail.len())].to_vec();
    Ok(Bson::Array(out))
}

/// A positional projection's parse-time checks (mongod 8.2.11, 2026-10-24):
/// one per query (31276), never an exclusion (31395), and no `$expr` in the
/// filter beside it (224). Whether the filter names the array is not one of
/// them: the position is whatever the query's match recorded
/// (`elem_match_key`), and none is an error per document (51246).
pub fn validate_projection(spec: &Document, query: Option<&Document>) -> R<()> {
    // Path collisions are the projection parser's (`expr_parse::projection_spec`),
    // which runs before this and walks the tree as mongod does.
    let positional: Vec<&String> = spec.keys().filter(|k| k.ends_with(".$")).collect();
    if positional.is_empty() {
        return Ok(());
    }
    if positional.len() > 1 {
        return Err(Fallback::mongo(
            31276,
            "Cannot specify more than one positional projection per query.",
        )
        .bare());
    }
    if !spec_truthy(&spec[positional[0]])? {
        return Err(
            Fallback::mongo(31395, "positional projection cannot be used with exclusion").bare(),
        );
    }
    if query.is_some_and(filter_has_expr) {
        return Err(Fallback::mongo(224, "$expr is not allowed in this context").bare());
    }
    Ok(())
}

fn filter_has_expr(filter: &Document) -> bool {
    filter.iter().any(|(k, v)| match (k.as_str(), v) {
        ("$expr", _) => true,
        ("$and" | "$or" | "$nor", Bson::Array(subs)) => subs
            .iter()
            .any(|s| matches!(s, Bson::Document(d) if filter_has_expr(d))),
        _ => false,
    })
}

/// The field a positional `path.$` projects, as `(the path to set, its
/// value)`: the element at `key` of the first array on `path`, the rest of
/// the path projected within it; a value no array holds, as it is; nothing
/// for a missing one. An array with no recorded position is 51246, and one
/// too short for it 51247 (mongod 8.2.11, 2026-10-24).
fn positional_field(doc: &Document, path: &str, key: Option<usize>) -> R<Option<(String, Bson)>> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = doc;
    for (i, part) in parts.iter().enumerate() {
        let Some(value) = cur.get(*part) else {
            return Ok(None);
        };
        let last = i + 1 == parts.len();
        match value {
            Bson::Array(a) => {
                let key = key.ok_or_else(|| {
                    Fallback::mongo(
                        51246,
                        "positional operator '.$' couldn't find a matching element in the array",
                    )
                })?;
                let element = a.get(key).ok_or_else(|| {
                    Fallback::mongo(51247, "positional operator '.$' element mismatch")
                })?;
                let at = parts[..=i].join(".");
                let projected = if last {
                    element.clone()
                } else {
                    match element {
                        Bson::Document(ed) => {
                            let rest = parts[i + 1..].join(".");
                            Bson::Document(include_doc(ed, &spec_tree(&[rest.as_str()])))
                        }
                        _ => return Ok(Some((at, Bson::Array(Vec::new())))),
                    }
                };
                return Ok(Some((at, Bson::Array(vec![projected]))));
            }
            _ if last => return Ok(Some((path.to_string(), value.clone()))),
            Bson::Document(d) => cur = d,
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// Inclusion projection carrying a positional `arr.$`: the other requested fields
/// plus `array_path: [first-matching-element]`. Mirrors `projection._apply_positional`.
fn apply_positional(
    doc: &Document,
    spec_main: &[(&str, &Bson)],
    slice_specs: &[(&str, &Bson)],
    array_path: &str,
    key: Option<usize>,
) -> R<Document> {
    let mut result = Document::new();
    let id_spec = spec_main.iter().find(|(k, _)| *k == "_id").map(|(_, v)| *v);
    let include_id = match id_spec {
        None => true,
        Some(v) => spec_truthy(v)?,
    };
    if include_id {
        if let Some(id) = doc.get("_id") {
            result.insert("_id".to_string(), id.clone());
        }
    }
    let non_id: Vec<(&str, &Bson)> = spec_main
        .iter()
        .copied()
        .filter(|(k, _)| *k != "_id")
        .collect();
    // Positional forces inclusion; a companion exclusion is the mix mongod rejects.
    for (_, v) in &non_id {
        if elem_match_spec(v).is_none() && !spec_truthy(v)? {
            return Err(Fallback::Defer); // Location31254 -> Python
        }
    }
    let plain: Vec<&str> = non_id
        .iter()
        .filter(|(_, v)| elem_match_spec(v).is_none())
        .map(|(k, _)| *k)
        .collect();
    if !plain.is_empty() {
        for (k, v) in include_doc(doc, &spec_tree(&plain)) {
            result.insert(k, v);
        }
    }
    for (path, value) in &non_id {
        if let Some(sub) = elem_match_spec(value) {
            let Bson::Document(subf) = sub else {
                return Err(Fallback::Defer);
            };
            if let Some(first) = first_match(doc, path, subf)? {
                set(&mut result, path, Bson::Array(vec![first]))?;
            }
        }
    }
    if let Some((at, value)) = positional_field(doc, array_path, key)? {
        set(&mut result, &at, value)?;
    }
    for (path, slice_arg) in slice_specs {
        if !paths::has_path(&result, path) {
            if let Some(extracted) = paths::get_path(doc, path) {
                set(&mut result, path, extracted.clone())?;
            }
        }
        if let Some(current) = paths::get_path(&result, path).cloned() {
            let sliced = apply_slice(current, slice_arg)?;
            set(&mut result, path, sliced)?;
        }
    }
    Ok(result)
}

/// First array element under `path` matching `sub_filter` (`$elemMatch`
/// projection). `Ok(None)` for no match / non-array.
/// The first array element satisfying a `$elemMatch` projection criterion.
///
/// The criterion's SHAPE decides how each element is tested, decided once for
/// the whole array rather than per element: a criterion of only `$`-operators
/// (`{$gt: 2}`) is an ELEMENT-VALUE predicate, and anything else is a per-FIELD
/// predicate applied to the element as a document. Mirrors
/// `projection._first_match`, and the same rule `update._pull_matches` carries.
///
/// The value predicate tests each element as a SINGLE VALUE with no implicit
/// one-level array traversal, which is why it builds a NON-EXPANDABLE `Cand`
/// rather than going through `matches` with a wrapper document. Measured
/// 8.2.11 (2026-09-09) -- `$size` and `$eq` still see the element as the array
/// it is, so it is the traversal being suppressed, not the type:
///
/// ```text
///     [[1, 2], [3, 4]]  {$gt: 2}      -> omitted
///     [1, [3, 4], 5]    {$gt: 2}      -> [5]
///     [[1, 2], [3]]     {$size: 2}    -> [[1, 2]]
///     [[1, 2], [3, 4]]  {$eq: [3, 4]} -> [[3, 4]]
///     [1, 2, 3]         {$gt: 2}      -> [3]
/// ```
///
/// Branching on whether the ELEMENT was a document sent a bare operator
/// criterion over an array of documents into `matches(ed, {"$gt": 2})`, which
/// raised `2 unknown top level operator: $gt` -- a valid projection refused.
fn first_match(doc: &Document, path: &str, sub_filter: &Document) -> R<Option<Bson>> {
    let Some(Bson::Array(arr)) = paths::get_path(doc, path) else {
        return Ok(None);
    };
    let empty = Document::new();
    let value_predicate = !sub_filter.is_empty() && sub_filter.keys().all(|k| k.starts_with('$'));
    let cond = Bson::Document(sub_filter.clone());
    for elem in arr {
        let hit = match elem {
            Bson::Document(ed) if !value_predicate => query::matches(ed, sub_filter, &empty, None)?,
            other => {
                let cands = [query::Cand::new(Some(other), false)];
                query::field_matches(&cands, &cond, crate::collation::active().as_ref(), "_")?
            }
        };
        if hit {
            return Ok(Some(elem.clone()));
        }
    }
    Ok(None)
}

fn set(doc: &mut Document, path: &str, value: Bson) -> R<()> {
    paths::set_path(doc, path, value).map_err(|_| Fallback::Defer)
}

/// Is this projection value an include/exclude FLAG rather than a value?
///
/// Measured against mongod 8.2.11 (2026-09-07) by projecting one of each BSON
/// type: **only a number or a bool is a flag.** `Decimal128("1.5")` includes and
/// `Decimal128("0")` excludes, so the test is "is it a BSON number", not "is it
/// an integer". Everything else -- string, null, array, date, ObjectId, BinData,
/// regex, Timestamp, MinKey, MaxKey, Code -- is a *literal constant* that
/// replaces the field on every document. Mirrors `projection.py::_is_flag_value`.
pub fn is_flag_value(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Boolean(_) | Bson::Decimal128(_)
    )
}

/// Truthiness of a flag value. A `Decimal128` needs its own zero test -- it is
/// an object, so a plain "is it falsy" check calls every decimal truthy.
/// NaN is NOT zero, so it includes. Mirrors `projection.py::_flag_truthy`.
pub(crate) fn flag_truthy(v: &Bson) -> bool {
    spec_truthy(v).unwrap_or(true)
}

/// Is this (already flattened) projection value an EXPRESSION?
///
/// Every rule measured against mongod 8.2.11 (2026-09-07):
///
/// * a **string** is an expression -- `"$a"` is a field path and `"plain"` a
///   literal constant on every document. Neither is a flag.
/// * an operator **document** (first key `$`-prefixed) is an expression, except
///   the three projection operators, which have their own handling. Note
///   `{$literal: 0}` yields the value 0 -- it is NOT an exclusion.
/// * every other non-flag value (null, array, date, ...) is a literal constant.
///
/// A plain sub-document never reaches here: `flatten_projection_spec` has
/// already split it into dotted leaves, because mongod classifies a
/// sub-document PER LEAF -- `{o: {p: 1, z: "$b"}}` includes `o.p` *and*
/// computes `o.z`. Mirrors `projection.py::_is_computed_spec`.
pub fn is_computed_spec(v: &Bson) -> bool {
    if is_flag_value(v) {
        return false;
    }
    match v {
        Bson::Document(d) => {
            // $slice / $elemMatch / $meta have their own handling upstream.
            slice_spec(v).is_none()
                && elem_match_spec(v).is_none()
                && meta_spec(v).is_none()
                && d.keys().next().map(|k| k.starts_with('$')).unwrap_or(false)
        }
        _ => true,
    }
}

/// Split plain sub-documents into dotted leaves, preserving spec order.
///
/// mongod treats `{o: {p: 1, z: "$b"}}` as the two independent leaves `o.p: 1`
/// and `o.z: "$b"`, returning `{o: {p: <stored>, z: <computed>}}` -- so a
/// sub-document cannot be classified as a whole. An empty sub-document at any
/// depth is its own error (51270). Mirrors
/// `projection.py::_flatten_projection_spec`.
pub(crate) fn flatten_projection_spec(spec: &Document) -> R<Vec<(String, Bson)>> {
    let mut out: Vec<(String, Bson)> = Vec::new();
    for (k, v) in spec {
        flatten_entry(k, v, &mut out)?;
    }
    Ok(out)
}

fn flatten_entry(key: &str, value: &Bson, out: &mut Vec<(String, Bson)>) -> R<()> {
    if let Bson::Document(d) = value {
        if d.is_empty() {
            let leaf = key.rsplit('.').next().unwrap_or(key);
            return Err(Fallback::mongo(
                51270,
                format!("Invalid empty sub-projection: {leaf}"),
            ));
        }
        // `$id` / `$ref` / `$db` are DBRef field names, not operators.
        let first_is_op = d.keys().next().is_some_and(|k| {
            crate::expr_parse::field_name_problem(k).is_some() && k.starts_with('$')
        });
        if first_is_op {
            out.push((key.to_string(), value.clone()));
            return Ok(());
        }
        for (sub_key, sub_value) in d {
            flatten_entry(&format!("{key}.{sub_key}"), sub_value, out)?;
        }
        return Ok(());
    }
    out.push((key.to_string(), value.clone()));
    Ok(())
}

/// Evaluate the projection's expression-valued fields against `doc`.
///
/// Two rules that look alike and are not (both measured against mongod 8.2.11):
///
/// * a bare field REFERENCE that resolves to nothing OMITS the output field --
///   `{n: "$absent"}` gives `{_id: 1}`, with no `n` at all;
/// * an EXPRESSION over a missing field yields **null** --
///   `{n: {$add: ["$absent", 1]}}` gives `{_id: 1, n: null}`.
///
/// `evaluate_or_missing` is the field-value evaluator that draws exactly that
/// line. A dotted output key builds the nesting via `set`, never a key with a
/// literal dot in it. Mirrors `projection.py::_with_computed`.
fn with_computed(
    result: &mut Document,
    doc: &Document,
    computed: &[(String, Bson)],
    vars: &Document,
) -> R<()> {
    for (key, expr) in computed {
        let value = crate::expressions::evaluate_or_missing(doc, expr, vars)?;
        if matches!(value, Bson::Undefined) {
            continue; // the bare-reference case: leave the field out entirely
        }
        set(result, key, value)?;
    }
    Ok(())
}

/// Apply a projection spec to a document. `Err(Fallback::Defer)` => defer to the
/// pure-Python `apply_projection` (which also raises the mixed-mode error).
pub fn apply_projection(doc: &Document, spec: &Document, query: Option<&Document>) -> R<Document> {
    apply_projection_with(doc, spec, query, &Document::new())
}

/// [`apply_projection`] with the command's variables (its `let`, `$$NOW`),
/// which a computed field may read.
pub fn apply_projection_with(
    doc: &Document,
    spec: &Document,
    query: Option<&Document>,
    vars: &Document,
) -> R<Document> {
    if spec.is_empty() {
        return Ok(doc.clone());
    }

    // SecantusDB doesn't compute metadata, so a `$meta` field is *omitted* — but
    // the rest of the document is untouched. `$meta` does NOT participate in
    // inclusion / exclusion mode detection, the same rule `$slice` and
    // positional follow (mongod treats all three as value re-shapers).
    // Oracle-pinned against mongod 6.0.16:
    //
    //   {m: {$meta: X}}          -> the WHOLE document
    //   {_id: 0, m: {$meta: X}}  -> whole document minus _id
    //   {a: 1,   m: {$meta: X}}  -> {_id, a}   (the `a: 1` drives inclusion)
    //   {b: 0,   m: {$meta: X}}  -> exclusion, driven by `b: 0`
    //
    // This used to force a `$meta`-only spec into an inclusion projection of no
    // fields, mirroring a pure-Python comment that asserted "result: just
    // `_id`" — which is not what mongod does. Asking for a metadata field
    // silently discarded the caller's entire document. Parse-time validation
    // (Location17308 / 40218) still lives in `find::projection_meta_error`.
    if spec.values().any(|v| meta_spec(v).is_some()) {
        let mut stripped = Document::new();
        for (k, v) in spec {
            if meta_spec(v).is_none() {
                stripped.insert(k.clone(), v.clone());
            }
        }
        return apply_projection_with(doc, &stripped, query, vars);
    }

    // COMPUTED fields (`{x: "$a"}`, `{x: {$add: [...]}}`, a literal constant).
    // These are flattened first because mongod classifies a plain sub-document
    // PER LEAF, then split out of the flag set: they never participate in
    // inclusion/exclusion detection, they FORCE inclusion mode, and they are
    // evaluated after the flag projection has produced its result.
    //
    // Positional (`arr.$`) keeps its own path below and is left alone here.
    if !spec.keys().any(|k| k.ends_with(".$")) {
        let flattened = flatten_projection_spec(spec)?;
        if flattened.iter().any(|(_, v)| is_computed_spec(v)) {
            let mut flags = Document::new();
            let mut computed: Vec<(String, Bson)> = Vec::new();
            for (k, v) in flattened {
                if is_computed_spec(&v) {
                    computed.push((k, v));
                } else {
                    flags.insert(k, v);
                }
            }
            // A computed field forces INCLUSION mode, so a companion exclusion
            // is the mix mongod rejects with Location31254. `_id: 0` is the one
            // exclusion that is always legal.
            for (k, v) in &flags {
                if k != "_id" && is_flag_value(v) && !flag_truthy(v) {
                    return Err(Fallback::mongo(
                        31254,
                        format!("Cannot do exclusion on field {k} in inclusion projection"),
                    ));
                }
            }
            // The flag half is an ordinary inclusion projection. With no
            // NON-`_id` flags left it degenerates to `_id` only, which is
            // mongod's answer for a computed-only spec -- and `_id: 0` there
            // means "drop `_id` from an inclusion", NOT "exclusion projection".
            // Passing `{_id: 0}` down as a spec of its own read as the latter
            // and returned the whole document.
            let has_non_id_flag = flags.keys().any(|k| k != "_id");
            let mut result = if !has_non_id_flag {
                let mut only_id = Document::new();
                let keep_id = flags
                    .get("_id")
                    .map(|v| !is_flag_value(v) || flag_truthy(v))
                    .unwrap_or(true);
                if keep_id {
                    if let Some(id) = doc.get("_id") {
                        only_id.insert("_id".to_string(), id.clone());
                    }
                }
                only_id
            } else {
                apply_projection_with(doc, &flags, query, vars)?
            };
            with_computed(&mut result, doc, &computed, vars)?;
            return Ok(result);
        }
        // A sub-document is a sub-projection, one leaf per field: `{a: {b: 1}}`
        // is `{"a.b": 1}` (mongod 8.2.11, 2026-10-23). It used to be refused.
        if flattened.len() != spec.len()
            || flattened.iter().zip(spec.keys()).any(|((k, _), s)| k != s)
        {
            let flat: Document = flattened.into_iter().collect();
            return apply_projection_with(doc, &flat, query, vars);
        }
    }

    // Separate $slice specs (neutral modifiers) and positional (`arr.$`) keys from
    // the inclusion/exclusion set.
    let mut slice_specs: Vec<(&str, &Bson)> = Vec::new();
    let mut positional: Vec<(&str, &Bson)> = Vec::new();
    let mut spec_main: Vec<(&str, &Bson)> = Vec::new();
    for (k, v) in spec {
        if let Some(arg) = slice_spec(v) {
            slice_specs.push((k, arg));
        } else if k.ends_with(".$") {
            positional.push((k, v));
        } else {
            spec_main.push((k, v));
        }
    }

    if !positional.is_empty() {
        // More than one, or an exclusion, is refused while parsing
        // (`validate_projection`).
        if positional.len() > 1 {
            return Err(Fallback::Defer);
        }
        let (pos_key, pos_val) = positional[0];
        if !spec_truthy(pos_val)? {
            return Err(Fallback::Defer);
        }
        let array_path = &pos_key[..pos_key.len() - 2]; // strip ".$"
        let key = match query {
            Some(q) => crate::elem_match_key::elem_match_key(doc, q, vars)?,
            None => None,
        };
        return apply_positional(doc, &spec_main, &slice_specs, array_path, key)
            .map(|result| in_document_order(result, doc));
    }

    let id_spec: Option<&Bson> = spec_main.iter().find(|(k, _)| *k == "_id").map(|(_, v)| *v);

    // A non-numeric `_id` spec value is a LITERAL, not a flag: mongod 8.2.11
    // answers `{_id: null}` with `{_id: null}` and `{_id: ""}` with `{_id: ""}`,
    // replacing the stored value. Both engines used to return the STORED `_id`
    // here, and the parity suite pinned them to each other while they agreed on
    // the wrong answer. Computed projections live only in the Python engine, so
    // hand any such spec back rather than reproducing them.
    if let Some(v) = id_spec {
        if !matches!(
            v,
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Boolean(_)
        ) {
            return Err(Fallback::Defer);
        }
    }
    let non_id: Vec<(&str, &Bson)> = spec_main
        .iter()
        .copied()
        .filter(|(k, _)| *k != "_id")
        .collect();

    if non_id.is_empty() {
        // The spec is at most an `_id` entry plus `$slice` modifiers.
        // mongod's rules (mirrored in projection.py):
        //   * non-zero NUMBER / bool `_id` => INCLUSION: only `_id` plus any
        //     $slice'd fields survive. (`null` and `""` are literals, not
        //     flags -- they defer above. The comment that stood here said
        //     they were includes; measured 8.2.11, they are not.)
        //   * numeric zero / false => whole doc minus `_id`;
        //   * no `_id` key => whole doc ($slice applied in place).
        if let Some(id_v) = id_spec {
            if !is_drop_id(id_v) {
                let mut result = Document::new();
                if let Some(idv) = doc.get("_id") {
                    result.insert("_id", idv.clone());
                }
                for (path, slice_arg) in &slice_specs {
                    if let Some(current) = paths::get_path(doc, path).cloned() {
                        let sliced = apply_slice(current, slice_arg)?;
                        set(&mut result, path, sliced)?;
                    }
                }
                return Ok(result);
            }
        }
        let mut result = doc.clone();
        if id_spec.is_some() {
            result.remove("_id");
        }
        apply_slices(&mut result, &slice_specs)?;
        return Ok(result);
    }

    // Inclusion vs exclusion: elemMatch counts as truthy (inclusion).
    let mut truthy: Vec<bool> = Vec::new();
    for (_, v) in &non_id {
        truthy.push(if elem_match_spec(v).is_some() {
            true
        } else {
            spec_truthy(v)?
        });
    }
    let inclusion = if truthy.iter().all(|&t| t) {
        true
    } else if !truthy.iter().any(|&t| t) {
        false
    } else {
        return Err(Fallback::Defer); // mixed inclusion/exclusion -> Python raises
    };

    if inclusion {
        let mut result = Document::new();
        let include_id = match id_spec {
            None => true,
            Some(v) => spec_truthy(v)?,
        };
        if include_id {
            if let Some(id) = doc.get("_id") {
                result.insert("_id".to_string(), id.clone());
            }
        }
        // Plain inclusion paths go through a path trie so dotted
        // segments fan over array elements (mirrors projection.py's
        // _include_doc; oracle-pinned semantics).
        let plain: Vec<&str> = non_id
            .iter()
            .filter(|(_, v)| elem_match_spec(v).is_none())
            .map(|(k, _)| *k)
            .collect();
        if !plain.is_empty() {
            let tree = spec_tree(&plain);
            for (k, v) in include_doc(doc, &tree) {
                result.insert(k, v);
            }
        }
        for (path, value) in &non_id {
            if let Some(sub) = elem_match_spec(value) {
                let Bson::Document(subf) = sub else {
                    return Err(Fallback::Defer);
                };
                if let Some(first) = first_match(doc, path, subf)? {
                    set(&mut result, path, Bson::Array(vec![first]))?;
                }
            }
        }
        // $slice implicitly includes its path in inclusion mode.
        for (path, slice_arg) in &slice_specs {
            if !paths::has_path(&result, path) {
                if let Some(extracted) = paths::get_path(doc, path) {
                    set(&mut result, path, extracted.clone())?;
                }
            }
            if let Some(current) = paths::get_path(&result, path).cloned() {
                let sliced = apply_slice(current, slice_arg)?;
                set(&mut result, path, sliced)?;
            }
        }
        return Ok(in_document_order(result, doc));
    }

    // Exclusion mode: trie-walk so dotted unsets map over array
    // elements (non-document elements survive untouched).
    let mut result = doc.clone();
    let all_paths: Vec<&str> = non_id.iter().map(|(k, _)| *k).collect();
    exclude_doc(&mut result, &spec_tree(&all_paths));
    if id_spec.map(is_drop_id).unwrap_or(false) {
        result.remove("_id");
    }
    apply_slices(&mut result, &slice_specs)?;
    Ok(result)
}

/// Dotted paths -> nested trie; a leaf is an empty subtree.
#[derive(Default)]
struct SpecTree(std::collections::BTreeMap<String, SpecTree>);

fn spec_tree(paths: &[&str]) -> SpecTree {
    let mut root = SpecTree::default();
    for p in paths {
        let mut node = &mut root;
        for seg in p.split('.') {
            node = node.0.entry(seg.to_string()).or_default();
        }
    }
    root
}

/// Inclusion projection of `doc` against a path trie (mirrors
/// projection.py's `_include_doc`): a leaf copies the whole value; an
/// interior segment recurses into documents (keeping the `{}` skeleton
/// when the leaf is absent), maps over array elements (documents
/// project — possibly to `{}` — and scalar elements drop), and drops
/// the field entirely on a scalar. Numeric segments are field names.
fn include_doc(doc: &Document, tree: &SpecTree) -> Document {
    // Iterate the DOCUMENT, not the trie. mongod emits projected fields in the
    // stored document's order; `SpecTree` is a `BTreeMap`, so walking it emitted
    // ALPHABETICAL order. That is not the spec's order either, which is why it
    // went unnoticed for so long: it coincides with document order for any
    // corpus whose documents happen to be keyed alphabetically, which the fuzz
    // here was. Over `{_id, z, a, m}` mongod answers `_id, z, a, m` and this
    // answered `_id, a, m, z` (probed 8.2.11, 2026-09-04).
    //
    // The pure engine was wrong here too, differently -- it emitted SPEC order.
    // Two engines, two wrong answers, and a parity suite comparing them with
    // `==`, which ignores key order entirely. `exclude_doc` below needs no such
    // change: it removes keys from the document in place and so keeps its order.
    let mut out = Document::new();
    for (key, val) in doc {
        let Some(subtree) = tree.0.get(key) else {
            continue;
        };
        if subtree.0.is_empty() {
            out.insert(key.clone(), val.clone());
            continue;
        }
        if let Some(projected) = include_value(val, subtree) {
            out.insert(key.clone(), projected);
        }
    }
    out
}

fn include_value(val: &Bson, subtree: &SpecTree) -> Option<Bson> {
    match val {
        Bson::Document(d) => Some(Bson::Document(include_doc(d, subtree))),
        Bson::Array(arr) => Some(Bson::Array(
            arr.iter()
                .filter(|e| matches!(e, Bson::Document(_) | Bson::Array(_)))
                .filter_map(|e| include_value(e, subtree))
                .collect(),
        )),
        _ => None,
    }
}

/// Exclusion projection: unset trie leaves, recursing through
/// documents and mapping over array elements (mirrors `_exclude_doc`).
fn exclude_doc(doc: &mut Document, tree: &SpecTree) {
    for (key, subtree) in &tree.0 {
        if subtree.0.is_empty() {
            doc.remove(key);
        } else if let Some(val) = doc.get_mut(key) {
            exclude_value(val, subtree);
        }
    }
}

fn exclude_value(val: &mut Bson, subtree: &SpecTree) {
    match val {
        Bson::Document(d) => exclude_doc(d, subtree),
        Bson::Array(arr) => {
            for elem in arr.iter_mut() {
                exclude_value(elem, subtree);
            }
        }
        _ => {}
    }
}

/// Reorder a projected document's top-level keys the way mongod emits them.
///
/// `_id` first, then the SOURCE DOCUMENT's order -- not the projection spec's
/// -- with any key the document does not have (a computed field) appended
/// after, in spec order. Measured 8.2.11 (2026-09-09) against a document whose
/// own key order is `_id, b, a, c`: `{a: 1, b: 1}`, `{b: 1, a: 1}`,
/// `{a: {$slice: 2}, b: 1}` and `{a: {$elemMatch: ...}, b: 1}` all emit
/// `_id, b, a`, while `{z: {$literal: 1}, b: 1}` emits `_id, b, z`.
///
/// `$slice` and `$elemMatch` were applied after the plain inclusions and so
/// landed at the end. Key order is what a driver renders, and comparing
/// documents for equality ignores it, so nothing else in the suite could see
/// this. Mirrors `projection._in_document_order`.
fn in_document_order(result: Document, doc: &Document) -> Document {
    let order: std::collections::HashMap<&str, usize> = doc
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let mut ordered = Document::new();
    let mut rest: Vec<(String, Bson)> = Vec::new();
    for (k, v) in result {
        if k == "_id" {
            ordered.insert(k, v);
        } else {
            rest.push((k, v));
        }
    }
    // Stable: keys absent from the document keep their spec order at the end.
    rest.sort_by_key(|(k, _)| *order.get(k.as_str()).unwrap_or(&usize::MAX));
    for (k, v) in rest {
        ordered.insert(k, v);
    }
    ordered
}

fn apply_slices(result: &mut Document, slice_specs: &[(&str, &Bson)]) -> R<()> {
    for (path, slice_arg) in slice_specs {
        if let Some(current) = paths::get_path(result, path).cloned() {
            let sliced = apply_slice(current, slice_arg)?;
            set(result, path, sliced)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn proj(d: Document, s: Document) -> Document {
        let owned = apply_projection(&d, &s, None).expect("should not fall back");
        // Wherever the raw fast path claims a spec, it must produce the
        // byte-identical document — every inclusion projection test below thus
        // doubles as an apply_projection_raw parity check.
        let bytes = bson::to_vec(&d).unwrap();
        let raw = RawDocument::from_bytes(&bytes).unwrap();
        if let Some(fast) = apply_projection_raw(raw, &s) {
            assert_eq!(
                fast, owned,
                "apply_projection_raw != apply_projection for doc={d:?} spec={s:?}"
            );
        }
        owned
    }

    fn raw_of(d: &Document) -> Vec<u8> {
        bson::to_vec(d).unwrap()
    }

    #[test]
    fn raw_fast_path_activates_only_for_pure_inclusion() {
        let d = doc! {"_id": 1, "a": 10, "b": 20, "c": 30};
        let b = raw_of(&d);
        let raw = RawDocument::from_bytes(&b).unwrap();
        // Pure top-level inclusion -> fast path, byte-identical to owned.
        for spec in [
            doc! {"a": 1},
            doc! {"b": 1, "a": 1},
            doc! {"a": 1, "_id": 0},
        ] {
            let fast = apply_projection_raw(raw, &spec).expect("inclusion should fast-path");
            let full = apply_projection(&d, &spec, None).unwrap();
            // `Document`'s `==` ignores key order, so compare the keys too.
            assert_eq!(fast, full);
            assert!(fast.keys().eq(full.keys()), "{fast:?} vs {full:?}");
        }
        // Everything else must defer (None) so the owned path runs.
        for spec in [
            doc! {"a": 0},             // exclusion
            doc! {"a": 1, "b": 0},     // mixed
            doc! {"a.x": 1},           // dotted
            doc! {"a": {"$slice": 1}}, // operator
            doc! {"_id": 1},           // _id-only
            doc! {},                   // empty
        ] {
            assert!(
                apply_projection_raw(raw, &spec).is_none(),
                "spec should defer: {spec:?}"
            );
        }
    }

    /// mongod emits an inclusion projection's fields in the stored
    /// document's order, whatever the spec's (probed 8.2.11, 2026-10-15).
    /// The raw fast path sorted them by name.
    #[test]
    fn raw_fast_path_keeps_the_documents_order() {
        let d = doc! {"_id": 1, "op": "n", "ns": "", "o": {"x": 1}, "b": 2, "a": 1};
        let b = raw_of(&d);
        let raw = RawDocument::from_bytes(&b).unwrap();
        for (spec, want) in [
            (
                doc! {"_id": 0, "op": 1, "ns": 1, "o": 1},
                vec!["op", "ns", "o"],
            ),
            (doc! {"_id": 0, "a": 1, "b": 1}, vec!["b", "a"]),
            (doc! {"a": 1, "op": 1}, vec!["_id", "op", "a"]),
        ] {
            let fast = apply_projection_raw(raw, &spec).expect("fast path");
            assert_eq!(fast.keys().collect::<Vec<_>>(), want);
            let full = apply_projection(&d, &spec, None).unwrap();
            assert_eq!(full.keys().collect::<Vec<_>>(), want);
        }
    }

    #[test]
    fn inclusion() {
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": 1, "b": 2, "c": 3},
                doc! {"a": 1, "c": 1}
            ),
            doc! {"_id": 1, "a": 1, "c": 3}
        );
    }

    #[test]
    fn inclusion_without_id() {
        assert_eq!(
            proj(doc! {"_id": 1, "a": 1, "b": 2}, doc! {"a": 1, "_id": 0}),
            doc! {"a": 1}
        );
    }

    #[test]
    fn exclusion() {
        assert_eq!(
            proj(doc! {"_id": 1, "a": 1, "b": 2}, doc! {"b": 0}),
            doc! {"_id": 1, "a": 1}
        );
    }

    #[test]
    fn dotted_inclusion() {
        assert_eq!(
            proj(doc! {"_id": 1, "a": {"b": 2, "c": 3}}, doc! {"a.b": 1}),
            doc! {"_id": 1, "a": {"b": 2}}
        );
    }

    #[test]
    fn slice_and_mixed_defer() {
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": [1, 2, 3, 4]},
                doc! {"a": {"$slice": 2}}
            ),
            doc! {"_id": 1, "a": [1, 2]}
        );
        // mixed inclusion + exclusion -> defer
        assert!(apply_projection(&doc! {"a": 1, "b": 2}, &doc! {"a": 1, "b": 0}, None).is_err());
    }

    #[test]
    fn meta_only_field_omitted() {
        // The $meta field is omitted (not computed), but the DOCUMENT is
        // untouched: `$meta` is a value re-shaper, not an inclusion. This used
        // to assert `{"_id": 1}`, pinning the bug where asking for a metadata
        // field discarded the caller's document. mongod-probed 6.0.16.
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": 1, "b": 2},
                doc! {"m": {"$meta": "indexKey"}}
            ),
            doc! {"_id": 1, "a": 1, "b": 2}
        );
    }

    #[test]
    fn meta_alongside_inclusion() {
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": 1, "b": 2},
                doc! {"a": 1, "score": {"$meta": "recordId"}}
            ),
            doc! {"_id": 1, "a": 1}
        );
    }

    #[test]
    fn meta_excludes_id() {
        // `_id: 0` drops the identifier; the rest of the document stays.
        // Previously asserted `{}` — see `meta_only_field_omitted`.
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": 1},
                doc! {"_id": 0, "score": {"$meta": "sortKey"}}
            ),
            doc! {"a": 1}
        );
    }

    #[test]
    fn meta_spec_recognizes_and_keywords() {
        assert_eq!(
            meta_spec(&bson::bson!({"$meta": "textScore"})),
            Some("textScore")
        );
        assert_eq!(meta_spec(&bson::bson!({"$meta": 5})), None);
        assert_eq!(meta_spec(&bson::bson!({"a": 1})), None);
        assert!(META_KEYWORDS.contains(&"textScore"));
        assert!(!META_KEYWORDS.contains(&"bogus"));
    }
}

#[cfg(test)]
mod computed_projection_tests {
    use super::apply_projection;
    use bson::{doc, Bson};

    fn src() -> bson::Document {
        doc! {"_id": 1, "a": 2, "b": 3, "s": "hi", "n": {"p": 1, "q": 2}, "arr": [1, 2, 3]}
    }

    /// Every expectation here was MEASURED against mongod 8.2.11 on 2026-09-07,
    /// not derived from the pure engine -- parity with Python would be equally
    /// satisfied by both being wrong.
    #[test]
    fn matches_mongod() {
        let cases: Vec<(bson::Document, bson::Document)> = vec![
            (doc! {"x": {"$literal": 5}}, doc! {"_id": 1, "x": 5}),
            (doc! {"x": {"$add": ["$a", "$b"]}}, doc! {"_id": 1, "x": 5}),
            (
                doc! {"x": {"$concat": ["$s", "!"]}},
                doc! {"_id": 1, "x": "hi!"},
            ),
            (doc! {"x": "$a"}, doc! {"_id": 1, "x": 2}),
            (doc! {"x": "$n.p"}, doc! {"_id": 1, "x": 1}),
            (
                doc! {"x": {"$add": ["$a", 1]}, "b": 1},
                doc! {"_id": 1, "b": 3, "x": 3},
            ),
            (doc! {"_id": 0, "x": {"$literal": 7}}, doc! {"x": 7}),
            (
                doc! {"o.x": {"$literal": 9}},
                doc! {"_id": 1, "o": {"x": 9}},
            ),
            (doc! {"_id": {"$literal": 99}}, doc! {"_id": 99}),
            // A bare reference to a missing field OMITS the key entirely...
            (doc! {"x": "$nope"}, doc! {"_id": 1}),
            // ...while an EXPRESSION over a missing field yields null.
            (
                doc! {"x": {"$add": ["$nope", 1]}},
                doc! {"_id": 1, "x": Bson::Null},
            ),
            // A sub-document is classified PER LEAF.
            (
                doc! {"o": {"x": {"$literal": 3}}},
                doc! {"_id": 1, "o": {"x": 3}},
            ),
            (doc! {"x": {"$size": "$arr"}}, doc! {"_id": 1, "x": 3}),
        ];
        for (spec, want) in cases {
            let got = apply_projection(&src(), &spec, None)
                .unwrap_or_else(|e| panic!("spec {spec:?} failed: {e:?}"));
            assert_eq!(got, want, "spec {spec:?}");
            // Field ORDER is behaviour, and `==` on a Document ignores it.
            assert_eq!(
                got.keys().collect::<Vec<_>>(),
                want.keys().collect::<Vec<_>>(),
                "field order for spec {spec:?}"
            );
        }
    }

    /// `{x: false}` is a FLAG, not a literal -- it excludes a field that is not
    /// there, so the whole document survives. mongod 8.2.11.
    #[test]
    fn false_is_a_flag_not_a_literal() {
        let got = apply_projection(&src(), &doc! {"x": false}, None).unwrap();
        assert_eq!(got, src());
    }

    /// A computed field forces inclusion mode, so a companion exclusion is the
    /// mix mongod rejects with Location31254.
    #[test]
    fn computed_plus_exclusion_is_31254() {
        let err = apply_projection(&src(), &doc! {"x": {"$add": ["$a", 1]}, "b": 0}, None)
            .expect_err("expected the mix to be rejected");
        let (code, msg) = err.as_mongo().expect("should carry mongod's code");
        assert_eq!(code, 31254);
        assert_eq!(
            msg,
            "Cannot do exclusion on field b in inclusion projection"
        );
    }

    /// An empty sub-projection is its own error at any depth.
    #[test]
    fn empty_sub_projection_is_51270() {
        let err = apply_projection(&src(), &doc! {"o": {}}, None).expect_err("expected an error");
        let (code, msg) = err.as_mongo().expect("should carry mongod's code");
        assert_eq!(code, 51270);
        assert_eq!(msg, "Invalid empty sub-projection: o");
    }
}
