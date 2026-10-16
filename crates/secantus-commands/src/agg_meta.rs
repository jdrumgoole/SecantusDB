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
}
