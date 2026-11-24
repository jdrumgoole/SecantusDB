//! `$near` and `$nearSphere` in a query: where mongod allows one, the index
//! it needs, and the order it returns documents in.
//!
//! The matcher (`secantus_core::geoshape`) says which documents are inside
//! the query's distance bounds. mongod also SORTS them, nearest first, and
//! refuses the query when no geo index on the field could be read for it.

use crate::storage::{RecordId, Storage, StorageError};
use bson::{Bson, Document};
use secantus_core::fallback::Fallback;
use secantus_core::geoshape::{near_condition, Near};

/// A `$near` found in a filter.
pub(crate) struct Found {
    pub field: String,
    pub near: Near,
    /// The whole filter, which mongod prints as its query tree when it
    /// cannot plan the read.
    filter: Document,
}

fn refuse(code: i32, message: impl Into<String>) -> StorageError {
    StorageError::WriteError {
        code,
        errmsg: message.into(),
        exec: false,
    }
}

fn parsed(cond: &Document) -> Result<Option<Near>, StorageError> {
    near_condition(cond).map_err(|f| match f {
        Fallback::Mongo { code, message, .. } => refuse(code, message),
        Fallback::Defer => refuse(2, "invalid argument in geo near query"),
    })
}

/// Whether a filter holds a `$near` where mongod reads an operator: as an
/// operator of a field's condition, under `$not` / `$elemMatch`, or in a
/// clause of `$and` / `$or` / `$nor`. A `$near` key inside a VALUE (the
/// operand of `$eq`, `$ne`, `$in`, `$all`, ... or an embedded document
/// compared by equality) is part of that value, not a `$near`.
fn query_holds_near(filter: &Document) -> bool {
    filter.iter().any(|(k, v)| match (k.as_str(), v) {
        ("$and" | "$or" | "$nor", Bson::Array(clauses)) => clauses
            .iter()
            .any(|c| matches!(c, Bson::Document(d) if query_holds_near(d))),
        (k, _) if k.starts_with('$') => false,
        (_, v) => cond_holds_near(v),
    })
}

/// Whether a field's condition holds a `$near` operator.
fn cond_holds_near(v: &Bson) -> bool {
    let Bson::Document(d) = v else { return false };
    if !d.keys().next().is_some_and(|k| k.starts_with('$')) {
        return false;
    }
    d.iter().any(|(k, v)| match k.as_str() {
        "$near" | "$nearSphere" | "$geoNear" => true,
        "$not" => cond_holds_near(v),
        "$elemMatch" => match v {
            Bson::Document(inner) => query_holds_near(inner) || cond_holds_near(v),
            _ => false,
        },
        _ => false,
    })
}

fn holds_near(v: &Bson) -> bool {
    match v {
        Bson::Document(d) => query_holds_near(d),
        Bson::Array(list) => list
            .iter()
            .any(|c| matches!(c, Bson::Document(d) if query_holds_near(d))),
        _ => false,
    }
}

fn walk(filter: &Document, found: &mut Vec<Found>) -> Result<(), StorageError> {
    for (key, value) in filter {
        match (key.as_str(), value) {
            ("$and", Bson::Array(clauses)) => {
                for clause in clauses {
                    if let Bson::Document(inner) = clause {
                        walk(inner, found)?;
                    }
                }
            }
            (k, v) if k.starts_with('$') => {
                if holds_near(v) {
                    return Err(refuse(2, "geo $near must be top-level expr"));
                }
            }
            (field, Bson::Document(cond)) => match parsed(cond)? {
                Some(near) => found.push(Found {
                    field: field.to_string(),
                    near,
                    filter: Document::new(),
                }),
                None if cond_holds_near(value) => {
                    return Err(refuse(2, "geo $near must be top-level expr"));
                }
                None => {}
            },
            _ => {}
        }
    }
    Ok(())
}

/// The `$near` of a filter, if it has one: at the top level or under a
/// top-level `$and`, and only one.
pub(crate) fn find_near(filter: &Document) -> Result<Option<Found>, StorageError> {
    if !holds_near(&Bson::Document(filter.clone())) {
        return Ok(None);
    }
    let mut found = Vec::new();
    walk(filter, &mut found)?;
    if found.len() > 1 {
        return Err(refuse(2, "Too many geoNear expressions"));
    }
    Ok(found.pop().map(|mut f| {
        f.filter = filter.clone();
        f
    }))
}

/// A query's tree as mongod prints one it could not plan: the `$near` leaf,
/// under an `$and` with the query's clauses when it has any.
pub(crate) fn tree(query: Option<&Document>, leaf: &str) -> String {
    let body = query
        .filter(|q| !q.is_empty())
        .map(crate::explain::debug_string::debug_string)
        .unwrap_or_default();
    if body.is_empty() {
        return leaf.to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    if body.starts_with("$and\n") {
        lines.extend(body.lines().map(str::to_string));
    } else {
        lines.push("$and".to_string());
        lines.extend(body.lines().map(|l| format!("    {l}")));
    }
    lines.push(format!("    {leaf}"));
    lines.join("\n")
}

/// What the rest of the read asked for, which mongod prints with the query
/// it could not plan.
#[derive(Clone, Copy, Default)]
pub(crate) struct Asked<'a> {
    pub skip: i64,
    pub limit: i64,
    pub sort: Option<&'a Document>,
    pub projection: Option<&'a Document>,
    /// The read is a command's own, which consults and writes mongod's
    /// plan cache (`explain::plan_cache`); a read made only to learn about
    /// the plan does not.
    pub cached: bool,
    /// `distinct`'s key, part of the plan cache key.
    pub distinct: Option<&'a str>,
    /// The read is an aggregate's front that mongod runs in SBE
    /// (`explain::plan::Query::sbe`).
    pub sbe: bool,
}

impl Found {
    /// Whether an index kind can answer this query: GeoJSON needs the
    /// sphere, a flat `$near` needs a `2d` index, and a legacy point on the
    /// sphere reads either.
    fn reads(&self, kind: &str) -> bool {
        match kind {
            "2dsphere" => self.near.spherical,
            "2d" => !self.near.geojson,
            _ => false,
        }
    }

    /// mongod's refusal when the collection has no geo index this query
    /// could read.
    pub(crate) fn check_index(
        &self,
        storage: &dyn Storage,
        db: &str,
        coll: &str,
        asked: Asked,
        hinted: bool,
        collation: Option<&secantus_core::collation::Collation>,
    ) -> Result<(), StorageError> {
        let Some(usable) = self.refuse_unplannable(storage, db, coll, asked, hinted, collation)?
        else {
            return Ok(());
        };
        let usable: Vec<(&Document, String)> = usable.iter().map(|(s, k)| (s, k.clone())).collect();
        centre_hashes(&usable, self.near.centre)
    }

    /// The refusal alone, with no read of the data: `None` when the
    /// collection does not exist, else the indexes the query could read.
    pub(crate) fn refuse_unplannable(
        &self,
        storage: &dyn Storage,
        db: &str,
        coll: &str,
        asked: Asked,
        hinted: bool,
        collation: Option<&secantus_core::collation::Collation>,
    ) -> Result<Option<Vec<(Document, String)>>, StorageError> {
        // A collection that does not exist is read as nothing, with no plan
        // to make (measured 8.2.11, 2026-10-15: `find` and `count` answer
        // empty, and `explain` reads its dry run that way).
        if !storage.collection_exists(db, coll)? {
            return Ok(None);
        }
        let indexes = storage.list_indexes(db, coll)?;
        let kind_of = |spec: &Document| -> Option<String> {
            let kind = spec.get_document("key").ok()?.get_str(&self.field).ok()?;
            (!spec.get_bool("hidden").unwrap_or(false) && self.reads(kind))
                .then(|| kind.to_string())
        };
        let usable: Vec<(Document, String)> = indexes
            .iter()
            .filter_map(|spec| kind_of(spec).map(|kind| (spec.clone(), kind)))
            .collect();
        if usable.is_empty() {
            return Err(refuse(
                291,
                self.unplannable(db, coll, asked, "unable to find index for $geoNear query"),
            ));
        }
        // A hint to a geo index plans it whatever the query bounds; without
        // one, an index whose geo field does not lead is read only when a
        // predicate bounds its leading field (`explain::plan::near_refusal`).
        if !hinted {
            let catalog = crate::explain::catalog_of_collection(storage, db, coll)?;
            let canonical = crate::explain::parsed_query(&self.filter);
            let implies = |partial: &Document| storage.query_implies_partial(&self.filter, partial);
            if let Some(why) = crate::explain::near_refusal(
                &canonical,
                &self.field,
                &self.near,
                &catalog,
                &implies,
                collation,
            ) {
                return Err(refuse(
                    291,
                    self.unplannable(db, coll, asked, why.message()),
                ));
            }
        }
        Ok(Some(usable))
    }

    /// mongod's message for a `$near` it could not plan, for `reason`.
    pub(crate) fn unplannable(&self, db: &str, coll: &str, asked: Asked, reason: &str) -> String {
        crate::explain::CanonicalQuery {
            ns: format!("{db}.{coll}"),
            filter: &self.filter,
            sort: asked.sort,
            projection: asked.projection,
            skip: Some(asked.skip),
            limit: Some(asked.limit),
            collation: None,
            batch_size: None,
        }
        .planner_error(reason)
    }

    /// `rows` nearest first. Documents at one distance keep the order given,
    /// points ahead of lines and polygons.
    pub(crate) fn nearest_first(&self, rows: Vec<(RecordId, Vec<u8>)>) -> Vec<(RecordId, Vec<u8>)> {
        let mut keyed: Vec<(f64, bool, (RecordId, Vec<u8>))> = rows
            .into_iter()
            .map(|row| {
                let values = secantus_core::bsondoc::from_bytes(&row.1)
                    .map(|doc| {
                        secantus_core::geoorder::elements_along_path(&doc, &self.field, false)
                    })
                    .unwrap_or_default();
                // The nearest value along the path measures the document.
                let nearest = values
                    .iter()
                    .filter_map(|v| self.near.distance(v).map(|d| (d, v)))
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                let distance = nearest.map_or(f64::INFINITY, |(d, _)| d);
                let shaped = nearest.is_some_and(|(_, v)| {
                    matches!(v, Bson::Document(d) if d.get_str("type").is_ok_and(|t| t != "Point"))
                });
                (distance, shaped, row)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        keyed.into_iter().map(|(_, _, row)| row).collect()
    }
}

/// A `2d` index the query may read hashes the centre first, and refuses one
/// outside its range (16433, measured 8.2.11, 2026-10-15). Read as the one
/// plan, that is an execution error; beside another index, mongod's trial
/// of the two plans is what meets it.
pub(crate) fn centre_hashes(
    usable: &[(&Document, String)],
    centre: secantus_core::geoshape::P,
) -> Result<(), StorageError> {
    for (spec, kind) in usable {
        if kind != "2d" {
            continue;
        }
        let Ok(index) = secantus_core::geoshape::flat_index(spec) else {
            continue;
        };
        if let Some(why) = secantus_core::geoshape::flat_centre_problem(&index, centre) {
            if usable.len() > 1 {
                return Err(refuse(16433, format!("{MULTIPLANNER}{why}")));
            }
            return Err(StorageError::WriteError {
                code: 16433,
                errmsg: why,
                exec: true,
            });
        }
    }
    Ok(())
}

/// mongod's prefix on an error met while it tries candidate plans.
pub(crate) const MULTIPLANNER: &str =
    "error while multiplanner was selecting best plan :: caused by :: ";

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn a_near_key_inside_a_value_is_not_a_near() {
        for f in [
            doc! {"a": {"$eq": {"$near": 1}}},
            doc! {"a": {"$ne": {"$nearSphere": 1}}},
            doc! {"a": {"x": {"$near": 1}}},
            doc! {"a": {"$all": [{"x": {"$near": 1}}]}},
            doc! {"a": {"$in": [{"$near": 1}]}},
        ] {
            assert!(!query_holds_near(&f), "{f}");
        }
    }

    #[test]
    fn a_near_operator_is_found_where_mongod_reads_one() {
        for f in [
            doc! {"a": {"$near": [0, 0]}},
            doc! {"$or": [{"a": {"$near": [0, 0]}}]},
            doc! {"a": {"$not": {"$near": [0, 0]}}},
            doc! {"a": {"$elemMatch": {"b": {"$near": [0, 0]}}}},
        ] {
            assert!(query_holds_near(&f), "{f}");
        }
    }
}
