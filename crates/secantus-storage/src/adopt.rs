//! This server's registry brought in line with mongod's catalog as a store
//! opens: `_mdb_catalog` and each database's `system.views` are the
//! catalog (storage-model plan, phase 7c).
//!
//! The registry (`secantus_collections`, `secantus_idents`,
//! `secantus_indexes`) is what this server's transactional machinery reads;
//! it writes `_mdb_catalog` beside it in the same transaction, so after any
//! crash of this server the two agree. They disagree only when another
//! server wrote the store: a store mongod made (no registry at all), or one
//! of this server's that mongod opened and wrote to since (a collection or
//! an index made, dropped or changed; a document made an index multikey).
//! Then the catalog wins, as it is what mongod and this server both read:
//! a namespace the catalog lists gets its registry row, placement and index
//! rows from it (an index whose spec is unchanged keeps the stamps this
//! server keeps for it), and one it no longer lists loses them. Before
//! that write, what this server cannot serve is refused by name
//! ([`crate::store_format::catalog_findings`]). Measured on mongod 8.2.11
//! (2026-11-22) before this was built: mongod's writes to a store this
//! server had written were lost when it opened the store again -- a
//! collection mongod made had its tables dropped as orphans, and one mongod
//! dropped was served from its drop-pending table.

use std::collections::{BTreeMap, HashMap, HashSet};

use bson::{Bson, Document};
use secantus_wt::Session;

use crate::mdb_catalog::{self, Placement};
use crate::{decode_doc, encode_doc, Result, COLL_TABLE, IDX_TABLE, ID_INDEX_NAME, OPLOG_NS};

/// One `_mdb_catalog` row.
struct CatalogRow {
    id: i64,
    db: String,
    coll: String,
    entry: Document,
}

/// Who wrote the store last, as the open finds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LastWriter {
    /// A store mongod made: this server has no registry for it.
    Mongod,
    /// This server's store, closed cleanly by it, with nothing written
    /// since: its registry is the catalog.
    ThisServer,
    /// This server's store, which another server opened after this one
    /// closed it cleanly (the oplog has moved past the close's marker).
    AnotherServer,
    /// This server's store, which it did not close cleanly: its registry
    /// and catalog agree unless another server wrote it since the crash,
    /// which only their idents can show.
    Unknown,
}

/// What [`reconcile`] changed.
#[derive(Debug, Default)]
pub(crate) struct Reconciled {
    /// Namespaces whose registry rows were written from the catalog.
    pub adopted: Vec<String>,
    /// Namespaces the catalog no longer lists, whose rows went.
    pub removed: Vec<String>,
}

fn split_ns(ns: &str) -> Option<(String, String)> {
    ns.split_once('.')
        .map(|(d, c)| (d.to_string(), c.to_string()))
}

fn catalog_rows(session: &Session) -> Result<Vec<CatalogRow>> {
    let cur = session.open_cursor(mdb_catalog::CATALOG_TABLE, None)?;
    let mut out = Vec::new();
    while cur.next()? {
        let id = cur.get_key_q()?;
        let entry = decode_doc(&cur.get_value_u()?)?;
        let Some((db, coll)) = entry.get_str("ns").ok().and_then(split_ns) else {
            continue;
        };
        out.push(CatalogRow {
            id,
            db,
            coll,
            entry,
        });
    }
    Ok(out)
}

/// The placement a catalog row describes: its ident, its RecordId, its
/// indexes' idents (`idxIdent`, the `_id_` index's included), clustered
/// when its options say so.
fn placement_of(row: &CatalogRow) -> Placement {
    let mut indexes = BTreeMap::new();
    if let Ok(idx) = row.entry.get_document("idxIdent") {
        for (name, ident) in idx {
            if let Some(ident) = ident.as_str() {
                indexes.insert(name.clone(), ident.to_string());
            }
        }
    }
    let clustered = row
        .entry
        .get_document("md")
        .and_then(|md| md.get_document("options"))
        .is_ok_and(|o| o.contains_key("clusteredIndex"));
    Placement {
        ident: row.entry.get_str("ident").unwrap_or("").to_string(),
        catalog_id: row.id,
        indexes,
        clustered,
    }
}

/// Whether the registry's placements and the catalog's rows name the same
/// tables: false once another server made, dropped or renamed a collection
/// or an index. Exact after any crash of this server, which writes both in
/// one transaction.
fn idents_agree(rows: &[CatalogRow], placements: &HashMap<(String, String), Placement>) -> bool {
    let listed = rows
        .iter()
        .filter(|r| format!("{}.{}", r.db, r.coll) != OPLOG_NS)
        .count();
    listed == placements.len()
        && rows
            .iter()
            .filter(|r| format!("{}.{}", r.db, r.coll) != OPLOG_NS)
            .all(|r| placements.get(&(r.db.clone(), r.coll.clone())) == Some(&placement_of(r)))
}

/// This server's index rows of `(db, coll)`: name -> payload.
fn index_rows(session: &Session, db: &str, coll: &str) -> Result<Vec<(String, Document)>> {
    let cur = session.open_cursor(IDX_TABLE, None)?;
    cur.set_key_sss(db, coll, "");
    let mut more = match cur.search_near() {
        Ok(cmp) if cmp < 0 => cur.next()?,
        Ok(_) => true,
        Err(e) if e.is_not_found() => false,
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    while more {
        let (d, c, name) = cur.get_key_sss()?;
        if d != db || c != coll {
            break;
        }
        out.push((name, decode_doc(&cur.get_value_u()?)?));
        more = cur.next()?;
    }
    Ok(out)
}

/// The spec an index row lists in `_mdb_catalog` (what `sync_catalog_entry`
/// writes): its fields in the order the row recorded.
fn listed_spec(payload: &Document, name: &str) -> Document {
    crate::index_event_spec(payload, name)
}

/// Whether two specs are the same, field order included: `Document`'s own
/// equality ignores it, and the order is part of what the catalog keeps.
fn same_spec(a: &Document, b: &Document) -> bool {
    a == b && a.keys().eq(b.keys())
}

/// mongod's `multikeyPaths` (per key field, a byte per path component, 1
/// where the component is an array) as this server keeps them: per field,
/// the array prefixes.
fn prefixes_of(paths: &Document) -> Document {
    let mut out = Document::new();
    for (field, bytes) in paths {
        let Bson::Binary(b) = bytes else { continue };
        let parts: Vec<&str> = field.split('.').collect();
        let prefixes: Vec<Bson> = b
            .bytes
            .iter()
            .enumerate()
            .filter(|(i, flag)| **flag != 0 && *i < parts.len())
            .map(|(i, _)| Bson::String(parts[..=i].join(".")))
            .collect();
        if !prefixes.is_empty() {
            out.insert(field.clone(), prefixes);
        }
    }
    out
}

/// The union of two `multikeyPaths` documents of this server's form.
fn union_paths(ours: &Document, theirs: &Document) -> Document {
    let mut out = ours.clone();
    for (field, add) in theirs {
        let mut have: Vec<Bson> = out.get_array(field).cloned().unwrap_or_default();
        for p in add.as_array().into_iter().flatten() {
            if !have.contains(p) {
                have.push(p.clone());
            }
        }
        out.insert(field.clone(), have);
    }
    out
}

/// The registry row of a catalog index (`md.indexes[i]`): its key, its
/// options as the spec lists them, its place in its collection's build
/// order, and what this server records beside them (the collation its keys
/// are made under, whether it is multikey and at which paths).
fn index_payload(ix: &Document, build_seq: i64) -> Option<(String, Document)> {
    let spec = ix.get_document("spec").ok()?;
    let name = spec.get_str("name").ok()?.to_string();
    let key = spec.get_document("key").ok()?.clone();
    let mut options = Document::new();
    for (k, v) in spec {
        if !matches!(k.as_str(), "v" | "key" | "name") {
            options.insert(k.clone(), v.clone());
        }
    }
    options.insert(crate::BUILD_SEQ, build_seq);
    if options
        .get_document("collation")
        .ok()
        .and_then(secantus_core::collation::parse)
        .is_some()
    {
        options.insert(crate::COLLATION_KEYS, secantus_core::collation::KEY_FORMAT);
    }
    if ix.get_bool("multikey").unwrap_or(false) {
        options.insert("multikey", true);
    }
    if let Ok(paths) = ix.get_document("multikeyPaths") {
        let prefixes = prefixes_of(paths);
        if !prefixes.is_empty() {
            options.insert(crate::MULTIKEY_PATHS, prefixes);
        }
    }
    // The spec's field order, as mongod wrote it.
    let order: Vec<Bson> = spec.keys().map(|k| Bson::String(k.clone())).collect();
    Some((
        name,
        bson::doc! {"key": key, "options": options, "specOrder": order},
    ))
}

fn put(
    session: &Session,
    table: &str,
    key: (&str, &str, Option<&str>),
    doc: &Document,
) -> Result<()> {
    let blob = encode_doc(doc)?;
    let cur = session.open_cursor(table, None)?;
    match key.2 {
        Some(name) => cur.set_key_sss(key.0, key.1, name),
        None => cur.set_key_ss(key.0, key.1),
    }
    cur.set_value_u(&blob);
    cur.insert()?;
    Ok(())
}

fn remove(session: &Session, table: &str, key: (&str, &str, Option<&str>)) -> Result<()> {
    let cur = session.open_cursor(table, None)?;
    match key.2 {
        Some(name) => cur.set_key_sss(key.0, key.1, name),
        None => cur.set_key_ss(key.0, key.1),
    }
    match cur.remove() {
        Ok(()) => Ok(()),
        Err(e) if e.is_not_found() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Write `(db, coll)`'s registry from its catalog row: its options, its
/// placement, and an index row for each of its indexes but `_id_` (which
/// its placement names). An index row whose listed spec is the catalog's
/// keeps what this server recorded for it, with the catalog's multikey
/// paths added; one the catalog lists otherwise, or not at all, is
/// replaced or goes.
fn adopt_row(session: &Session, row: &CatalogRow) -> Result<bool> {
    let (db, coll) = (row.db.as_str(), row.coll.as_str());
    let md = row.entry.get_document("md").cloned().unwrap_or_default();
    let mut options = md.get_document("options").cloned().unwrap_or_default();
    // A time-series collection's buckets: the catalog's options are derived
    // from the collection's (`buckets_catalog_options`); the registry keeps
    // the buckets' own.
    if coll.starts_with(secantus_core::viewdefs::BUCKETS_PREFIX) {
        if let Some((_, own)) = secantus_core::viewdefs::split_buckets_catalog_options(&options) {
            options = own;
        }
    }
    let mut changed = false;
    if crate::coll_options(session, db, coll)?.as_ref() != Some(&options) {
        put(session, COLL_TABLE, (db, coll, None), &options)?;
        changed = true;
    }
    let placement = placement_of(row);
    if mdb_catalog::read_placement(session, db, coll)?.as_ref() != Some(&placement) {
        mdb_catalog::write_placement(session, db, coll, &placement)?;
        changed = true;
    }
    let ours: HashMap<String, Document> = index_rows(session, db, coll)?.into_iter().collect();
    // An index the catalog adds was built after every one this server
    // knows (mongod appends to `md.indexes`): it is listed after them.
    let built_before = ours
        .values()
        .map(|p| crate::build_seq(&p.get_document("options").cloned().unwrap_or_default()))
        .max()
        .unwrap_or(0);
    let mut listed: HashSet<String> = HashSet::new();
    let mut seq = 0i64;
    for ix in md.get_array("indexes").into_iter().flatten() {
        let Some(ix) = ix.as_document() else { continue };
        let Some(name) = ix
            .get_document("spec")
            .ok()
            .and_then(|s| s.get_str("name").ok())
        else {
            continue;
        };
        if name == ID_INDEX_NAME {
            continue;
        }
        seq += 1;
        listed.insert(name.to_string());
        let spec = ix.get_document("spec").cloned().unwrap_or_default();
        let theirs_paths = ix
            .get_document("multikeyPaths")
            .map(prefixes_of)
            .unwrap_or_default();
        let theirs_multikey = ix.get_bool("multikey").unwrap_or(false);
        let want = match ours.get(name) {
            Some(payload) if same_spec(&listed_spec(payload, name), &spec) => {
                // Ours: keep its stamps, and what mongod learnt since.
                let mut payload = payload.clone();
                let mut opts = payload.get_document("options").cloned().unwrap_or_default();
                if theirs_multikey && !opts.get_bool("multikey").unwrap_or(false) {
                    opts.insert("multikey", true);
                }
                let paths = opts
                    .get_document(crate::MULTIKEY_PATHS)
                    .cloned()
                    .unwrap_or_default();
                let merged = union_paths(&paths, &theirs_paths);
                if !merged.is_empty() {
                    opts.insert(crate::MULTIKEY_PATHS, merged);
                }
                payload.insert("options", opts);
                payload
            }
            _ => match index_payload(ix, built_before + seq) {
                Some((_, payload)) => payload,
                None => continue,
            },
        };
        if ours.get(name) != Some(&want) {
            put(session, IDX_TABLE, (db, coll, Some(name)), &want)?;
            changed = true;
        }
    }
    for name in ours.keys() {
        if !listed.contains(name) {
            remove(session, IDX_TABLE, (db, coll, Some(name)))?;
            changed = true;
        }
    }
    Ok(changed)
}

/// The registry rows of the time-series collection whose buckets `row` is
/// (`system.buckets.<name>`): its row, with the options the buckets' catalog
/// options carry for it, and the indexes listed on it, one per index of the
/// buckets, keyed on its own fields (as `createIndexes` on it writes them).
/// An index it no longer has goes; one whose spec is unchanged keeps the
/// stamps this server keeps for it. Whether it changed anything.
fn adopt_time_series(session: &Session, row: &CatalogRow) -> Result<bool> {
    let Some(name) = row
        .coll
        .strip_prefix(secantus_core::viewdefs::BUCKETS_PREFIX)
    else {
        return Ok(false);
    };
    let md = row.entry.get_document("md").cloned().unwrap_or_default();
    let catalog = md.get_document("options").cloned().unwrap_or_default();
    let Some((options, _)) = secantus_core::viewdefs::split_buckets_catalog_options(&catalog)
    else {
        return Ok(false);
    };
    let ts = options
        .get_document("timeseries")
        .cloned()
        .unwrap_or_default();
    let db = row.db.as_str();
    let mut changed = false;
    let current = crate::coll_options(session, db, name)?;
    // What this server keeps beside the definition (the uuid it minted).
    let mut want = current.clone().unwrap_or_default();
    want.remove("expireAfterSeconds");
    want.remove("timeseries");
    for (k, v) in options {
        want.insert(k, v);
    }
    if current.as_ref() != Some(&want) {
        put(session, COLL_TABLE, (db, name, None), &want)?;
        changed = true;
    }
    let ours: HashMap<String, Document> = index_rows(session, db, name)?.into_iter().collect();
    let built_before = ours
        .values()
        .map(|p| crate::build_seq(&p.get_document("options").cloned().unwrap_or_default()))
        .max()
        .unwrap_or(0);
    let mut listed: HashSet<String> = HashSet::new();
    let mut seq = 0i64;
    for ix in md.get_array("indexes").into_iter().flatten() {
        let Some(spec) = ix.as_document().and_then(|ix| ix.get_document("spec").ok()) else {
            continue;
        };
        if spec.get_str("name") == Ok(ID_INDEX_NAME) {
            continue;
        }
        // Refused by name before this runs when there is none.
        let Some(on_collection) = secantus_core::viewdefs::collection_index_spec(spec, &ts) else {
            continue;
        };
        let Ok(ix_name) = on_collection.get_str("name").map(str::to_string) else {
            continue;
        };
        seq += 1;
        listed.insert(ix_name.clone());
        let want = match ours.get(&ix_name) {
            Some(payload) if listed_spec(payload, &ix_name) == on_collection => payload.clone(),
            _ => match index_payload(&bson::doc! {"spec": on_collection}, built_before + seq) {
                Some((_, payload)) => payload,
                None => continue,
            },
        };
        if ours.get(&ix_name) != Some(&want) {
            put(session, IDX_TABLE, (db, name, Some(&ix_name)), &want)?;
            changed = true;
        }
    }
    for ix_name in ours.keys() {
        if !listed.contains(ix_name) {
            remove(session, IDX_TABLE, (db, name, Some(ix_name)))?;
            changed = true;
        }
    }
    Ok(changed)
}

/// Remove `(db, coll)` from the registry: its row, placement and index rows.
fn forget(session: &Session, db: &str, coll: &str) -> Result<()> {
    for (name, _) in index_rows(session, db, coll)? {
        remove(session, IDX_TABLE, (db, coll, Some(&name)))?;
    }
    mdb_catalog::remove_placement(session, db, coll)?;
    remove(session, COLL_TABLE, (db, coll, None))
}

/// Every registry row: `(db, coll) -> options`.
fn registry_rows(session: &Session) -> Result<Vec<((String, String), Document)>> {
    let cur = session.open_cursor(COLL_TABLE, None)?;
    let mut out = Vec::new();
    while cur.next()? {
        let (db, coll) = cur.get_key_ss()?;
        let blob = cur.get_value_u()?;
        let opts = if blob.is_empty() {
            Document::new()
        } else {
            decode_doc(&blob)?
        };
        out.push(((db, coll), opts));
    }
    Ok(out)
}

/// The documents of a placed collection's table.
fn table_docs(session: &Session, ident: &str) -> Result<Vec<Document>> {
    let cur = session.open_cursor(&mdb_catalog::table_uri(ident), None)?;
    let mut out = Vec::new();
    while cur.next()? {
        out.push(decode_doc(&cur.get_value_u()?)?);
    }
    Ok(out)
}

/// The registry's view rows from each database's `system.views`, the
/// catalog mongod keeps them in: `{_id: "<db>.<name>", viewOn, pipeline,
/// collation?}` is a row `{viewOn, viewPipeline, collation?}`; a view row
/// no document names goes. (A time-series collection's document is left
/// to [`adopt_time_series`].)
fn adopt_views(session: &Session, rows: &[CatalogRow]) -> Result<(Vec<String>, Vec<String>)> {
    let mut want: HashMap<(String, String), Document> = HashMap::new();
    for row in rows
        .iter()
        .filter(|r| r.coll == secantus_core::viewdefs::SYSTEM_VIEWS)
    {
        let Ok(ident) = row.entry.get_str("ident") else {
            continue;
        };
        for view in table_docs(session, ident)? {
            let Some((db, name)) = view.get_str("_id").ok().and_then(split_ns) else {
                continue;
            };
            if db != row.db {
                continue;
            }
            // A time-series collection's row is its buckets' to write
            // (`adopt_time_series`).
            if view
                .get_str("viewOn")
                .is_ok_and(|on| on.starts_with(secantus_core::viewdefs::BUCKETS_PREFIX))
            {
                continue;
            }
            let mut opts = Document::new();
            if let Ok(on) = view.get_str("viewOn") {
                opts.insert("viewOn", on);
            }
            opts.insert(
                "viewPipeline",
                view.get_array("pipeline").cloned().unwrap_or_default(),
            );
            if let Ok(c) = view.get_document("collation") {
                opts.insert("collation", c.clone());
            }
            want.insert((db, name), opts);
        }
    }
    let (mut adopted, mut removed) = (Vec::new(), Vec::new());
    for ((db, coll), opts) in registry_rows(session)? {
        if !opts.contains_key("viewOn") {
            continue;
        }
        match want.remove(&(db.clone(), coll.clone())) {
            Some(w) => {
                let mut kept = opts.clone();
                for k in ["viewOn", "viewPipeline", "collation"] {
                    kept.remove(k);
                }
                for (k, v) in w {
                    kept.insert(k, v);
                }
                if kept != opts {
                    put(session, COLL_TABLE, (&db, &coll, None), &kept)?;
                    adopted.push(format!("{db}.{coll}"));
                }
            }
            None => {
                remove(session, COLL_TABLE, (&db, &coll, None))?;
                removed.push(format!("{db}.{coll}"));
            }
        }
    }
    for ((db, coll), opts) in want {
        put(session, COLL_TABLE, (&db, &coll, None), &opts)?;
        adopted.push(format!("{db}.{coll}"));
    }
    Ok((adopted, removed))
}

/// Bring the registry in line with the catalog, in `session`, before the
/// store reads anything through it (see the module's documentation).
/// `last` is who the open finds wrote the store last. Nothing is read or
/// written for a store this server closed cleanly; for one it did not, the
/// idents are compared, and the rest follows only if they disagree.
/// What this server cannot serve is refused by name first (`refuse` builds
/// the error from the findings), so a refused store is left as it was.
pub(crate) fn reconcile(
    session: &Session,
    last: LastWriter,
    timestamped: bool,
    refuse: impl FnOnce(&[String]) -> crate::StorageError,
) -> Result<Reconciled> {
    if last == LastWriter::ThisServer {
        return Ok(Reconciled::default());
    }
    let rows = catalog_rows(session)?;
    let placements: HashMap<(String, String), Placement> = mdb_catalog::all_placements(session)?
        .into_iter()
        .map(|(d, c, p)| ((d, c), p))
        .collect();
    if last == LastWriter::Unknown && idents_agree(&rows, &placements) {
        return Ok(Reconciled::default());
    }
    let findings = crate::store_format::catalog_findings(session)?;
    if !findings.is_empty() {
        return Err(refuse(&findings));
    }
    let mut out = Reconciled::default();
    crate::begin_untimestamped(session, timestamped)?;
    let wrote = (|| -> Result<()> {
        let mut listed: HashSet<(String, String)> = HashSet::new();
        for row in rows
            .iter()
            .filter(|r| format!("{}.{}", r.db, r.coll) != OPLOG_NS)
        {
            listed.insert((row.db.clone(), row.coll.clone()));
            if adopt_row(session, row)? {
                out.adopted.push(format!("{}.{}", row.db, row.coll));
            }
            if adopt_time_series(session, row)? {
                let name = row
                    .coll
                    .trim_start_matches(secantus_core::viewdefs::BUCKETS_PREFIX);
                out.adopted.push(format!("{}.{name}", row.db));
            }
        }
        for (db, coll) in placements.keys() {
            if !listed.contains(&(db.clone(), coll.clone())) {
                forget(session, db, coll)?;
                out.removed.push(format!("{db}.{coll}"));
            }
        }
        // A time-series collection whose buckets the catalog no longer
        // lists was dropped with them.
        for ((db, coll), opts) in registry_rows(session)? {
            let buckets = format!("{}{coll}", secantus_core::viewdefs::BUCKETS_PREFIX);
            if opts.contains_key("timeseries") && !listed.contains(&(db.clone(), buckets)) {
                forget(session, &db, &coll)?;
                out.removed.push(format!("{db}.{coll}"));
            }
        }
        let (views, gone) = adopt_views(session, &rows)?;
        out.adopted.extend(views);
        out.removed.extend(gone);
        Ok(())
    })();
    match wrote {
        Ok(()) => crate::commit_untimestamped(session)?,
        Err(e) => {
            let _ = session.rollback_transaction(None);
            return Err(e);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multikey_path_bytes_become_prefixes() {
        let paths = bson::doc! {
            "a.b": Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::Generic,
                bytes: vec![0, 1],
            }),
            "c": Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::Generic,
                bytes: vec![0],
            }),
        };
        assert_eq!(prefixes_of(&paths), bson::doc! {"a.b": ["a.b"]});
        let ours = bson::doc! {"a.b": ["a"]};
        assert_eq!(
            union_paths(&ours, &prefixes_of(&paths)),
            bson::doc! {"a.b": ["a", "a.b"]}
        );
    }
}
