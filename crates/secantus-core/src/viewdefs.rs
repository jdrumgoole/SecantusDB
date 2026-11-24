//! How mongod 8.2.11 records a view and a time-series collection on disk.
//!
//! A view is no collection there: it is one document of its database's
//! `system.views` collection, `{_id: "<db>.<name>", viewOn, pipeline,
//! collation?}`, and nothing else (no table, no `_mdb_catalog` row). A
//! time-series collection is the same kind of document, a view over its
//! `system.buckets.<name>` collection that unpacks the buckets, and the
//! buckets collection's catalog options carry the bucket validator and the
//! `timeseries` options. Measured on mongod 8.2.11 (storage-model plan,
//! phase 7): `system.views` after creating views with and without a
//! collation, a view on a view, and time-series collections with and
//! without a `metaField` and an `expireAfterSeconds`.

use bson::{doc, Bson, Document};

/// The name of a database's view catalog collection.
pub const SYSTEM_VIEWS: &str = "system.views";

/// The prefix of a time-series collection's buckets collection.
pub const BUCKETS_PREFIX: &str = "system.buckets.";

/// The stage mongod's view of a time-series collection with the options
/// `ts` unpacks its buckets with: `timeField`, `metaField` when there is
/// one, and `bucketMaxSpanSeconds`, in that order.
pub fn unpack_stage(ts: &Document) -> Document {
    let mut unpack = Document::new();
    for field in ["timeField", "metaField", "bucketMaxSpanSeconds"] {
        if let Some(v) = ts.get(field) {
            unpack.insert(field, v.clone());
        }
    }
    doc! { "$_internalUnpackBucket": unpack }
}

/// The `system.views` document of `db.name` when its stored options make it
/// a view (`viewOn`, `viewPipeline`, `collation`) or a time-series
/// collection (`timeseries`); `None` for any other collection.
pub fn view_definition(db: &str, name: &str, options: &Document) -> Option<Document> {
    let mut entry = doc! { "_id": format!("{db}.{name}") };
    if let Ok(view_on) = options.get_str("viewOn") {
        entry.insert("viewOn", view_on);
        entry.insert(
            "pipeline",
            options
                .get_array("viewPipeline")
                .cloned()
                .unwrap_or_default(),
        );
        if let Ok(collation) = options.get_document("collation") {
            entry.insert("collation", collation.clone());
        }
    } else if let Ok(ts) = options.get_document("timeseries") {
        entry.insert("viewOn", format!("{BUCKETS_PREFIX}{name}"));
        entry.insert("pipeline", vec![Bson::Document(unpack_stage(ts))]);
    } else {
        return None;
    }
    Some(entry)
}

/// The validator mongod gives `system.buckets.<name>` for a collection whose
/// time field is `time`.
pub fn buckets_validator(time: &str) -> Document {
    let bound = doc! {
        "bsonType": "object",
        "required": [time],
        "properties": { time: { "bsonType": "date" } },
    };
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "control", "data"],
        "properties": {
            "_id": { "bsonType": "objectId" },
            "control": {
                "bsonType": "object",
                "required": ["version", "min", "max"],
                "properties": {
                    "version": { "bsonType": "number" },
                    "min": bound.clone(),
                    "max": bound,
                    "closed": { "bsonType": "bool" },
                    "count": { "bsonType": "number", "minimum": 1 },
                },
                "additionalProperties": false,
            },
            "data": { "bsonType": "object" },
            "meta": {},
        },
        "additionalProperties": false,
    } }
}

/// The catalog options mongod 8.2.11 keeps for `system.buckets.<name>`, from
/// the buckets collection's own stored options (`own`, its `uuid` among
/// them) and the time-series collection's (`ts_options`): `uuid`,
/// `validator`, `clusteredIndex: true`, `expireAfterSeconds` when the
/// collection has one, `timeseries`, then whatever else `own` holds.
pub fn buckets_catalog_options(own: &Document, ts_options: &Document) -> Option<Document> {
    let ts = ts_options.get_document("timeseries").ok()?;
    let time = ts.get_str("timeField").ok()?;
    let mut out = Document::new();
    if let Some(uuid) = own.get("uuid") {
        out.insert("uuid", uuid.clone());
    }
    out.insert("validator", buckets_validator(time));
    out.insert("clusteredIndex", true);
    if let Some(expire) = ts_options.get("expireAfterSeconds") {
        out.insert("expireAfterSeconds", expire.clone());
    }
    out.insert("timeseries", ts.clone());
    for (k, v) in own {
        if !out.contains_key(k) && k != "clusteredIndex" {
            out.insert(k.clone(), v.clone());
        }
    }
    Some(out)
}

/// The time-series collection's own options from the catalog options
/// mongod keeps for its `system.buckets.<name>` (the inverse of
/// [`buckets_catalog_options`]): `(collection, buckets)`, the collection's
/// `expireAfterSeconds` and `timeseries`, and what the buckets collection
/// keeps of its own (`uuid`, `clusteredIndex: true` and any option mongod
/// does not derive from the collection's). `None` when `catalog` holds no
/// `timeseries`.
pub fn split_buckets_catalog_options(catalog: &Document) -> Option<(Document, Document)> {
    let ts = catalog.get_document("timeseries").ok()?;
    let mut collection = Document::new();
    if let Some(expire) = catalog.get("expireAfterSeconds") {
        collection.insert("expireAfterSeconds", expire.clone());
    }
    collection.insert("timeseries", ts.clone());
    let mut buckets = Document::new();
    if let Some(uuid) = catalog.get("uuid") {
        buckets.insert("uuid", uuid.clone());
    }
    buckets.insert("clusteredIndex", true);
    for (k, v) in catalog {
        if !matches!(
            k.as_str(),
            "uuid" | "validator" | "clusteredIndex" | "expireAfterSeconds" | "timeseries"
        ) {
            buckets.insert(k.clone(), v.clone());
        }
    }
    Some((collection, buckets))
}

/// The spec of an index of a time-series collection with the `timeseries`
/// options `ts`, from the spec mongod 8.2.11 keeps for it on the buckets
/// (`bucket_spec`): its `originalSpec` when it has one (a partial index),
/// else the spec with the key on the collection's fields -- `meta` is the
/// meta field, a bound pair `control.min.f` / `control.max.f` (reversed for
/// a descending key) is `f`, and a TTL index's `control.min.<time>` is the
/// time field. `None` for a key no collection key gives.
pub fn collection_index_spec(bucket_spec: &Document, ts: &Document) -> Option<Document> {
    if let Ok(original) = bucket_spec.get_document("originalSpec") {
        return Some(original.clone());
    }
    let key = bucket_spec.get_document("key").ok()?;
    let time = ts.get_str("timeField").ok()?;
    let meta = ts.get_str("metaField").ok();
    let mut out_key = Document::new();
    let ttl_field = format!("control.min.{time}");
    if bucket_spec.contains_key("expireAfterSeconds")
        && key.len() == 1
        && key.keys().next() == Some(&ttl_field)
    {
        out_key.insert(time, key.values().next()?.clone());
    } else {
        let mut fields = key.iter();
        while let Some((field, direction)) = fields.next() {
            if let Some(rest) = field.strip_prefix("meta") {
                if rest.is_empty() || rest.starts_with('.') {
                    out_key.insert(format!("{}{rest}", meta?), direction.clone());
                    continue;
                }
            }
            let descending = match direction {
                Bson::Int32(n) => *n < 0,
                Bson::Int64(n) => *n < 0,
                Bson::Double(d) => *d < 0.0,
                _ => return None,
            };
            let (first, second) = if descending {
                ("max", "min")
            } else {
                ("min", "max")
            };
            let name = field.strip_prefix(&format!("control.{first}.") as &str)?;
            let (pair, pair_direction) = fields.next()?;
            if pair.strip_prefix(&format!("control.{second}.") as &str) != Some(name)
                || pair_direction != direction
            {
                return None;
            }
            out_key.insert(name, direction.clone());
        }
    }
    let mut out = Document::new();
    for (k, v) in bucket_spec {
        if k == "key" {
            out.insert("key", out_key.clone());
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_are_mongods() {
        let view = doc! {"viewOn": "c", "viewPipeline": [{"$match": {"a": 1}}]};
        assert_eq!(
            view_definition("app", "v1", &view),
            Some(doc! {"_id": "app.v1", "viewOn": "c", "pipeline": [{"$match": {"a": 1}}]})
        );
        let ts = doc! {"timeseries": {"timeField": "t", "metaField": "m",
        "granularity": "seconds", "bucketMaxSpanSeconds": 3600}};
        assert_eq!(
            view_definition("app", "ts", &ts),
            Some(
                doc! {"_id": "app.ts", "viewOn": "system.buckets.ts", "pipeline": [
                {"$_internalUnpackBucket": {"timeField": "t", "metaField": "m",
                    "bucketMaxSpanSeconds": 3600}}]}
            )
        );
        assert_eq!(view_definition("app", "c", &doc! {}), None);
    }

    #[test]
    fn buckets_options_split_back() {
        let own = doc! {"uuid": 1, "clusteredIndex": true};
        let ts = doc! {"expireAfterSeconds": 60_i64, "timeseries": {"timeField": "t"}};
        let catalog = buckets_catalog_options(&own, &ts).unwrap();
        assert_eq!(split_buckets_catalog_options(&catalog), Some((ts, own)));
    }

    #[test]
    fn index_specs_on_the_collection() {
        let ts = doc! {"timeField": "t", "metaField": "m"};
        let spec = doc! {"v": 2, "key": {"meta": 1, "control.min.t": 1, "control.max.t": 1},
        "name": "m_1_t_1"};
        assert_eq!(
            collection_index_spec(&spec, &ts),
            Some(doc! {"v": 2, "key": {"m": 1, "t": 1}, "name": "m_1_t_1"})
        );
        let desc = doc! {"key": {"meta.a": -1, "control.max.v": -1, "control.min.v": -1}};
        assert_eq!(
            collection_index_spec(&desc, &ts),
            Some(doc! {"key": {"m.a": -1, "v": -1}})
        );
        let ttl = doc! {"key": {"control.min.t": 1}, "expireAfterSeconds": 5};
        assert_eq!(
            collection_index_spec(&ttl, &ts),
            Some(doc! {"key": {"t": 1}, "expireAfterSeconds": 5})
        );
        assert_eq!(
            collection_index_spec(&doc! {"key": {"control.min.t": 1}}, &ts),
            None
        );
    }

    #[test]
    fn buckets_options_in_mongods_order() {
        let own = doc! {"clusteredIndex": true, "uuid": 1};
        let ts = doc! {"expireAfterSeconds": 60_i64, "timeseries": {"timeField": "t"}};
        let out = buckets_catalog_options(&own, &ts).unwrap();
        assert_eq!(
            out.keys().collect::<Vec<_>>(),
            [
                "uuid",
                "validator",
                "clusteredIndex",
                "expireAfterSeconds",
                "timeseries"
            ]
        );
    }
}
