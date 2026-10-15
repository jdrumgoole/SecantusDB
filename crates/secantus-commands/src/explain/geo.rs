//! The order of a read mongod answers from a geo index.
//!
//! A `$geoWithin` or `$geoIntersects` on a field with a `2d` or `2dsphere`
//! index is an index scan of the query's covering, and a hint to a geo
//! index a scan of all of it; either way the documents come in the order of
//! the index's keys ([`secantus_core::geoorder::scan_order`]). A `$near` is
//! mongod's ring search ([`secantus_core::geoorder::near`]). The keys,
//! coverings and the search are transcriptions of mongod's own, so the
//! order is mongod's; this module only decides which index mongod reads,
//! and with what bounds on its other fields.
//!
//! Which index, when several could serve, is a fixed preference measured
//! against mongod 8.2.11 (whose multi-planner decides by trial): a
//! `$geoWithin` reads the `2dsphere` index before a `2d` one, a `$near` the
//! `2d` index before a `2dsphere` one. A query another (non-geo) index
//! could serve is left to the planner, as are a compound geo index's
//! trailing fields under anything but an equality.

use crate::near::Found;
use crate::storage::{Storage, StorageError};
use bson::{Bson, Document};
use secantus_core::geoorder::{near::near_order, FieldBounds, GeoIndex};

type Rows = Vec<(i64, Vec<u8>)>;

/// The largest collection a `$near` replays mongod's search over (the
/// search reads every index entry for its density estimate).
const NEAR_COLLECTION: usize = 2000;

fn refuse(code: i32, message: impl Into<String>) -> StorageError {
    StorageError::WriteError {
        code,
        errmsg: message.into(),
        exec: false,
    }
}

/// The collection's visible geo indexes, in catalog order.
fn geo_indexes(storage: &dyn Storage, db: &str, coll: &str) -> Result<Vec<GeoIndex>, StorageError> {
    Ok(storage
        .list_indexes(db, coll)?
        .iter()
        .filter(|spec| !spec.get_bool("hidden").unwrap_or(false))
        .filter_map(GeoIndex::from_spec)
        .collect())
}

/// The index spec a hint names, if any.
fn hinted_spec(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    hint: &Bson,
) -> Result<Option<Document>, StorageError> {
    let specs = storage.list_indexes(db, coll)?;
    Ok(match hint {
        Bson::String(name) => specs.into_iter().find(|s| s.get_str("name") == Ok(name)),
        Bson::Document(key) if !key.is_empty() && !key.contains_key("$natural") => specs
            .into_iter()
            .find(|s| s.get_document("key").is_ok_and(|k| k == key)),
        _ => None,
    })
}

/// A hint to a geo index: the index, or `None` for any other hint.
pub(crate) fn hinted_geo(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    hint: Option<&Bson>,
) -> Result<Option<GeoIndex>, StorageError> {
    let Some(hint) = hint else { return Ok(None) };
    Ok(hinted_spec(storage, db, coll, hint)?.and_then(|s| GeoIndex::from_spec(&s)))
}

/// mongod's refusals of a `$near` under a hint (measured, 8.2.11): a
/// `$natural` hint is 2, a hint naming no index is 2 with the query, and a
/// hint to an index that cannot answer the `$near` is 291.
pub(crate) fn check_near_hint(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    hint: Option<&Bson>,
    near: &Found,
    unplannable: impl Fn(&str) -> String,
) -> Result<Option<GeoIndex>, StorageError> {
    let Some(hint) = hint else { return Ok(None) };
    match hint {
        Bson::Document(d) if d.is_empty() => return Ok(None),
        Bson::Document(d) if d.contains_key("$natural") => {
            return Err(refuse(
                2,
                "geoNear expression not allowed with $natural hint",
            ))
        }
        _ => {}
    }
    let Some(spec) = hinted_spec(storage, db, coll, hint)? else {
        return Err(refuse(
            2,
            unplannable("hint provided does not correspond to an existing index"),
        ));
    };
    let index = GeoIndex::from_spec(&spec).filter(|ix| {
        ix.geo_field() == near.field
            && if ix.is_sphere() {
                near.near.spherical
            } else {
                !near.near.geojson
            }
    });
    match index {
        Some(ix) => Ok(Some(ix)),
        None => Err(refuse(
            291,
            unplannable("unable to find index for $geoNear query"),
        )),
    }
}

/// The filter's top-level clauses: its own, and those of a top-level
/// `$and`, as `(field, value)`.
fn clauses(filter: &Document) -> Vec<(&str, &Bson)> {
    let mut out = Vec::new();
    for (k, v) in filter {
        match (k.as_str(), v) {
            ("$and", Bson::Array(list)) => {
                for c in list {
                    if let Bson::Document(d) = c {
                        out.extend(clauses(d));
                    }
                }
            }
            (k, v) => out.push((k, v)),
        }
    }
    out
}

/// The bounds of the index's non-geo fields from the filter: one value
/// under an equality, all of them with no predicate. `None` when a field
/// has another predicate, whose bounds this does not build.
fn trailing_bounds(index: &GeoIndex, filter: &Document) -> Option<Vec<FieldBounds>> {
    let cl = clauses(filter);
    let mut out = Vec::with_capacity(index.fields.len());
    for (at, field) in index.fields.iter().enumerate() {
        if at == index.geo_at {
            out.push(FieldBounds::Full);
            continue;
        }
        let preds: Vec<&Bson> = cl
            .iter()
            .filter(|(k, _)| k == field)
            .map(|(_, v)| *v)
            .collect();
        let bound = match preds.as_slice() {
            [] => FieldBounds::Full,
            [v] => {
                let value = match v {
                    Bson::Document(d) if d.keys().any(|k| k.starts_with('$')) => {
                        if d.len() == 1 {
                            d.get("$eq")?
                        } else {
                            return None;
                        }
                    }
                    other => other,
                };
                if matches!(
                    value,
                    Bson::Array(_) | Bson::Document(_) | Bson::RegularExpression(_) | Bson::Null
                ) {
                    return None;
                }
                FieldBounds::Point(secantus_core::sortkey::encode_value(value, None).ok()?)
            }
            _ => return None,
        };
        out.push(bound);
    }
    Some(out)
}

/// The geo predicate the filter puts on the index's field, as the bounds
/// of a scan of it.
fn predicate_bounds(index: &GeoIndex, filter: &Document) -> Option<FieldBounds> {
    clauses(filter).into_iter().find_map(|(field, v)| {
        if field != index.geo_field() {
            return None;
        }
        let Bson::Document(cond) = v else { return None };
        cond.iter().find_map(|(op, arg)| match op.as_str() {
            "$geoWithin" | "$within" => index.predicate_bounds("$geoWithin", arg),
            "$geoIntersects" => index.predicate_bounds("$geoIntersects", arg),
            _ => None,
        })
    })
}

/// Whether the filter has a geo predicate on `field` at all.
fn has_geo_predicate(filter: &Document, field: &str) -> bool {
    clauses(filter).into_iter().any(|(f, v)| {
        f == field
            && matches!(v, Bson::Document(d)
                if d.keys().any(|k| matches!(k.as_str(), "$geoWithin" | "$within" | "$geoIntersects")))
    })
}

/// A scan of a geo index: the index and the bounds of each of its fields.
pub(crate) struct Scan {
    index: GeoIndex,
    bounds: Vec<FieldBounds>,
    /// A scan of the whole index (a hint with no geo predicate to bound it):
    /// a document the index does not hold is not returned.
    selects: bool,
}

/// The geo index scan mongod answers this read with, if it is one.
/// `hinted` is a hint's geo index; `elsewhere` whether the read goes
/// another way (the planner found a non-geo index to read, or a hint names
/// one, or the collection).
pub(crate) fn scan(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    hinted: Option<GeoIndex>,
    elsewhere: bool,
) -> Result<Option<Scan>, StorageError> {
    if let Some(index) = hinted {
        let Some(mut bounds) = trailing_bounds(&index, filter) else {
            return Ok(None);
        };
        let geo = predicate_bounds(&index, filter);
        let selects = geo.is_none();
        bounds[index.geo_at] = geo.unwrap_or(FieldBounds::Full);
        return Ok(Some(Scan {
            index,
            bounds,
            selects,
        }));
    }
    if elsewhere {
        return Ok(None);
    }
    let mut candidates: Vec<GeoIndex> = geo_indexes(storage, db, coll)?
        .into_iter()
        .filter(|ix| ix.geo_at == 0 && has_geo_predicate(filter, ix.geo_field()))
        .collect();
    // The `2dsphere` index first, as mongod's trial picks it.
    candidates.sort_by_key(|ix| !ix.is_sphere());
    for index in candidates {
        let Some(geo) = predicate_bounds(&index, filter) else {
            continue;
        };
        let Some(mut bounds) = trailing_bounds(&index, filter) else {
            return Ok(None);
        };
        bounds[index.geo_at] = geo;
        return Ok(Some(Scan {
            index,
            bounds,
            selects: false,
        }));
    }
    Ok(None)
}

fn decode(blob: &[u8]) -> Option<Document> {
    secantus_core::bsondoc::from_bytes(blob).ok()
}

impl Scan {
    /// `rows` (in insertion order) in the scan's order. A document the scan
    /// does not reach comes after the rest, unless the scan is of the whole
    /// index, which does not hold it.
    pub(crate) fn order(&self, rows: Rows) -> Option<Rows> {
        let mut keys = Vec::with_capacity(rows.len());
        for (_, blob) in &rows {
            keys.push(self.index.keys(&decode(blob)?));
        }
        let order = secantus_core::geoorder::scan_order(&keys, &self.bounds);
        Some(arrange(rows, order, !self.selects))
    }
}

/// `rows` in `order` (indexes into it), then, when `keep_rest`, the rows
/// it leaves out, in their own order.
fn arrange(rows: Rows, order: Vec<usize>, keep_rest: bool) -> Rows {
    let mut slots: Vec<Option<(i64, Vec<u8>)>> = rows.into_iter().map(Some).collect();
    let mut out: Rows = order
        .into_iter()
        .filter_map(|i| slots.get_mut(i).and_then(Option::take))
        .collect();
    if keep_rest {
        out.extend(slots.into_iter().flatten());
    }
    out
}

/// The index a `$near` (or `$geoNear`) on `field` reads when no hint names
/// one: the `2d` index before a `2dsphere` one, as mongod's trial picks.
pub(crate) fn near_index(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    field: &str,
    near: &secantus_core::geoshape::Near,
) -> Result<Option<GeoIndex>, StorageError> {
    let mut usable: Vec<GeoIndex> = geo_indexes(storage, db, coll)?
        .into_iter()
        .filter(|ix| {
            ix.geo_field() == field
                && if ix.is_sphere() {
                    near.spherical
                } else {
                    !near.geojson
                }
        })
        .collect();
    usable.sort_by_key(|ix| ix.is_sphere());
    Ok(usable.into_iter().next())
}

/// The documents of the collection a `$near` search reads, decoded. `None`
/// past [`NEAR_COLLECTION`] documents.
fn collection(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
) -> Result<Option<Vec<(i64, Document)>>, StorageError> {
    let Some(all) = storage.collection_at_most(db, coll, NEAR_COLLECTION)? else {
        return Ok(None);
    };
    Ok(all
        .into_iter()
        .map(|(rid, blob)| decode(&blob).map(|d| (rid, d)))
        .collect())
}

/// `rows` (the documents a `$near` matched) in mongod's order, or `Err`
/// with them untouched when the search is not replayed here.
pub(crate) fn near_rows(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    index: &GeoIndex,
    near: &Found,
    rows: Rows,
) -> Result<Result<Rows, Rows>, StorageError> {
    let Some(base) = trailing_bounds(index, filter) else {
        return Ok(Err(rows));
    };
    let Some(all) = collection(storage, db, coll)? else {
        return Ok(Err(rows));
    };
    // The rest of the query is a filter over what the near stage returns
    // (the matched rows); the stage itself fetches every document its rings
    // reach, inside the bounds of the index's other fields.
    let docs: Vec<Document> = all.iter().map(|(_, d)| d.clone()).collect();
    let passes = vec![true; docs.len()];
    let Some(order) = near_order(index, &base, &near.near, &docs, &passes) else {
        return Ok(Err(rows));
    };
    let place: std::collections::HashMap<i64, usize> = order
        .iter()
        .enumerate()
        .map(|(at, &i)| (all[i].0, at))
        .collect();
    let (mut placed, rest_rows): (Rows, Rows) = rows
        .into_iter()
        .partition(|(rid, _)| place.contains_key(rid));
    placed.sort_by_key(|(rid, _)| place[rid]);
    if rest_rows.is_empty() {
        return Ok(Ok(placed));
    }
    // Rows the search did not return: kept, after the rest, nearest first.
    let rest_rows = near.nearest_first(rest_rows);
    placed.extend(rest_rows);
    Ok(Ok(placed))
}

/// The order `$geoNear` returns `docs` (the collection, in insertion
/// order) in: indexes into `docs`, or `None` when the search is not
/// replayed here. Its `query` is a filter over the search's output, apart
/// from an equality on a compound index's other field, which bounds it.
pub(crate) fn geo_near_order(
    index: &GeoIndex,
    near: &secantus_core::geoshape::Near,
    query: Option<&Document>,
    docs: &[Document],
) -> Option<Vec<usize>> {
    let passes = vec![true; docs.len()];
    if docs.len() > NEAR_COLLECTION {
        return None;
    }
    let empty = Document::new();
    let base = trailing_bounds(index, query.unwrap_or(&empty))?;
    near_order(index, &base, near, docs, &passes)
}
