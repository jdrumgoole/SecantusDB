//! Commands that name `system.buckets.<name>` directly.
//!
//! The bucket namespace is a real collection (`ts_store`); most commands
//! read and write it as any other. The ones handled here are those whose
//! answer involves the time-series collection: its indexes, which are kept
//! on the collection and listed here in bucket form, dropping the buckets
//! (which drops the collection), and writing a bucket by hand, which the
//! bucket catalog has to hear about. Shapes were read from `mongod` 8.2.11.

use crate::util::ReplyOk;
use bson::{doc, Bson, Document};

use crate::ts_buckets::{self, Layout};
use crate::{dispatch, CommandContext, CommandError};

/// The reply to `name` when it targets the bucket namespace of a time-series
/// collection and is one of the commands handled here.
pub(crate) fn intercept(name: &str, doc: &Document, ctx: &mut CommandContext) -> Option<Document> {
    let handled = matches!(
        name,
        "insert"
            | "update"
            | "delete"
            | "findAndModify"
            | "findandmodify"
            | "listIndexes"
            | "createIndexes"
            | "dropIndexes"
            | "deleteIndexes"
            | "drop"
    );
    if !handled {
        return None;
    }
    let buckets = doc.get_str(name).ok()?.to_string();
    let storage = ctx.storage.clone()?;
    let db = ctx.db_name.clone();
    let (base, options) = ts_buckets::bucketed(storage.as_ref(), &db, &buckets)?;
    let layout = Layout::of(&options)?;
    let ns = format!("{db}.{buckets}");
    match name {
        "insert" => insert(doc, ctx, &layout, &buckets),
        // A write to a bucket by hand: the catalog forgets the buckets it
        // may change (mongod's `directWriteStart` clears them), and the
        // write runs on the buckets as on any collection.
        "update" | "delete" | "findAndModify" | "findandmodify" => {
            forget_written(storage.as_ref(), &db, &buckets, name, doc);
            None
        }
        "listIndexes" => {
            let listed: Vec<Bson> = match bucket_indexes(storage.as_ref(), &db, &base, &layout) {
                Ok(indexes) => indexes.into_iter().map(Bson::Document).collect(),
                Err(e) => return Some(e.into_reply()),
            };
            Some(doc! {
                "cursor": { "id": 0_i64, "ns": ns, "firstBatch": listed },
                "ok": 1.0,
            })
        }
        "createIndexes" => {
            let mut specs = Vec::new();
            for spec in doc.get_array("indexes").ok()?.iter() {
                let mut spec = spec.as_document()?.clone();
                let key = spec.get_document("key").ok()?;
                let Some(key) = ts_buckets::measurement_key(key, &layout) else {
                    return Some(
                        CommandError::new(
                            115,
                            "CommandNotSupported",
                            format!(
                                "an index on {ns} must be one the time-series collection can \
                                 show: this server keeps a time-series collection's indexes \
                                 on the collection"
                            ),
                        )
                        .into_reply(),
                    );
                };
                spec.insert("key", key);
                specs.push(Bson::Document(spec));
            }
            let mut redirected = doc.clone();
            redirected.insert(name, base);
            redirected.insert("indexes", specs);
            Some(dispatch(&redirected, ctx))
        }
        "dropIndexes" | "deleteIndexes" => {
            let mut redirected = doc.clone();
            redirected.insert(name, base);
            Some(dispatch(&redirected, ctx))
        }
        // The buckets are the collection: dropping them drops it.
        _ => {
            let indexes = storage
                .list_indexes(&db, &base)
                .map(|ix| {
                    ix.iter()
                        .filter(|i| i.get_str("name") != Ok("_id_"))
                        .count()
                })
                .unwrap_or(0);
            let mut redirected = doc.clone();
            redirected.insert(name, base);
            let reply = dispatch(&redirected, ctx);
            if reply.ok_number().ok() != Some(1.0) {
                return Some(reply);
            }
            Some(doc! { "nIndexesWas": indexes as i32, "ok": 1.0 })
        }
    }
}

/// The indexes of `system.buckets.<base>` as `listIndexes` lists them: the
/// time-series collection's own, each keyed as mongod keys it on the
/// buckets, and no `_id` index.
pub(crate) fn bucket_indexes(
    storage: &dyn crate::storage::Storage,
    db: &str,
    base: &str,
    layout: &Layout,
) -> Result<Vec<Document>, CommandError> {
    let indexes = storage
        .list_indexes(db, base)
        .map_err(crate::util::command_error)?;
    Ok(indexes
        .into_iter()
        .filter(|ix| ix.get_str("name") != Ok("_id_"))
        .map(|mut ix| {
            if let Ok(key) = ix.get_document("key") {
                let key = ts_buckets::bucket_key(key, layout);
                ix.insert("key", key);
            }
            for internal in crate::admin::INDEX_CATALOG_ONLY {
                ix.remove(*internal);
            }
            ix
        })
        .collect())
}

/// Forget, in the bucket catalog, every bucket a write's filters match.
fn forget_written(
    storage: &dyn crate::storage::Storage,
    db: &str,
    buckets: &str,
    name: &str,
    doc: &Document,
) {
    let Some(catalog) = storage.bucket_catalog() else {
        return;
    };
    let Ok(uuid) = storage.collection_uuid(db, buckets) else {
        return;
    };
    let filters: Vec<Document> = match name {
        "update" | "delete" => {
            let field = if name == "update" {
                "updates"
            } else {
                "deletes"
            };
            doc.get_array(field)
                .map(|a| {
                    a.iter()
                        .filter_map(Bson::as_document)
                        .map(|s| s.get_document("q").cloned().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => vec![doc.get_document("query").cloned().unwrap_or_default()],
    };
    for filter in filters {
        match storage.find(db, buckets, &filter, None, None) {
            Ok(rows) => {
                for raw in rows {
                    if let Ok(d) = secantus_core::bsondoc::from_bytes(&raw[..]) {
                        if let Some(Bson::ObjectId(id)) = d.get("_id") {
                            catalog.clear_bucket(&uuid, id);
                        }
                    }
                }
            }
            // A filter the store cannot read: forget them all.
            Err(_) => catalog.clear(&uuid),
        }
    }
}

/// An insert of bucket documents by hand. Each is checked against the
/// validator `mongod` gives the bucket namespace; when every one passes,
/// the insert runs on the buckets as on any collection (`None`).
fn insert(
    doc: &Document,
    ctx: &CommandContext,
    layout: &Layout,
    buckets: &str,
) -> Option<Document> {
    let documents: Vec<Document> = match &ctx.raw_insert_documents {
        Some(raw) => raw
            .iter()
            .filter_map(|b| secantus_core::bsondoc::from_bytes(&b[..]).ok())
            .collect(),
        None => doc
            .get_array("documents")
            .map(|a| a.iter().filter_map(Bson::as_document).cloned().collect())
            .unwrap_or_default(),
    };
    let ordered = !matches!(doc.get("ordered"), Some(Bson::Boolean(false)));
    let validator = ts_buckets::validator(&layout.time);
    // A validator no interrupt reaches (RM-313).
    let _opaque = secantus_core::deadline::opaque();
    let passes: Vec<bool> = documents
        .iter()
        .map(|d| {
            secantus_core::query::matches(d, &validator, &Document::new(), None).unwrap_or(true)
        })
        .collect();
    if passes.iter().all(|p| *p) {
        return None;
    }
    let storage = ctx.storage.clone()?;
    let mut errors: Vec<Document> = Vec::new();
    let mut kept: Vec<Vec<u8>> = Vec::new();
    let mut kept_index: Vec<usize> = Vec::new();
    for (index, d) in documents.iter().enumerate() {
        if passes[index] {
            kept.push(crate::bsondoc::to_vec(d).ok()?);
            kept_index.push(index);
            continue;
        }
        errors.push(doc! {
            "index": index as i32,
            "code": 121_i32,
            "errmsg": "Document failed validation",
            "errInfo": crate::crud::validation_error_info(&validator, d),
        });
        if ordered {
            break;
        }
    }
    let mut n = 0_i32;
    if !kept.is_empty() {
        match storage.insert(&ctx.db_name, buckets, kept, ordered) {
            Ok((inserted, failed)) => {
                n = inserted as i32;
                for mut e in failed {
                    if let Some(local) = e.get("index").and_then(crate::util::as_i64) {
                        let orig = kept_index.get(local as usize).copied().unwrap_or(0);
                        e.insert("index", orig as i32);
                    }
                    errors.push(e);
                }
            }
            Err(e) => return Some(crate::util::command_error(e).into_reply()),
        }
    }
    errors.sort_by_key(|e| e.get_i32("index").unwrap_or(0));
    Some(doc! { "n": n, "writeErrors": errors, "ok": 1.0 })
}
