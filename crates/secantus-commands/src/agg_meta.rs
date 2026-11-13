//! `{$meta: "recordId"}` and `{$meta: "indexKey"}` in an `aggregate`.
//!
//! The pipeline engine keeps no metadata with a document, so `$meta`
//! evaluates to MISSING there. mongod has the two where its query executor
//! hands the document over (measured 8.2.11, 2026-10-16):
//!
//! * the record id only in a `$project` the query layer takes -- the first
//!   stage after nothing but `$match`es. `$addFields` / `$set` add nothing,
//!   and neither does a `$project` behind a `$sort` or another stage;
//! * the index key, when the read is one index scan, in the stages that
//!   pass the document on with its metadata (`$project`, `$addFields`,
//!   `$set`, `$match`, `$sort`, ...), however many of them come first.
//!
//! Here each such `$meta` is rewritten into a read of a field the documents
//! are given before the pipeline runs, and the fields are taken off again
//! after the stages that read them.

use bson::{Bson, Document};

/// The field a document's record id is handed to the pipeline under.
pub(crate) const RECORD_ID: &str = "__secantus_meta_recordId";
/// The field a document's index key is handed to the pipeline under.
pub(crate) const INDEX_KEY: &str = "__secantus_meta_indexKey";
/// The field a `$sort` (or `$geoNear`) leaves its sort key in.
pub(crate) const SORT_KEY: &str = secantus_core::SORT_KEY_FIELD;
/// The field that marks a document with no sort key at all (RM-522).
pub(crate) const NO_SORT_KEY: &str = secantus_core::NO_SORT_KEY_FIELD;
/// The fields a `$geoNear` leaves its distance and point in.
pub(crate) const GEO_DISTANCE: &str = "__secantus_meta_geoNearDistance";
pub(crate) const GEO_POINT: &str = "__secantus_meta_geoNearPoint";

/// The stages a document's sort key and `$geoNear` metadata pass through
/// (measured 8.2.11: `$match`, `$limit`, `$skip`, `$addFields`, `$project`,
/// `$unwind` and a later `$sort` keep them; a `$group` reads them and drops
/// them; a second `$_internalSetWindowFields` ranks by the key the first
/// passed on, 2026-11-10, RM-410). A `$replaceRoot` / `$replaceWith`, a
/// `$lookup` (in either engine) and a `$setWindowFields` keep them too, a
/// `$setWindowFields` that sorts with the key of its own `$sort`
/// (2026-11-13, RM-520).
const KEEPS_SORT_METADATA: &[&str] = &[
    "$project",
    "$addFields",
    "$set",
    "$match",
    "$unset",
    "$sort",
    "$limit",
    "$skip",
    "$unwind",
    "$_internalSetWindowFields",
    "$replaceRoot",
    "$replaceWith",
    "$lookup",
    "$setWindowFields",
    "$_secantusCarry",
    // Where the stages move between mongod's engines: no stage of its own.
    secantus_core::sbe::ENGINE_STAGE,
    // Passes each document on as it is.
    secantus_core::aggregate::INHIBIT_OPTIMIZATION,
];

/// The `$_secantusSortKey` that writes the key of the `$sort` mongod runs a
/// `$setWindowFields` behind, on its partition and then its `sortBy` (a
/// computed partition is a field mongod sets for it first, and a constant one
/// is none); `None` when the stage sorts on neither and passes on the key it
/// is handed (measured 8.2.11, 2026-11-13, RM-520).
fn window_sort_key(spec: &Bson) -> Option<Bson> {
    let spec = spec.as_document()?;
    let sort_by = match spec.get("sortBy") {
        None => None,
        // A `sortBy` the stage refuses is left for it to refuse.
        Some(by) => {
            secantus_core::aggregate::sort_fields(by).ok()?;
            Some(by.as_document()?)
        }
    };
    // A constant partition is no partition: mongod sorts on none.
    let partition = spec
        .get("partitionBy")
        .filter(|by| !secantus_core::aggregate::is_constant_expression(by, &[]));
    if sort_by.is_none() && partition.is_none() {
        return None;
    }
    let mut by = Document::new();
    let mut computed = None;
    match partition {
        Some(Bson::String(p)) if p.starts_with('$') && !p.starts_with("$$") => {
            by.insert(&p[1..], 1);
        }
        Some(expr) => computed = Some(expr.clone()),
        None => {}
    }
    for (k, v) in sort_by.into_iter().flatten() {
        by.insert(k.clone(), v.clone());
    }
    let mut stage = bson::doc! { "into": SORT_KEY };
    if !by.is_empty() {
        stage.insert("by", by);
    }
    if let Some(expr) = computed {
        stage.insert("partitionBy", expr);
    }
    Some(Bson::Document(bson::doc! { "$_secantusSortKey": stage }))
}

/// The stages whose documents carry no sort key at all, not even the empty
/// one: each makes new documents, and a `$sort` behind it gives them none
/// either (measured 8.2.11, 2026-11-13, RM-522: `$_internalSetWindowFields`
/// refuses a `$rank`, `$denseRank` or `$documentNumber` behind each, and
/// `{$meta: "sortKey"}` is missing).
const DROPS_SORT_KEY: &[&str] = &[
    "$group",
    "$bucket",
    "$bucketAuto",
    "$sortByCount",
    "$count",
    "$facet",
    "$redact",
];

/// The stages that keep a document's metadata as they pass it on.
const KEEPS_METADATA: &[&str] = &[
    "$project",
    "$addFields",
    "$set",
    "$match",
    "$unset",
    "$sort",
    "$limit",
    "$skip",
    secantus_core::aggregate::INHIBIT_OPTIMIZATION,
];

/// What [`rewrite`] did.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Rewritten {
    /// The documents need [`RECORD_ID`].
    pub record_id: bool,
    /// The documents need [`INDEX_KEY`].
    pub index_key: bool,
}

fn name(stage: &Bson) -> &str {
    stage
        .as_document()
        .and_then(|d| d.keys().next())
        .map_or("", String::as_str)
}

/// Whether `v` reads `field` (a `"$field"` [`replace`] left there).
pub(crate) fn reads(v: &Bson, field: &str) -> bool {
    match v {
        Bson::String(s) => s.strip_prefix('$') == Some(field),
        Bson::Document(d) => d.values().any(|x| reads(x, field)),
        Bson::Array(items) => items.iter().any(|x| reads(x, field)),
        _ => false,
    }
}

/// Replace each `{$meta: kind}` in `v` with a read of `field`.
pub(crate) fn replace(v: &mut Bson, kind: &str, field: &str) -> bool {
    match v {
        Bson::Document(d) => {
            if d.len() == 1 {
                if let Some(Bson::String(k)) = d.get("$meta") {
                    if k == kind {
                        *v = Bson::String(format!("${field}"));
                        return true;
                    }
                    return false;
                }
                if d.contains_key("$literal") || d.contains_key("$const") {
                    return false;
                }
            }
            let mut any = false;
            for (_, x) in d.iter_mut() {
                any |= replace(x, kind, field);
            }
            any
        }
        Bson::Array(items) => {
            let mut any = false;
            for x in items.iter_mut() {
                any |= replace(x, kind, field);
            }
            any
        }
        _ => false,
    }
}

/// Rewrite the `$meta`s mongod answers in `remaining`, the stages left
/// after the read of `pipeline` (the whole pipeline, a view's in front).
pub(crate) fn rewrite(pipeline: &[Bson], remaining: &mut Vec<Bson>) -> Rewritten {
    let mut done = Rewritten::default();
    // The record id: a `$project` behind nothing but `$match`es.
    let first_other = pipeline.iter().find(|s| name(s) != "$match");
    if let (Some(first), Some(stage)) = (first_other, remaining.first_mut()) {
        if name(first) == "$project" && first == &*stage {
            done.record_id = replace(stage, "recordId", RECORD_ID);
        }
    }
    // The index key, through the run of stages that keep it.
    let run = remaining
        .iter()
        .take_while(|s| KEEPS_METADATA.contains(&name(s)))
        .count();
    for stage in remaining.iter_mut().take(run) {
        done.index_key |= replace(stage, "indexKey", INDEX_KEY);
    }
    if done.record_id || done.index_key {
        let mut gone: Vec<Bson> = Vec::new();
        if done.record_id {
            gone.push(Bson::String(RECORD_ID.to_string()));
        }
        if done.index_key {
            gone.push(Bson::String(INDEX_KEY.to_string()));
        }
        let at = run.max(1).min(remaining.len());
        remaining.insert(at, Bson::Document(bson::doc! { "$unset": gone }));
    }
    done
}

/// Whether `v` holds a `{$meta: kind}`.
pub(crate) fn mentions(v: &Bson, kind: &str) -> bool {
    match v {
        Bson::Document(d) => {
            if d.len() == 1 {
                if let Some(Bson::String(k)) = d.get("$meta") {
                    return k == kind;
                }
                if d.contains_key("$literal") || d.contains_key("$const") {
                    return false;
                }
            }
            d.values().any(|x| mentions(x, kind))
        }
        Bson::Array(items) => items.iter().any(|x| mentions(x, kind)),
        _ => false,
    }
}

/// A `{$meta: "sortKey"}` read of [`SORT_KEY`]. The field holds the key as
/// a `$_internalSetWindowFields` compares it, an empty array's part
/// `undefined`, which this engine's expressions take for MISSING; `$meta`
/// gives that part as null, as it was before RM-410 kept the key whole.
fn sort_key_read() -> Bson {
    Bson::Document(bson::doc! {
        "$map": { "input": format!("${SORT_KEY}"), "in": { "$ifNull": ["$$this", null] } }
    })
}

/// Whether `stage` reads a document's sort-key metadata: a `{$meta:
/// "sortKey"}`, or a `$_internalSetWindowFields` whose `$rank` or
/// `$denseRank` compares the key a `$sort` in front attached (mongod 8.2.1
/// and 8.2.11, RM-410; `$setWindowFields` ranks behind its own `$sort`).
pub(crate) fn reads_sort_key(stage: &Bson) -> bool {
    if mentions(stage, "sortKey") {
        return true;
    }
    let Some(Bson::Document(spec)) = stage
        .as_document()
        .and_then(|d| d.get("$_internalSetWindowFields"))
    else {
        return false;
    };
    spec.get_document("output").is_ok_and(|output| {
        output
            .values()
            .filter_map(Bson::as_document)
            .any(|f| f.contains_key("$rank") || f.contains_key("$denseRank"))
    })
}

/// Whether `stage` is a `$_internalSetWindowFields` whose output reads each
/// document's sort key to number it: `$rank`, `$denseRank`, and
/// `$documentNumber`, which mongod refuses as they do with no key at all
/// (8.2.11, RM-522), though it ranks nothing by it.
fn ranks_by_sort_key(stage: &Bson) -> bool {
    let Some(Bson::Document(spec)) = stage
        .as_document()
        .and_then(|d| d.get("$_internalSetWindowFields"))
    else {
        return false;
    };
    spec.get_document("output").is_ok_and(|output| {
        output.values().filter_map(Bson::as_document).any(|f| {
            f.contains_key("$rank")
                || f.contains_key("$denseRank")
                || f.contains_key("$documentNumber")
        })
    })
}

/// Whether a stage in `stages` ranks documents that may carry no sort key:
/// it comes behind a stage in [`DROPS_SORT_KEY`] or a `$unionWith`.
pub(crate) fn ranks_with_no_sort_key(stages: &[Bson]) -> bool {
    stages.iter().enumerate().any(|(i, s)| {
        ranks_by_sort_key(s)
            && stages[..i]
                .iter()
                .any(|b| DROPS_SORT_KEY.contains(&name(b)) || name(b) == "$unionWith")
    })
}

/// Whether the rewrite has anything to do in `stages`.
pub(crate) fn needs_sort_metadata(stages: &[Bson]) -> bool {
    stages.iter().any(|s| {
        reads_sort_key(s)
            || ["sortKey", "geoNearDistance", "geoNearPoint"]
                .iter()
                .any(|k| mentions(s, k))
    }) || ranks_with_no_sort_key(stages)
}

/// Whether the documents `$unionWith` at `stages[at]` adds reach a rank
/// with no sort key. mongod's sub-pipeline gives them the empty key only
/// when a `{$meta: "sortKey"}` reads them before the rank does, and never
/// when it ends behind a stage in [`DROPS_SORT_KEY`] (8.2.11, 2026-11-13,
/// RM-522: `$unionWith`, then a rank, is refused; with an `$addFields` of
/// the key in between, it ties).
fn union_adds_no_sort_key(stages: &[Bson], at: usize) -> bool {
    let rest = &stages[at + 1..];
    let Some(rank) = rest.iter().position(ranks_by_sort_key) else {
        return false;
    };
    let between = &rest[..rank];
    if between.iter().any(|s| DROPS_SORT_KEY.contains(&name(s))) {
        // New documents: those are refused whatever this stage added.
        return false;
    }
    let sub_drops = stages[at]
        .as_document()
        .and_then(|d| d.get_document("$unionWith").ok())
        .and_then(|spec| spec.get_array("pipeline").ok())
        .is_some_and(|sub| sub.iter().any(|s| DROPS_SORT_KEY.contains(&name(s))));
    sub_drops || !between.iter().any(|s| mentions(s, "sortKey"))
}

/// Whether no stage writes a sort key because mongod's dependency analysis
/// never asked for one. It walks the stages front to back and stops at the
/// first `$_internalInhibitOptimization`, which says nothing of what it
/// needs; a sort key read only behind that stage is not asked for, so the
/// read and every `$sort` leave the documents with none at all (mongod
/// 8.2.11, RM-518: `{$meta: "sortKey"}` there is missing and a rank is
/// refused). A reader ahead of it asks, a `$lookup` or `$unionWith`
/// sub-pipeline's for its own documents only, and a `$geoNear` sets the key
/// whatever is asked.
fn sort_key_unrequested(stages: &[Bson]) -> bool {
    let Some(at) = stages
        .iter()
        .position(|s| name(s) == secantus_core::aggregate::INHIBIT_OPTIMIZATION)
    else {
        return false;
    };
    !stages[..at].iter().any(|s| {
        name(s) == "$geoNear" || (!matches!(name(s), "$lookup" | "$unionWith") && reads_sort_key(s))
    })
}

/// Mark the documents `$unionWith` adds as carrying no sort key: a stage
/// at the end of its sub-pipeline, which the stage gets when it has none.
fn mark_union_documents(stage: &mut Bson) {
    let mark = Bson::Document(bson::doc! { "$set": { NO_SORT_KEY: true } });
    let Some(outer) = stage.as_document_mut() else {
        return;
    };
    match outer.get_mut("$unionWith") {
        Some(Bson::String(coll)) => {
            let coll = coll.clone();
            outer.insert(
                "$unionWith",
                bson::doc! { "coll": coll, "pipeline": [mark] },
            );
        }
        Some(Bson::Document(spec)) => match spec.get_mut("pipeline") {
            Some(Bson::Array(sub)) => sub.push(mark),
            _ => {
                spec.insert("pipeline", vec![mark]);
            }
        },
        _ => {}
    }
}

/// Replace each `{$meta: kind}` in `v` with `with`.
fn replace_with(v: &mut Bson, kind: &str, with: &Bson) {
    match v {
        Bson::Document(d) => {
            if d.len() == 1 {
                if let Some(Bson::String(k)) = d.get("$meta") {
                    if k == kind {
                        *v = with.clone();
                    }
                    return;
                }
                if d.contains_key("$literal") || d.contains_key("$const") {
                    return;
                }
            }
            for (_, x) in d.iter_mut() {
                replace_with(x, kind, with);
            }
        }
        Bson::Array(items) => {
            for x in items.iter_mut() {
                replace_with(x, kind, with);
            }
        }
        _ => {}
    }
}

/// An inclusion `$project` (one that names a field to keep or compute).
fn is_inclusion(spec: &Document) -> bool {
    spec.iter().any(|(k, v)| {
        k != "_id"
            && !matches!(v, Bson::Int32(0) | Bson::Int64(0) | Bson::Boolean(false))
            && !matches!(v, Bson::Double(d) if *d == 0.0)
    })
}

/// `{$meta: "sortKey"}`, `"geoNearDistance"` and `"geoNearPoint"`, which
/// mongod keeps from a `$sort` or a `$geoNear` through the stages in
/// [`KEEPS_SORT_METADATA`] and into a `$group`. Each is rewritten into a read
/// of a field the stage that sets it is made to write, and the fields come
/// off again where the metadata ends. With no sort key in reach, `$addFields`
/// and `$group` give `[]` (mongod 8.2.11) and the other stages are left to
/// refuse it; in `$match` and `$redact` the sort key is always missing (a
/// leading `$match` becomes the query, whose `$expr` sees it missing too).
///
/// A document with no sort key at all -- behind a stage in
/// [`DROPS_SORT_KEY`], or added by a `$unionWith` -- gives `{$meta:
/// "sortKey"}` as missing and has a rank refused (RM-522).
///
/// Only the `$sort` mongod's query layer runs sets the key: `query_sort`
/// gives its pattern, mongod's pipeline front as `explain` models it
/// (`explain::pipeline_canonical`, which moves a `$sort` ahead of an
/// `$unwind` that leaves its fields alone), and the first `$sort` of
/// `stages` with that pattern is the one. A `$sort` the pipeline runs itself
/// sorts and leaves the key it was handed: the query layer's sort's, `[]`, or
/// a `$geoNear`'s distance (mongod 8.2.11, 2026-11-13, RM-523: its
/// `DocumentSourceSort` writes the key only when built to, as the one a
/// `$setWindowFields` runs behind is). `query_sort` is asked only when a
/// stage reads the metadata.
pub(crate) fn rewrite_sort_metadata(
    stages: &mut Vec<Bson>,
    query_sort: impl FnOnce() -> Option<Document>,
) {
    if !needs_sort_metadata(stages) {
        return;
    }
    // The query layer's sort, until the `$sort` that is it has run.
    let mut query_sort = query_sort();
    let mut out: Vec<Bson> = Vec::with_capacity(stages.len() + 4);
    // The hidden fields in place, and so the metadata in reach.
    let (mut sort_live, mut geo_live) = (false, false);
    // No document has a sort key at all (behind a stage in
    // `DROPS_SORT_KEY`), and the ones a `$unionWith` added carry
    // `NO_SORT_KEY` (RM-522).
    let (mut keyless, mut marked) = (false, false);
    let n = stages.len();
    let all = std::mem::take(stages);
    if sort_key_unrequested(&all) {
        keyless = true;
    }
    let unions: Vec<bool> = (0..all.len())
        .map(|i| name(&all[i]) == "$unionWith" && union_adds_no_sort_key(&all, i))
        .collect();
    for (i, stage) in all.into_iter().enumerate() {
        let mut stage = stage;
        let name = name(&stage).to_string();
        let keeps = KEEPS_SORT_METADATA.contains(&name.as_str());
        // With no key at all, `{$meta: "sortKey"}` is missing in every stage
        // (a `$group`'s `_id` null, a `$push` of it nothing), behind a later
        // `$sort` too; and a rank is refused, for which the documents are
        // marked as they reach it (mongod 8.2.11, RM-522).
        if keyless {
            // Not into a `$lookup`'s sub-pipeline, which reads its own
            // documents' key, nor a `$setWindowFields`, which holds it off.
            let own = !matches!(name.as_str(), "$lookup" | "$setWindowFields");
            if (keeps && own) || name == "$group" {
                replace_with(&mut stage, "sortKey", &Bson::String("$$REMOVE".into()));
            }
            if ranks_by_sort_key(&stage) {
                out.push(Bson::Document(bson::doc! { "$set": { NO_SORT_KEY: true } }));
            }
        }
        if unions[i] && !keyless {
            mark_union_documents(&mut stage);
            marked = true;
        }
        if marked && name == "$project" {
            // An inclusion projection would drop the mark.
            if let Some(Bson::Document(spec)) =
                stage.as_document_mut().and_then(|d| d.get_mut("$project"))
            {
                if is_inclusion(spec) {
                    spec.insert(NO_SORT_KEY, 1);
                }
            }
        }
        // `$match` and `$redact` never see a sort key: there it is MISSING,
        // after a `$sort` too (mongod 8.2.11, 2026-10-22; RM-252).
        if matches!(name.as_str(), "$match" | "$redact") {
            replace_with(&mut stage, "sortKey", &Bson::String("$$REMOVE".into()));
        }
        // A `$lookup`'s sub-pipeline reads its own documents' metadata
        // (RM-521); a `$setWindowFields` holds the sort key off its
        // documents while it runs, so a `$meta` in it is left to refuse.
        let reads_here = name != "$lookup";
        let reads_sort_here = name != "$setWindowFields";
        if (keeps && reads_here) || name == "$group" {
            if !reads_sort_here {
                // Left as it is.
            } else if sort_live {
                replace_with(&mut stage, "sortKey", &sort_key_read());
            } else if matches!(name.as_str(), "$addFields" | "$set" | "$group") {
                replace_with(
                    &mut stage,
                    "sortKey",
                    &Bson::Document(bson::doc! { "$literal": [] }),
                );
            }
            if geo_live {
                replace(&mut stage, "geoNearDistance", GEO_DISTANCE);
                replace(&mut stage, "geoNearPoint", GEO_POINT);
            }
        }
        let live: Vec<&str> = [
            (sort_live, SORT_KEY),
            (geo_live, GEO_DISTANCE),
            (geo_live, GEO_POINT),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, f)| *f)
        .collect();
        if !keeps && !live.is_empty() {
            if name != "$group" {
                // The metadata ends here; take its fields off first.
                out.push(Bson::Document(bson::doc! { "$unset": live.clone() }));
            }
            sort_live = false;
            geo_live = false;
        } else if name == "$project" && !live.is_empty() {
            // An inclusion projection would drop the fields a later stage
            // still reads the metadata from.
            if let Some(Bson::Document(spec)) =
                stage.as_document_mut().and_then(|d| d.get_mut("$project"))
            {
                if is_inclusion(spec) {
                    for f in &live {
                        spec.insert(*f, 1);
                    }
                }
            }
        }
        match name.as_str() {
            "$sort" => {
                let by = stage
                    .as_document()
                    .and_then(|d| d.get_document("$sort").ok())
                    .filter(|by| query_sort.as_ref() == Some(*by))
                    .cloned();
                out.push(stage);
                if let Some(by) = by {
                    query_sort = None;
                    out.push(Bson::Document(
                        bson::doc! { "$_secantusSortKey": { "into": SORT_KEY, "by": by } },
                    ));
                    sort_live = true;
                }
                continue;
            }
            "$geoNear" => {
                let mut copy = bson::Document::new();
                if let Some(Bson::Document(spec)) =
                    stage.as_document_mut().and_then(|d| d.get_mut("$geoNear"))
                {
                    let dist = match spec.get("distanceField") {
                        Some(Bson::String(f)) => f.clone(),
                        _ => {
                            spec.insert("distanceField", GEO_DISTANCE);
                            GEO_DISTANCE.to_string()
                        }
                    };
                    let point = match spec.get("includeLocs") {
                        Some(Bson::String(f)) => f.clone(),
                        _ => {
                            spec.insert("includeLocs", GEO_POINT);
                            GEO_POINT.to_string()
                        }
                    };
                    if dist != GEO_DISTANCE {
                        copy.insert(GEO_DISTANCE, format!("${dist}"));
                    }
                    if point != GEO_POINT {
                        copy.insert(GEO_POINT, format!("${point}"));
                    }
                    copy.insert(SORT_KEY, vec![Bson::String(format!("${GEO_DISTANCE}"))]);
                }
                out.push(stage);
                if !copy.is_empty() {
                    // Two steps: the sort key reads the distance just copied.
                    let sort_key = copy.remove(SORT_KEY);
                    if !copy.is_empty() {
                        out.push(Bson::Document(bson::doc! { "$set": copy }));
                    }
                    if let Some(sk) = sort_key {
                        out.push(Bson::Document(bson::doc! { "$set": { SORT_KEY: sk } }));
                    }
                    geo_live = true;
                    sort_live = true;
                }
                continue;
            }
            // A stage that makes a new document from each: the metadata
            // goes with it, as mongod copies it over.
            // The mark a `$unionWith` left goes with it too (RM-522).
            "$replaceRoot" | "$replaceWith" if !live.is_empty() || marked => {
                let mut fields = live.clone();
                if marked {
                    fields.push(NO_SORT_KEY);
                }
                stage = Bson::Document(bson::doc! {
                    "$_secantusCarry": { "stage": stage, "fields": fields }
                });
            }
            "$_secantusCarry" if !live.is_empty() || marked => {
                if let Some(Bson::Document(spec)) = stage
                    .as_document_mut()
                    .and_then(|d| d.get_mut("$_secantusCarry"))
                {
                    let mut fields = spec.get_array("fields").cloned().unwrap_or_default();
                    fields.extend(live.iter().map(|f| Bson::String((*f).to_string())));
                    if marked {
                        fields.push(Bson::String(NO_SORT_KEY.to_string()));
                    }
                    spec.insert("fields", fields);
                }
            }
            "$setWindowFields" => {
                if let Some(key) = stage
                    .as_document()
                    .and_then(|d| d.get("$setWindowFields"))
                    .and_then(window_sort_key)
                {
                    out.push(key);
                    sort_live = true;
                }
            }
            _ => {}
        }
        let ranks = ranks_by_sort_key(&stage);
        out.push(stage);
        if DROPS_SORT_KEY.contains(&name.as_str()) {
            keyless = true;
            marked = false;
        } else if ranks {
            // Every document that passed carried a key: one with none was
            // refused there.
            marked = false;
        }
        if i + 1 == n && (sort_live || geo_live) {
            let live: Vec<&str> = [
                (sort_live, SORT_KEY),
                (geo_live, GEO_DISTANCE),
                (geo_live, GEO_POINT),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, f)| *f)
            .collect();
            out.push(Bson::Document(bson::doc! { "$unset": live }));
        }
    }
    // A pipeline ending on a `$sort` or `$geoNear` leaves its fields set.
    let tail_live: Vec<&str> = [
        (sort_live, SORT_KEY),
        (geo_live, GEO_DISTANCE),
        (geo_live, GEO_POINT),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, f)| *f)
    .collect();
    let ends_unset = out.last().is_some_and(|s| name(s) == "$unset");
    if !tail_live.is_empty() && !ends_unset {
        out.push(Bson::Document(bson::doc! { "$unset": tail_live }));
    }
    *stages = out;
}

/// Give each of `docs` its record id and index key, where known.
pub(crate) fn attach(
    docs: &mut [Document],
    record_ids: Option<&[Option<i64>]>,
    index_keys: Option<&[Document]>,
) {
    for (i, doc) in docs.iter_mut().enumerate() {
        if let Some(Some(rid)) = record_ids.map(|r| r[i]) {
            doc.insert(RECORD_ID, Bson::Int64(rid));
        }
        if let Some(key) = index_keys.map(|k| &k[i]) {
            doc.insert(INDEX_KEY, Bson::Document(key.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn stages(v: Vec<Document>) -> Vec<Bson> {
        v.into_iter().map(Bson::Document).collect()
    }

    #[test]
    fn a_project_behind_matches_reads_the_record_id() {
        let pipeline = stages(vec![
            doc! { "$match": { "a": 1 } },
            doc! { "$project": { "r": { "$ifNull": [{ "$meta": "recordId" }, 0] } } },
        ]);
        let mut remaining = pipeline[1..].to_vec();
        let done = rewrite(&pipeline, &mut remaining);
        assert_eq!(
            done,
            Rewritten {
                record_id: true,
                index_key: false
            }
        );
        assert_eq!(
            remaining,
            stages(vec![
                doc! { "$project": { "r": { "$ifNull": [format!("${RECORD_ID}"), 0] } } },
                doc! { "$unset": [RECORD_ID] },
            ])
        );
    }

    #[test]
    fn add_fields_and_a_later_project_have_no_record_id() {
        for pipeline in [
            stages(vec![
                doc! { "$addFields": { "r": { "$meta": "recordId" } } },
            ]),
            stages(vec![
                doc! { "$sort": { "a": 1 } },
                doc! { "$project": { "r": { "$meta": "recordId" } } },
            ]),
        ] {
            let mut remaining = pipeline.clone();
            assert_eq!(rewrite(&pipeline, &mut remaining), Rewritten::default());
            assert_eq!(remaining, pipeline);
        }
    }

    #[test]
    fn the_index_key_reaches_through_stages_that_keep_it() {
        let pipeline = stages(vec![
            doc! { "$addFields": { "q": 1 } },
            doc! { "$project": { "k": { "$meta": "indexKey" } } },
            doc! { "$group": { "_id": { "$meta": "indexKey" } } },
        ]);
        let mut remaining = pipeline.clone();
        let done = rewrite(&pipeline, &mut remaining);
        assert_eq!(
            done,
            Rewritten {
                record_id: false,
                index_key: true
            }
        );
        assert_eq!(
            remaining[1],
            Bson::Document(doc! { "$project": { "k": format!("${INDEX_KEY}") } })
        );
        assert_eq!(remaining[2], Bson::Document(doc! { "$unset": [INDEX_KEY] }));
        // Past a `$group` the key is gone, as it is on mongod.
        assert_eq!(remaining[3], pipeline[2]);
    }

    #[test]
    fn a_sort_key_is_written_after_the_sort_and_taken_off_at_a_group() {
        let mut stages = stages(vec![
            doc! { "$sort": { "a": 1 } },
            doc! { "$project": { "a": 1 } },
            doc! { "$project": { "k": { "$meta": "sortKey" } } },
            doc! { "$group": { "_id": { "$meta": "sortKey" } } },
            doc! { "$project": { "k": { "$meta": "sortKey" } } },
        ]);
        rewrite_sort_metadata(&mut stages, || Some(doc! { "a": 1 }));
        let read = sort_key_read();
        assert_eq!(
            stages,
            self::stages(vec![
                doc! { "$sort": { "a": 1 } },
                doc! { "$_secantusSortKey": { "into": SORT_KEY, "by": { "a": 1 } } },
                // An inclusion projection carries the key on.
                doc! { "$project": { "a": 1, SORT_KEY: 1 } },
                doc! { "$project": { "k": read.clone(), SORT_KEY: 1 } },
                doc! { "$group": { "_id": read } },
                // Past the `$group` there is no sort key: it is missing
                // (mongod 8.2.11, RM-522).
                doc! { "$project": { "k": "$$REMOVE" } },
            ])
        );
    }

    /// A `$_internalSetWindowFields` that ranks reads the sort key the
    /// `$sort` in front left, through stages that keep it and through
    /// itself to a second window (RM-410); one that does not rank, and a
    /// `$setWindowFields`, which ranks behind its own `$sort`, read none.
    #[test]
    fn an_internal_window_that_ranks_reads_the_sort_key() {
        let rank = doc! { "$_internalSetWindowFields": {
        "sortBy": { "n": 1 }, "output": { "r": { "$rank": {} } } } };
        let number = doc! { "$_internalSetWindowFields": {
        "sortBy": { "n": 1 }, "output": { "x": { "$documentNumber": {} } } } };
        assert!(reads_sort_key(&Bson::Document(rank.clone())));
        assert!(!reads_sort_key(&Bson::Document(number.clone())));
        let public = doc! { "$setWindowFields": {
        "sortBy": { "n": 1 }, "output": { "r": { "$rank": {} } } } };
        assert!(!reads_sort_key(&Bson::Document(public)));

        let mut piped = stages(vec![
            doc! { "$sort": { "k": 1 } },
            doc! { "$addFields": { "z": 1 } },
            number.clone(),
            rank.clone(),
        ]);
        rewrite_sort_metadata(&mut piped, || Some(doc! { "k": 1 }));
        assert_eq!(
            piped,
            stages(vec![
                doc! { "$sort": { "k": 1 } },
                doc! { "$_secantusSortKey": { "into": SORT_KEY, "by": { "k": 1 } } },
                doc! { "$addFields": { "z": 1 } },
                number,
                rank.clone(),
                doc! { "$unset": [SORT_KEY] },
            ])
        );

        // No `$sort` in reach: nothing to read, and every document ties.
        let mut alone = stages(vec![rank.clone()]);
        rewrite_sort_metadata(&mut alone, || None);
        assert_eq!(alone, stages(vec![rank]));
    }

    /// A `$replaceRoot` / `$replaceWith`, a `$lookup` and a
    /// `$setWindowFields` that does not sort pass the key on; a
    /// `$replaceRoot` runs carried so its new documents keep it, and a
    /// `$lookup`'s sub-pipeline is not rewritten (RM-520).
    #[test]
    fn the_sort_key_passes_through_stages_that_keep_it() {
        let meta = doc! { "$addFields": { "m": { "$meta": "sortKey" } } };
        let lookup = doc! { "$lookup": { "from": "c", "pipeline": [
        { "$addFields": { "m": { "$meta": "sortKey" } } }], "as": "j" } };
        let window = doc! { "$setWindowFields": { "output": { "x": { "$sum": 1 } } } };
        let mut piped = stages(vec![
            doc! { "$sort": { "k": 1 } },
            doc! { "$replaceWith": "$$ROOT" },
            lookup.clone(),
            secantus_core_engine(),
            window.clone(),
            meta,
        ]);
        rewrite_sort_metadata(&mut piped, || Some(doc! { "k": 1 }));
        assert_eq!(
            piped,
            stages(vec![
                doc! { "$sort": { "k": 1 } },
                doc! { "$_secantusSortKey": { "into": SORT_KEY, "by": { "k": 1 } } },
                doc! { "$_secantusCarry": {
                "stage": { "$replaceWith": "$$ROOT" }, "fields": [SORT_KEY] } },
                lookup,
                secantus_core_engine(),
                window,
                doc! { "$addFields": { "m": sort_key_read() } },
                doc! { "$unset": [SORT_KEY] },
            ])
        );
    }

    fn secantus_core_engine() -> Document {
        doc! { secantus_core::sbe::ENGINE_STAGE: true }
    }

    /// A `$setWindowFields` on a partition or a `sortBy` sets the key of the
    /// `$sort` mongod runs it behind; a constant partition is none (mongod
    /// 8.2.11, 2026-11-13, RM-520).
    #[test]
    fn a_window_that_sorts_sets_its_own_key() {
        let key = |spec: Document| window_sort_key(&Bson::Document(spec));
        assert_eq!(
            key(doc! { "partitionBy": "$p", "sortBy": { "n": -1 }, "output": {} }),
            Some(Bson::Document(doc! { "$_secantusSortKey": {
            "into": SORT_KEY, "by": { "p": 1, "n": -1 } } }))
        );
        assert_eq!(
            key(doc! { "partitionBy": { "$mod": ["$n", 2] }, "output": {} }),
            Some(Bson::Document(doc! { "$_secantusSortKey": {
            "into": SORT_KEY, "partitionBy": { "$mod": ["$n", 2] } } }))
        );
        assert_eq!(
            key(doc! { "partitionBy": { "$literal": "x" }, "sortBy": { "n": 1 }, "output": {} }),
            Some(Bson::Document(doc! { "$_secantusSortKey": {
            "into": SORT_KEY, "by": { "n": 1 } } }))
        );
        assert_eq!(key(doc! { "partitionBy": "x", "output": {} }), None);
        assert_eq!(key(doc! { "output": {} }), None);
        // A `sortBy` the stage refuses is its to refuse.
        assert_eq!(key(doc! { "sortBy": { "n": 2 }, "output": {} }), None);

        let window = doc! { "$setWindowFields": {
        "sortBy": { "n": 1 }, "output": { "x": { "$push": { "$meta": "sortKey" } } } } };
        let mut piped = stages(vec![
            window.clone(),
            doc! { "$addFields": { "m": { "$meta": "sortKey" } } },
        ]);
        rewrite_sort_metadata(&mut piped, || None);
        assert_eq!(
            piped,
            stages(vec![
                doc! { "$_secantusSortKey": { "into": SORT_KEY, "by": { "n": 1 } } },
                // The key is off the documents while the stage runs: its
                // own `$meta` is left as it was.
                window,
                doc! { "$addFields": { "m": sort_key_read() } },
                doc! { "$unset": [SORT_KEY] },
            ])
        );
    }

    /// Behind a `$group` no document has a sort key, a later `$sort`
    /// gives it none (only the query layer's sets one, RM-523), `{$meta:
    /// "sortKey"}` is missing and a rank is refused: the documents are
    /// marked as they reach it (RM-522).
    #[test]
    fn behind_a_group_there_is_no_sort_key() {
        let rank = doc! { "$_internalSetWindowFields": {
        "sortBy": { "_id": 1 }, "output": { "r": { "$documentNumber": {} } } } };
        let mut piped = stages(vec![
            doc! { "$group": { "_id": "$k", "m": { "$push": { "$meta": "sortKey" } } } },
            doc! { "$sort": { "_id": 1 } },
            doc! { "$project": { "m": { "$meta": "sortKey" } } },
            rank.clone(),
        ]);
        assert!(needs_sort_metadata(&piped));
        rewrite_sort_metadata(&mut piped, || None);
        assert_eq!(
            piped,
            stages(vec![
                // Read from the collection with no `$sort`: the empty key.
                doc! { "$group": { "_id": "$k", "m": { "$push": { "$literal": [] } } } },
                doc! { "$sort": { "_id": 1 } },
                doc! { "$project": { "m": "$$REMOVE" } },
                doc! { "$set": { NO_SORT_KEY: true } },
                rank,
            ])
        );
    }

    /// The documents a `$unionWith` adds have no sort key when a rank
    /// reads them first, and the empty one when a `{$meta: "sortKey"}`
    /// does (RM-522).
    #[test]
    fn a_union_adds_documents_with_no_sort_key() {
        let rank = doc! { "$_internalSetWindowFields": {
        "sortBy": { "_id": 1 }, "output": { "r": { "$rank": {} } } } };
        let mut piped = stages(vec![
            doc! { "$unionWith": "c" },
            doc! { "$project": { "n": 1 } },
            rank.clone(),
        ]);
        rewrite_sort_metadata(&mut piped, || None);
        assert_eq!(
            piped,
            stages(vec![
                doc! { "$unionWith": { "coll": "c",
                "pipeline": [{ "$set": { NO_SORT_KEY: true } }] } },
                doc! { "$project": { "n": 1, NO_SORT_KEY: 1 } },
                rank.clone(),
            ])
        );

        let meta = doc! { "$addFields": { "m": { "$meta": "sortKey" } } };
        let mut read = stages(vec![doc! { "$unionWith": "c" }, meta, rank.clone()]);
        rewrite_sort_metadata(&mut read, || None);
        assert_eq!(
            read,
            stages(vec![
                doc! { "$unionWith": "c" },
                doc! { "$addFields": { "m": { "$literal": [] } } },
                rank,
            ])
        );
    }

    /// Only the `$sort` mongod's query layer runs sets the key: one the
    /// pipeline runs behind another stage sorts and leaves the key it was
    /// handed, the query layer's or none (mongod 8.2.11, RM-523).
    #[test]
    fn only_the_query_layers_sort_sets_the_key() {
        let meta = doc! { "$addFields": { "m": { "$meta": "sortKey" } } };
        let mut piped = stages(vec![
            doc! { "$sort": { "k": 1 } },
            doc! { "$addFields": { "q": 1 } },
            doc! { "$sort": { "n": 1 } },
            meta.clone(),
        ]);
        rewrite_sort_metadata(&mut piped, || Some(doc! { "k": 1 }));
        assert_eq!(
            piped,
            stages(vec![
                doc! { "$sort": { "k": 1 } },
                doc! { "$_secantusSortKey": { "into": SORT_KEY, "by": { "k": 1 } } },
                doc! { "$addFields": { "q": 1 } },
                doc! { "$sort": { "n": 1 } },
                doc! { "$addFields": { "m": sort_key_read() } },
                doc! { "$unset": [SORT_KEY] },
            ])
        );

        // The query layer took a later `$sort` (one an `$unwind` let ahead).
        let mut piped = stages(vec![
            doc! { "$sort": { "k": 1 } },
            doc! { "$unwind": "$a" },
            doc! { "$sort": { "n": 1 } },
            meta.clone(),
        ]);
        rewrite_sort_metadata(&mut piped, || Some(doc! { "n": 1 }));
        assert_eq!(
            piped[3],
            Bson::Document(doc! { "$_secantusSortKey": { "into": SORT_KEY, "by": { "n": 1 } } })
        );

        // No sort in the query layer: the documents keep the empty key.
        let mut piped = stages(vec![
            doc! { "$addFields": { "q": 1 } },
            doc! { "$sort": { "n": 1 } },
            meta,
        ]);
        rewrite_sort_metadata(&mut piped, || None);
        assert_eq!(
            piped,
            stages(vec![
                doc! { "$addFields": { "q": 1 } },
                doc! { "$sort": { "n": 1 } },
                doc! { "$addFields": { "m": { "$literal": [] } } },
            ])
        );
    }

    #[test]
    fn add_fields_with_no_sort_key_in_reach_gives_an_empty_array() {
        let mut stages = stages(vec![doc! { "$addFields": { "k": { "$meta": "sortKey" } } }]);
        rewrite_sort_metadata(&mut stages, || None);
        assert_eq!(
            stages,
            self::stages(vec![doc! { "$addFields": { "k": { "$literal": [] } } }])
        );
    }

    #[test]
    fn a_sort_key_read_only_behind_an_inhibit_is_never_asked_for() {
        let inhibit = doc! { "$_internalInhibitOptimization": {} };
        let meta = doc! { "$addFields": { "m": { "$meta": "sortKey" } } };
        let sort = doc! { "$sort": { "n": 1 } };
        let unrequested = |v: Vec<Document>| sort_key_unrequested(&stages(v));
        assert!(unrequested(vec![
            sort.clone(),
            inhibit.clone(),
            meta.clone()
        ]));
        assert!(unrequested(vec![
            inhibit.clone(),
            sort.clone(),
            meta.clone()
        ]));
        assert!(!unrequested(vec![
            sort.clone(),
            meta.clone(),
            inhibit.clone(),
            meta.clone()
        ]));
        assert!(!unrequested(vec![sort.clone(), meta.clone()]));
        // A `$lookup`'s reader asks for its own documents' key only.
        let lookup = doc! { "$lookup": { "from": "c", "pipeline": [meta.clone()], "as": "j" } };
        assert!(unrequested(vec![lookup, inhibit.clone(), meta.clone()]));
        // A `$geoNear` sets the key whatever is asked.
        let geo = doc! { "$geoNear": { "near": [0, 0], "distanceField": "d" } };
        assert!(!unrequested(vec![geo, inhibit.clone(), meta.clone()]));

        // The documents carry no key at all: the read is missing.
        let mut piped = stages(vec![sort.clone(), inhibit.clone(), meta]);
        rewrite_sort_metadata(&mut piped, || Some(doc! { "n": 1 }));
        assert!(!piped.iter().any(|s| mentions(s, "sortKey")));
        assert!(piped.contains(&Bson::Document(doc! { "$addFields": { "m": "$$REMOVE" } })));
    }
}
