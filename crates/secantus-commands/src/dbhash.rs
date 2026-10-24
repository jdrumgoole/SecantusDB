//! `dbHash`: an MD5 of each collection's documents and one of the whole
//! database, computed the way mongod computes them so the hashes of two
//! servers holding the same documents are EQUAL.
//!
//! Measured on mongod 8.2.11 (2026-10-14) by inserting the same documents
//! on both servers and comparing, collection by collection
//! (`tools/probes/admin_commands.py`):
//!
//! - a collection's hash is the MD5 of its documents' BSON, one after
//!   another, in `_id` order under the collection's default collation; a
//!   capped collection is hashed in natural (insertion) order instead;
//! - the database's hash is the MD5 of the collections' hex digests, one
//!   after another, in name order;
//! - views, `system.profile` and everything in `local` are left out (what
//!   mongod does not replicate it does not hash); `system.views` is hashed
//!   like any collection (here, from the definitions it is derived from);
//! - a time-series collection is hashed as `system.buckets.<name>`, from
//!   its bucket documents (here, the ones this server derives; their bytes
//!   equal mongod's only where the buckets do, RM-088);
//! - `collections` names the ones to hash: a name that is not a collection
//!   is ignored, a non-array `collections` is ignored, and an element that
//!   is not a string is `BadValue`. Any other field is ignored.

use std::collections::BTreeMap;
use std::time::Instant;

use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};
use md5::{Digest, Md5};

use crate::{CommandContext, CommandError, HandlerResult};

/// The machine's host name, as mongod reports it in `host`.
pub fn host_name() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: `buf` is a valid, writable buffer of the length passed.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(name) = std::str::from_utf8(&buf[..end]) {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "secantus".to_string())
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The names `collections` asks for, or `None` for every collection.
fn requested(doc: &Document) -> Result<Option<Vec<String>>, CommandError> {
    let Some(Bson::Array(names)) = doc.get("collections") else {
        return Ok(None);
    };
    if names.is_empty() {
        return Ok(None);
    }
    names
        .iter()
        .map(|v| match v {
            Bson::String(s) => Ok(s.clone()),
            _ => Err(CommandError::new(
                2,
                "BadValue",
                "collections entries have to be strings",
            )),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// `dbHash` on the context's database.
pub fn db_hash(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let started = Instant::now();
    let wanted = requested(doc)?;
    let storage = ctx.storage()?;
    let db = ctx.db_name.as_str();
    let port = ctx.server_address.as_ref().map_or(27017, |(_, p)| *p);

    let mut names: Vec<String> = if db == "local" {
        Vec::new()
    } else {
        storage
            .list_collections(db)
            .map_err(crate::util::command_error)?
    };
    names.retain(|name| name != "system.profile");
    names.sort();
    let asked = |name: &str| wanted.as_ref().is_none_or(|w| w.iter().any(|n| n == name));

    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut capped = Vec::new();
    let mut uuids = Document::new();
    let mut derived_views = false;
    for name in &names {
        let options = storage
            .get_collection_options(db, name)
            .map_err(crate::util::command_error)?;
        // A view is a definition in `system.views`, not documents; a
        // time-series collection is one too, over `system.buckets.<name>`,
        // whose bucket documents mongod hashes under that name (RM-026,
        // measured 8.2.11): here, the buckets `find` returns for it, in
        // `_id` order, under the collection's UUID.
        if options.contains_key("timeseries") {
            derived_views = true;
            let buckets = format!("system.buckets.{name}");
            if asked(&buckets) {
                let mut docs = crate::ts_buckets::of_collection(storage, db, name)?;
                docs.sort_by(|a, b| {
                    secantus_core::order::cmp(
                        a.get("_id").unwrap_or(&Bson::Null),
                        b.get("_id").unwrap_or(&Bson::Null),
                    )
                });
                let mut md5 = Md5::new();
                for d in &docs {
                    md5.update(bson::to_vec(d).map_err(|e| {
                        CommandError::new(8, "UnknownError", format!("dbHash: {e}"))
                    })?);
                }
                hashes.insert(buckets.clone(), hex(&md5.finalize()));
                let uuid = storage
                    .collection_uuid(db, name)
                    .map_err(crate::util::command_error)?;
                if !uuid.is_empty() {
                    uuids.insert(
                        buckets,
                        Binary {
                            subtype: BinarySubtype::Uuid,
                            bytes: uuid,
                        },
                    );
                }
            }
            continue;
        }
        if options.contains_key("viewOn") {
            derived_views = true;
            continue;
        }
        if !asked(name) {
            continue;
        }
        let is_capped = storage
            .collection_is_capped(db, name)
            .map_err(crate::util::command_error)?;
        let rows = if is_capped {
            capped.push(Bson::String(name.clone()));
            storage.find(db, name, &Document::new(), None, None)
        } else {
            let collation = options
                .get_document("collation")
                .ok()
                .and_then(secantus_core::collation::parse);
            storage.find_collated(
                db,
                name,
                &Document::new(),
                Some(&doc! {"_id": 1}),
                None,
                collation.as_ref(),
                &Document::new(),
            )
        }
        .map_err(crate::util::command_error)?;
        let mut md5 = Md5::new();
        for row in &rows {
            md5.update(row);
        }
        hashes.insert(name.clone(), hex(&md5.finalize()));
        let uuid = storage
            .collection_uuid(db, name)
            .map_err(crate::util::command_error)?;
        if !uuid.is_empty() {
            uuids.insert(
                name.clone(),
                Binary {
                    subtype: BinarySubtype::Uuid,
                    bytes: uuid,
                },
            );
        }
    }
    // `system.views` is what `listCollections` and `find` give for it: the
    // definitions, under the UUID both report, hashed in `_id` order.
    if derived_views && asked("system.views") && !hashes.contains_key("system.views") {
        let mut definitions = crate::ts_buckets::view_definitions(storage, db)?;
        definitions.sort_by(|a, b| {
            a.get_str("_id")
                .unwrap_or_default()
                .cmp(b.get_str("_id").unwrap_or_default())
        });
        let mut md5 = Md5::new();
        for definition in &definitions {
            md5.update(
                bson::to_vec(definition)
                    .map_err(|e| CommandError::new(8, "UnknownError", format!("dbHash: {e}")))?,
            );
        }
        hashes.insert("system.views".to_string(), hex(&md5.finalize()));
        uuids.insert(
            "system.views",
            crate::ts_buckets::derived_uuid(db.as_bytes(), 2),
        );
    }
    let mut whole = Md5::new();
    let mut collections = Document::new();
    for (name, digest) in &hashes {
        whole.update(digest.as_bytes());
        collections.insert(name.clone(), digest.clone());
    }
    let elapsed = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
    Ok(doc! {
        "host": format!("{}:{port}", host_name()),
        "collections": collections,
        "capped": capped,
        "uuids": uuids,
        "md5": hex(&whole.finalize()),
        "timeMillis": elapsed,
        "ok": 1.0,
    })
}
