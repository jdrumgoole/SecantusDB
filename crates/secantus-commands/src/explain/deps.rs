//! The fields of a collection's documents an aggregate's stages read.
//!
//! When the stages after the query need only some fields of each document,
//! mongod adds a projection of those fields to the query (measured 8.2.11
//! with `explain`): `[{$replaceRoot: {newRoot: "$n"}}]` reads the
//! collection through `PROJECTION_SIMPLE {n: 1, _id: 0}`. The stages are
//! walked front to back until one that says exactly what it outputs (a
//! `$project`, `$replaceRoot`, `$group` ...); without one, the whole
//! document is needed and there is no projection.

use super::hoist::{expr_paths, filter_paths, unwind_paths};
use crate::util::truthy;
use bson::{Bson, Document};
use std::collections::BTreeSet;

/// What the stages read so far.
#[derive(Default)]
struct Deps {
    fields: BTreeSet<String>,
    id: bool,
    /// Paths an earlier stage wrote: a later read of them reads no source
    /// field.
    written: Vec<String>,
}

impl Deps {
    fn read(&mut self, paths: Vec<String>) {
        for path in paths {
            let shadowed = self
                .written
                .iter()
                .any(|w| path == *w || path.starts_with(&format!("{w}.")));
            if shadowed {
                continue;
            }
            if path == "_id" || path.starts_with("_id.") {
                self.id = true;
            } else {
                self.fields.insert(path);
            }
        }
    }

    fn expr(&mut self, v: &Bson) -> Option<()> {
        let mut paths = Vec::new();
        expr_paths(v, &mut paths)?;
        self.read(paths);
        Some(())
    }

    /// The projection: `_id` first when it is read, the fields in order, a
    /// field read whole standing for the paths inside it. `None`: nothing
    /// to project.
    fn projection(self) -> Option<Document> {
        let mut kept: Vec<String> = Vec::new();
        for f in &self.fields {
            if !kept.iter().any(|k| f.starts_with(&format!("{k}."))) {
                kept.push(f.clone());
            }
        }
        if kept.is_empty() && !self.id {
            return None;
        }
        let mut out = Document::new();
        if self.id {
            out.insert("_id", 1_i32);
        }
        for f in kept {
            out.insert(f, 1_i32);
        }
        if !self.id {
            out.insert("_id", 0_i32);
        }
        Some(out)
    }
}

/// An inclusion or computed `$project`'s reads: whether `_id` is kept, and
/// what the other fields read. `None`: an exclusion, which passes the rest
/// of the document along.
fn project_reads(spec: &Document, deps: &mut Deps) -> Option<()> {
    let mut id = true;
    let mut included = false;
    for (field, value) in spec {
        let on = match value {
            Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => {
                Some(truthy(value))
            }
            _ => None,
        };
        if field == "_id" {
            match on {
                Some(keep) => id = keep,
                None => deps.expr(value)?,
            }
            continue;
        }
        match on {
            Some(true) => {
                included = true;
                deps.read(vec![field.clone()]);
            }
            Some(false) => return None,
            None => {
                included = true;
                // A nested inclusion (`{n: {k: 1}}`) or an expression.
                match value {
                    Bson::Document(d) if !d.keys().any(|k| k.starts_with('$')) => {
                        let mut nested = Deps::default();
                        project_reads(d, &mut nested)?;
                        deps.read(
                            nested
                                .fields
                                .into_iter()
                                .map(|f| format!("{field}.{f}"))
                                .collect(),
                        );
                    }
                    other => deps.expr(other)?,
                }
            }
        }
    }
    if !included && !id {
        return None;
    }
    if id {
        deps.read(vec!["_id".to_string()]);
    }
    Some(())
}

/// The projection mongod adds to the query for `rest`, the stages it did
/// not take into it.
pub(crate) fn projection(rest: &[Document]) -> Option<Document> {
    let mut deps = Deps::default();
    for stage in rest {
        let (name, arg) = stage.iter().next()?;
        match (name.as_str(), arg) {
            // A `$match` moved ahead of a `$replaceRoot` reads as any other
            // (measured 8.2.11, RM-536).
            ("$match" | super::hoist::PUSHED_MATCH, Bson::Document(filter)) => {
                let mut paths = Vec::new();
                filter_paths(filter, &mut paths)?;
                deps.read(paths);
            }
            ("$unwind", spec) => {
                let paths = unwind_paths(spec)?;
                deps.read(vec![paths[0].clone()]);
                deps.written.extend(paths.into_iter().skip(1));
            }
            ("$sort" | super::shape::WINDOW_SORT, Bson::Document(spec)) => {
                deps.read(spec.keys().cloned().collect());
            }
            ("$limit" | "$skip", _) => {}
            ("$addFields" | "$set", Bson::Document(fields)) => {
                for (_, v) in fields {
                    deps.expr(v)?;
                }
                deps.written.extend(fields.keys().cloned());
            }
            ("$unset", spec) => {
                let names: Vec<String> = match spec {
                    Bson::String(f) => vec![f.clone()],
                    Bson::Array(items) => items
                        .iter()
                        .map(|i| i.as_str().map(str::to_string))
                        .collect::<Option<_>>()?,
                    _ => return None,
                };
                deps.written.extend(names);
            }
            ("$project", Bson::Document(spec)) => {
                project_reads(spec, &mut deps)?;
                return deps.projection();
            }
            ("$replaceRoot", Bson::Document(spec)) => {
                deps.expr(spec.get("newRoot")?)?;
                return deps.projection();
            }
            ("$replaceWith", v) => {
                deps.expr(v)?;
                return deps.projection();
            }
            ("$group", Bson::Document(spec)) => {
                for (k, v) in spec {
                    match (k.as_str(), v) {
                        ("_id", v) => deps.expr(v)?,
                        (_, Bson::Document(acc)) => {
                            for (_, arg) in acc {
                                deps.expr(arg)?;
                            }
                        }
                        _ => return None,
                    }
                }
                return deps.projection();
            }
            ("$count", _) => return deps.projection(),
            _ => return None,
        }
    }
    None
}

/// Whether `rest`, the stages mongod did not take into the query, read each
/// document's sort key, which the query must then compute
/// (`DepsTracker`'s `sortKey` metadata; measured 8.2.11, RM-428): a
/// `{$meta: "sortKey"}`, a `$rank`, `$denseRank` or `$documentNumber`
/// window, or a `$rankFusion`, in a stage or in a `$facet`'s pipelines,
/// before a stage that drops the metadata (a `$group`). A `$lookup`'s or
/// `$unionWith`'s pipeline reads other documents; a `$sort` in front
/// that produces the key does not stop the need.
pub(crate) fn reads_sort_key(rest: &[Bson]) -> bool {
    for stage in rest {
        let Some((name, arg)) = stage.as_document().and_then(|d| d.iter().next()) else {
            return false;
        };
        match name.as_str() {
            "$lookup" | "$unionWith" | "$graphLookup" => continue,
            "$facet" => {
                return arg.as_document().is_some_and(|facets| {
                    facets.values().any(|sub| match sub {
                        Bson::Array(stages) => reads_sort_key(stages),
                        _ => false,
                    })
                });
            }
            "$rankFusion" => return true,
            _ => {}
        }
        if reads_key(arg) {
            return true;
        }
        if matches!(
            name.as_str(),
            "$group" | "$bucket" | "$bucketAuto" | "$sortByCount" | "$count" | "$redact"
        ) {
            return false;
        }
    }
    false
}

/// Whether an argument reads a sort key: `{$meta: "sortKey"}`, or a
/// window function that ranks by it.
fn reads_key(v: &Bson) -> bool {
    match v {
        Bson::Document(d) => {
            if d.len() == 1 && d.get_str("$meta") == Ok("sortKey") {
                return true;
            }
            d.iter().any(|(k, x)| {
                matches!(k.as_str(), "$rank" | "$denseRank" | "$documentNumber")
                    || (k == "$_secantusFusePrep"
                        && x.as_document()
                            .is_some_and(|p| p.get_str("kind") == Ok("rank")))
                    || reads_key(x)
            })
        }
        Bson::Array(items) => items.iter().any(reads_key),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn p(stages: Vec<Document>) -> Option<Document> {
        projection(&stages)
    }

    // Every expectation is mongod 8.2.11's `PROJECTION_*` `transformBy`.
    #[test]
    fn reads_what_the_stages_need() {
        assert_eq!(
            p(vec![doc! { "$replaceRoot": { "newRoot": "$n" } }]),
            Some(doc! { "n": 1, "_id": 0 })
        );
        assert_eq!(
            p(vec![
                doc! { "$unwind": "$t" },
                doc! { "$project": { "y": 1, "b": 1, "a": 1 } }
            ]),
            Some(doc! { "_id": 1, "a": 1, "b": 1, "t": 1, "y": 1 })
        );
        assert_eq!(
            p(vec![
                doc! { "$addFields": { "z": "$a" } },
                doc! { "$project": { "z": 1 } }
            ]),
            Some(doc! { "_id": 1, "a": 1 })
        );
        assert_eq!(
            p(vec![
                doc! { "$replaceRoot": { "newRoot": { "x": "$n.k", "y": "$n" } } }
            ]),
            Some(doc! { "n": 1, "_id": 0 })
        );
        assert_eq!(
            p(vec![
                doc! { "$unwind": { "path": "$t", "includeArrayIndex": "i" } },
                doc! { "$project": { "i": 1 } }
            ]),
            Some(doc! { "_id": 1, "t": 1 })
        );
    }

    #[test]
    fn the_whole_document_needs_no_projection() {
        assert_eq!(p(vec![doc! { "$unwind": "$t" }]), None);
        assert_eq!(
            p(vec![
                doc! { "$unwind": "$t" },
                doc! { "$project": { "a": 0 } }
            ]),
            None
        );
        assert_eq!(
            p(vec![doc! { "$replaceRoot": { "newRoot": "$$ROOT" } }]),
            None
        );
        assert_eq!(
            p(vec![
                doc! { "$addFields": { "z": 1 } },
                doc! { "$replaceRoot": { "newRoot": "$z" } }
            ]),
            None
        );
    }

    #[test]
    fn a_rank_in_a_facet_reads_the_sort_key_and_a_group_stops_the_need() {
        let rank = bson::doc! { "$setWindowFields": {
        "sortBy": { "x": 1 }, "output": { "r": { "$rank": {} } } } };
        let facet = Bson::Document(bson::doc! { "$facet": { "f": [rank.clone()] } });
        assert!(reads_sort_key(&[facet]));
        let group = Bson::Document(bson::doc! { "$group": { "_id": "$x" } });
        assert!(!reads_sort_key(&[group, Bson::Document(rank.clone())]));
        let lookup = Bson::Document(bson::doc! { "$lookup": {
        "from": "s", "pipeline": [rank], "as": "j" } });
        assert!(!reads_sort_key(&[lookup]));
    }
}
