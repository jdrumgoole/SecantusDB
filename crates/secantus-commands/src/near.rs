//! `$near` and `$nearSphere` in a query: where mongod allows one, the index
//! it needs, and the order it returns documents in.
//!
//! The matcher (`secantus_core::geoshape`) says which documents are inside
//! the query's distance bounds. mongod also SORTS them, nearest first, and
//! refuses the query when no geo index on the field could be read for it.

use crate::storage::{Storage, StorageError};
use bson::{Bson, Document};
use secantus_core::fallback::Fallback;
use secantus_core::geoshape::{near_condition, Near};
use secantus_core::query::bson_value_repr;

/// A `$near` found in a filter.
pub(crate) struct Found {
    pub field: String,
    pub near: Near,
    /// The filter's other top-level clauses, as mongod prints them.
    others: Vec<String>,
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

/// Whether a clause holds a `$near` anywhere under it.
fn holds_near(v: &Bson) -> bool {
    match v {
        Bson::Document(d) => d.iter().any(|(k, v)| {
            matches!(k.as_str(), "$near" | "$nearSphere" | "$geoNear") || holds_near(v)
        }),
        Bson::Array(list) => list.iter().any(holds_near),
        _ => false,
    }
}

fn walk(
    filter: &Document,
    found: &mut Vec<Found>,
    others: &mut Vec<String>,
) -> Result<(), StorageError> {
    for (key, value) in filter {
        match (key.as_str(), value) {
            ("$and", Bson::Array(clauses)) => {
                for clause in clauses {
                    if let Bson::Document(inner) = clause {
                        walk(inner, found, others)?;
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
                    others: Vec::new(),
                }),
                None if holds_near(value) => {
                    return Err(refuse(2, "geo $near must be top-level expr"));
                }
                None => others.push(format!("{field} {}", bson_value_repr(value))),
            },
            (field, v) => others.push(format!("{field} $eq {}", bson_value_repr(v))),
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
    let (mut found, mut others) = (Vec::new(), Vec::new());
    walk(filter, &mut found, &mut others)?;
    if found.len() > 1 {
        return Err(refuse(2, "Too many geoNear expressions"));
    }
    Ok(found.pop().map(|mut f| {
        f.others = others;
        f
    }))
}

/// A query's tree as mongod prints one it could not plan: the `$near` leaf,
/// under an `$and` with the other clauses when `query` has any.
pub(crate) fn tree(query: Option<&Document>, leaf: &str) -> String {
    let others: Vec<String> = query
        .into_iter()
        .flat_map(|q| q.iter())
        .map(|(field, v)| match v {
            Bson::Document(_) => format!("{field} {}", bson_value_repr(v)),
            _ => format!("{field} $eq {}", bson_value_repr(v)),
        })
        .collect();
    if others.is_empty() {
        return leaf.to_string();
    }
    let mut lines = vec!["$and".to_string()];
    lines.extend(others.iter().map(|o| format!("    {o}")));
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
}

fn shown(d: Option<&Document>) -> String {
    d.map_or("{}".to_string(), |d| {
        bson_value_repr(&Bson::Document(d.clone()))
    })
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
    ) -> Result<(), StorageError> {
        // A collection that does not exist is read as nothing, with no plan
        // to make (measured 8.2.11, 2026-10-15: `find` and `count` answer
        // empty, and `explain` reads its dry run that way).
        if !storage.collection_exists(db, coll)? {
            return Ok(());
        }
        let indexes = storage.list_indexes(db, coll)?;
        let kind_of = |spec: &Document| -> Option<String> {
            let kind = spec.get_document("key").ok()?.get_str(&self.field).ok()?;
            (!spec.get_bool("hidden").unwrap_or(false) && self.reads(kind))
                .then(|| kind.to_string())
        };
        let usable: Vec<(&Document, String)> = indexes
            .iter()
            .filter_map(|spec| kind_of(spec).map(|kind| (spec, kind)))
            .collect();
        if !usable.is_empty() {
            return centre_hashes(&usable, self.near.centre);
        }
        Err(refuse(
            291,
            self.unplannable(db, coll, asked, "unable to find index for $geoNear query"),
        ))
    }

    /// mongod's message for a `$near` it could not plan, for `reason`.
    pub(crate) fn unplannable(&self, db: &str, coll: &str, asked: Asked, reason: &str) -> String {
        // The bound as given: metres for GeoJSON, radians for a legacy point
        // on the sphere, the coordinates' own units in the plane.
        let max = match self.near.max {
            None => "1.79769e+308".to_string(),
            Some(m) if self.near.spherical && !self.near.geojson => {
                crate::crud::fmt_g(m / secantus_core::geoshape::EARTH_RADIUS_METERS)
            }
            Some(m) => crate::crud::fmt_g(m),
        };
        let sphere = i32::from(self.near.spherical && !self.near.geojson);
        let leaf = format!(
            "GEONEAR  field={} maxdist={max} isNearSphere={sphere}",
            self.field
        );
        let tree = if self.others.is_empty() {
            leaf
        } else {
            let mut lines = vec!["$and".to_string()];
            lines.extend(self.others.iter().map(|o| format!("    {o}")));
            lines.push(format!("    {leaf}"));
            lines.join("\n")
        };
        let mut paging = String::new();
        if asked.limit > 0 {
            paging.push_str(&format!(" limit={}", asked.limit));
        }
        if asked.skip > 0 {
            paging.push_str(&format!(" skip={}", asked.skip));
        }
        format!(
            "error processing query: ns={db}.{coll}{paging}Tree: {tree}\nSort: {}\nProj: {}\n \
             planner returned error :: caused by :: {reason}",
            shown(asked.sort),
            shown(asked.projection)
        )
    }

    /// `rows` nearest first. Documents at one distance keep the order given,
    /// points ahead of lines and polygons.
    pub(crate) fn nearest_first(&self, rows: Vec<(i64, Vec<u8>)>) -> Vec<(i64, Vec<u8>)> {
        let mut keyed: Vec<(f64, bool, (i64, Vec<u8>))> = rows
            .into_iter()
            .map(|row| {
                let value = bson::RawDocument::from_bytes(&row.1)
                    .ok()
                    .and_then(|raw| at_path(raw, &self.field));
                let distance = value
                    .as_ref()
                    .and_then(|v| self.near.distance(v))
                    .unwrap_or(f64::INFINITY);
                let shaped = value.as_ref().is_some_and(|v| {
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

/// The value at a dotted path of a stored document.
fn at_path(raw: &bson::RawDocument, path: &str) -> Option<Bson> {
    let (head, rest) = match path.split_once('.') {
        Some((head, rest)) => (head, Some(rest)),
        None => (path, None),
    };
    let value = Bson::try_from(raw.get(head).ok()??).ok()?;
    match rest {
        None => Some(value),
        Some(rest) => {
            let mut at = &value;
            for part in rest.split('.') {
                at = at.as_document()?.get(part)?;
            }
            Some(at.clone())
        }
    }
}
