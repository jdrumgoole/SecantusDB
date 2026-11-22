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
