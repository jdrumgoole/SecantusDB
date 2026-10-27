//! What other commands do about an index build in progress (RM-187).
//!
//! A build over documents holds no collection lock while it scans, so the
//! commands that would reshape its collection meet it before they take
//! their own locks, as mongod's do (8.2.11, measured 2026-10-26):
//!
//! - `drop` and `dropDatabase` abort it ("Collection <ns>(<uuid>) is being
//!   dropped" / "dropDatabase command", 276 `IndexBuildAborted`), wait for
//!   the abort, then drop; the dropped collection's `nIndexesWas` counts the
//!   aborted build's indexes.
//! - `dropIndexes` naming every index of a build ("*", their names, a key)
//!   aborts it ("dropIndexes command") and counts them dropped; naming only
//!   some of a build's indexes is refused with 27 "can't drop unfinished
//!   index with name: <name>" and the build goes on.
//! - `collMod`, `renameCollection` and `convertToCapped` are refused with
//!   12587 `BackgroundOperationInProgressForNamespace`.

use std::cell::RefCell;

use bson::{Bson, Document};

use crate::storage::{IndexBuildAbort, IndexBuildInfo};
use crate::{CommandContext, CommandError};

thread_local! {
    /// The building indexes the running command aborted before its locks:
    /// `(name, key)`, for the command's own reply.
    static ABORTED: RefCell<Vec<(String, Document)>> = const { RefCell::new(Vec::new()) };
}

/// The building indexes the running command aborted, taken.
pub(crate) fn take_aborted() -> Vec<(String, Document)> {
    ABORTED.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

fn uuid_text(bytes: &[u8]) -> String {
    <[u8; 16]>::try_from(bytes)
        .map(|b| bson::uuid::Uuid::from_bytes(b).to_string())
        .unwrap_or_default()
}

/// Run before `doc` takes its locks: abort the builds it ends, or refuse it
/// while one runs. `Some` is the reply it gets instead of running.
pub(crate) fn before_locks(doc: &Document, ctx: &CommandContext) -> Option<Document> {
    ABORTED.with(|a| a.borrow_mut().clear());
    let name = doc.keys().next()?.as_str();
    if !matches!(
        name,
        "drop"
            | "dropDatabase"
            | "dropIndexes"
            | "deleteIndexes"
            | "collMod"
            | "renameCollection"
            | "convertToCapped"
    ) {
        return None;
    }
    let storage = ctx.storage.as_ref()?;
    // Nothing building anywhere: the common case costs one look.
    if storage.index_builds(None, None).is_empty() {
        return None;
    }
    let db = ctx.db_name.as_str();
    match name {
        "dropDatabase" => {
            let asked = storage.request_index_build_abort(
                db,
                None,
                None,
                &IndexBuildAbort::new(276, "IndexBuildAborted", "dropDatabase command"),
            );
            storage.wait_index_builds_gone(&asked);
            None
        }
        "drop" => {
            let coll = doc.get_str(name).ok()?;
            let builds = storage.index_builds(Some(db), Some(coll));
            if builds.is_empty() {
                return None;
            }
            let uuid = storage
                .collection_uuid(db, coll)
                .map(|u| uuid_text(&u))
                .unwrap_or_default();
            let asked = storage.request_index_build_abort(
                db,
                Some(coll),
                None,
                &IndexBuildAbort::new(
                    276,
                    "IndexBuildAborted",
                    format!("Collection {db}.{coll}({uuid}) is being dropped"),
                ),
            );
            note_aborted(&builds);
            storage.wait_index_builds_gone(&asked);
            None
        }
        "dropIndexes" | "deleteIndexes" => {
            let coll = doc.get_str(name).ok()?;
            let builds = storage.index_builds(Some(db), Some(coll));
            if builds.is_empty() {
                return None;
            }
            let named = |ix: &Document, wanted: &Bson| match wanted {
                Bson::String(s) => s == "*" || ix.get_str("name") == Ok(s.as_str()),
                Bson::Document(k) => ix.get_document("key").is_ok_and(|key| key == k),
                Bson::Array(names) => names
                    .iter()
                    .any(|n| n.as_str().is_some_and(|n| ix.get_str("name") == Ok(n))),
                _ => false,
            };
            let wanted = doc.get("index")?;
            let mut ending = Vec::new();
            for b in &builds {
                let hit: Vec<&Document> = b.indexes.iter().filter(|ix| named(ix, wanted)).collect();
                if hit.is_empty() {
                    continue;
                }
                if hit.len() < b.indexes.len() {
                    let first = hit[0].get_str("name").unwrap_or_default();
                    return Some(
                        CommandError::new(
                            27,
                            "IndexNotFound",
                            format!("can't drop unfinished index with name: {first}"),
                        )
                        .into_reply(),
                    );
                }
                ending.push(b.clone());
            }
            let cause = IndexBuildAbort::new(276, "IndexBuildAborted", "dropIndexes command");
            let mut asked = Vec::new();
            for b in &ending {
                asked.extend(storage.request_index_build_abort(
                    db,
                    Some(coll),
                    Some(&b.uuid),
                    &cause,
                ));
            }
            note_aborted(&ending);
            storage.wait_index_builds_gone(&asked);
            None
        }
        _ => {
            // collMod / convertToCapped name the collection; a rename names
            // both of its namespaces in full.
            let namespaces: Vec<(String, String)> = if name == "renameCollection" {
                ["renameCollection", "to"]
                    .iter()
                    .filter_map(|k| doc.get_str(k).ok())
                    .filter_map(|ns| {
                        let (d, c) = ns.split_once('.')?;
                        Some((d.to_string(), c.to_string()))
                    })
                    .collect()
            } else {
                vec![(db.to_string(), doc.get_str(name).ok()?.to_string())]
            };
            for (d, c) in namespaces {
                if let Some(b) = storage.index_builds(Some(&d), Some(&c)).first() {
                    let uuid = storage
                        .collection_uuid(&d, &c)
                        .map(|u| uuid_text(&u))
                        .unwrap_or_default();
                    return Some(
                        CommandError::new(
                            12587,
                            "BackgroundOperationInProgressForNamespace",
                            format!(
                                "cannot perform operation: an index build is currently running \
                                 for collection with UUID: {uuid}. Found index build: {}",
                                uuid_text(&b.uuid)
                            ),
                        )
                        .into_reply(),
                    );
                }
            }
            None
        }
    }
}

fn note_aborted(builds: &[IndexBuildInfo]) {
    ABORTED.with(|a| {
        let mut a = a.borrow_mut();
        for b in builds {
            for ix in &b.indexes {
                a.push((
                    ix.get_str("name").unwrap_or_default().to_string(),
                    ix.get_document("key").cloned().unwrap_or_default(),
                ));
            }
        }
    });
}
