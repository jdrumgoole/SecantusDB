//! Commands that name `system.buckets.<name>` directly.
//!
//! Reads of the bucket namespace go through the view machinery (the storage
//! adapter presents it as a view whose one stage derives the buckets, see
//! [`crate::ts_buckets`]). The commands here are the ones a view would
//! refuse and `mongod` does not: listing and building indexes, dropping the
//! buckets, and writing a bucket by hand. Shapes were read from `mongod`
//! 8.2.11.

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
        "insert" => Some(insert(doc, ctx, &layout)),
        "update" | "delete" | "findAndModify" | "findandmodify" => Some(no_bucket_writes(&ns)),
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
                                 show: this server keeps its measurements, not its buckets"
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
            if reply.get_f64("ok").ok() != Some(1.0) {
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

/// `mongod` takes an update or delete of a bucket document. This server has
/// no bucket to change.
fn no_bucket_writes(ns: &str) -> Document {
    CommandError::new(
        115,
        "CommandNotSupported",
        format!(
            "{ns} cannot be written directly: this server keeps a time-series collection's \
             measurements, not its buckets"
        ),
    )
    .into_reply()
}

/// An insert of bucket documents by hand. Each is checked against the
/// validator `mongod` gives the bucket namespace, which is where a document
/// that is not a bucket is refused; one that passes it is refused too, since
/// there is nowhere to keep it.
fn insert(doc: &Document, ctx: &CommandContext, layout: &Layout) -> Document {
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
    let mut errors: Vec<Document> = Vec::new();
    // A validator no interrupt reaches (RM-313).
    let _opaque = secantus_core::deadline::opaque();
    for (index, d) in documents.iter().enumerate() {
        let is_bucket =
            secantus_core::query::matches(d, &validator, &Document::new(), None).unwrap_or(true);
        errors.push(if is_bucket {
            doc! {
                "index": index as i32,
                "code": 115_i32,
                "errmsg": "a bucket cannot be inserted directly: this server keeps a \
                           time-series collection's measurements, not its buckets",
            }
        } else {
            doc! {
                "index": index as i32,
                "code": 121_i32,
                "errmsg": "Document failed validation",
                "errInfo": crate::crud::validation_error_info(&validator, d),
            }
        });
        if ordered {
            break;
        }
    }
    doc! { "n": 0_i32, "writeErrors": errors, "ok": 1.0 }
}
