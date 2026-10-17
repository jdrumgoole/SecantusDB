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
pub(crate) const SORT_KEY: &str = "__secantus_meta_sortKey";
/// The fields a `$geoNear` leaves its distance and point in.
pub(crate) const GEO_DISTANCE: &str = "__secantus_meta_geoNearDistance";
pub(crate) const GEO_POINT: &str = "__secantus_meta_geoNearPoint";

/// The stages a document's sort key and `$geoNear` metadata pass through
/// (measured 8.2.11: `$match`, `$limit`, `$skip`, `$addFields`, `$project`,
/// `$unwind` and a later `$sort` keep them; a `$group` reads them and drops
/// them).
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
fn mentions(v: &Bson, kind: &str) -> bool {
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
/// gives `[]` (mongod 8.2.11) and the other stages are left to refuse it.
pub(crate) fn rewrite_sort_metadata(stages: &mut Vec<Bson>) {
    const KINDS: [(&str, &str); 3] = [
        ("sortKey", SORT_KEY),
        ("geoNearDistance", GEO_DISTANCE),
        ("geoNearPoint", GEO_POINT),
    ];
    if !stages
        .iter()
        .any(|s| KINDS.iter().any(|(k, _)| mentions(s, k)))
    {
        return;
    }
    let mut out: Vec<Bson> = Vec::with_capacity(stages.len() + 4);
    // The hidden fields in place, and so the metadata in reach.
    let (mut sort_live, mut geo_live) = (false, false);
    let n = stages.len();
    for (i, stage) in std::mem::take(stages).into_iter().enumerate() {
        let mut stage = stage;
        let name = name(&stage).to_string();
        let keeps = KEEPS_SORT_METADATA.contains(&name.as_str());
        if keeps || name == "$group" {
            if sort_live {
                replace(&mut stage, "sortKey", SORT_KEY);
            } else if matches!(name.as_str(), "$addFields" | "$set") {
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
                let by = stage.as_document().and_then(|d| d.get("$sort")).cloned();
                out.push(stage);
                if let Some(by) = by {
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
            _ => {}
        }
        out.push(stage);
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
        rewrite_sort_metadata(&mut stages);
        let read = format!("${SORT_KEY}");
        assert_eq!(
            stages,
            self::stages(vec![
                doc! { "$sort": { "a": 1 } },
                doc! { "$_secantusSortKey": { "into": SORT_KEY, "by": { "a": 1 } } },
                // An inclusion projection carries the key on.
                doc! { "$project": { "a": 1, SORT_KEY: 1 } },
                doc! { "$project": { "k": read.clone(), SORT_KEY: 1 } },
                doc! { "$group": { "_id": read } },
                // Past the `$group` there is no sort key.
                doc! { "$project": { "k": { "$meta": "sortKey" } } },
            ])
        );
    }

    #[test]
    fn add_fields_with_no_sort_key_in_reach_gives_an_empty_array() {
        let mut stages = stages(vec![doc! { "$addFields": { "k": { "$meta": "sortKey" } } }]);
        rewrite_sort_metadata(&mut stages);
        assert_eq!(
            stages,
            self::stages(vec![doc! { "$addFields": { "k": { "$literal": [] } } }])
        );
    }
}
