//! mongod's catalog and table names: `_mdb_catalog`, and each collection's
//! own WiredTiger table, named by an *ident*.
//!
//! mongod 8.2.11 keeps one WiredTiger table per collection, named
//! `collection-<UUID>` (a random UUID, not the collection's), and lists every
//! collection in `_mdb_catalog`, a table keyed by a RecordId whose values are
//! `{md: {ns, options, indexes}, idxIdent, ident, ns}` (measured in
//! `tasks/mongod-storage-model-plan.md`, phase 0b; the full record is
//! `tests/fixtures/storage_model/mongod-8.2.11-layout.json`).
//!
//! This server keeps its own registry (`secantus_collections`,
//! `secantus_indexes`), which its transactional machinery is built on, and
//! writes `_mdb_catalog` beside it, in the same WiredTiger transaction, at
//! every registry write ([`crate::Storage`]'s `sync_catalog_entry`): so the
//! catalog mongod reads always says what the registry says, crash or no
//! crash. Which ident a collection's documents live in is kept in
//! [`IDENTS_TABLE`], keyed by namespace, so a lookup is one search.

use bson::{doc, Bson, Document};
use secantus_wt::Session;

use crate::{decode_doc, encode_doc, new_uuid_bytes, Result};

/// mongod's catalog table.
pub const CATALOG_TABLE: &str = "table:_mdb_catalog";

/// `(db, coll) -> {ident, catalogId}`: where a collection's documents live
/// and which `_mdb_catalog` row lists it. This server's own table; mongod
/// ignores it.
pub const IDENTS_TABLE: &str = "table:secantus_idents";

/// The configuration mongod 8.2.11 creates a collection's table (and
/// `_mdb_catalog`) with: a RecordId key, the BSON document as the value,
/// snappy, and `app_metadata=(formatVersion=1)` (phase 0b). Logging is the
/// store's data-table mode, applied by the caller (mongod's replica-set
/// model, unlogged, is phase 4b of the plan).
pub const COLLECTION_TABLE_CFG: &str = "type=file,memory_page_max=10m,split_pct=90,\
     leaf_value_max=64MB,checksum=on,block_compressor=snappy,key_format=q,\
     value_format=u,app_metadata=(formatVersion=1)";

/// A collection's ident: `collection-<UUID>`, as mongod 8.2 names them.
pub fn new_collection_ident() -> String {
    format!("collection-{}", uuid_string(&new_uuid_bytes()))
}

/// The WiredTiger URI of an ident's table.
pub fn table_uri(ident: &str) -> String {
    format!("table:{ident}")
}

fn uuid_string(b: &[u8; 16]) -> String {
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Where a collection lives: its ident and its `_mdb_catalog` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub ident: String,
    pub catalog_id: i64,
}

impl Placement {
    pub fn table(&self) -> String {
        table_uri(&self.ident)
    }

    fn to_doc(&self) -> Document {
        doc! {"ident": &self.ident, "catalogId": self.catalog_id}
    }

    fn from_doc(d: &Document) -> Option<Placement> {
        Some(Placement {
            ident: d.get_str("ident").ok()?.to_string(),
            catalog_id: d.get_i64("catalogId").ok()?,
        })
    }
}

/// The placement recorded for `(db, coll)`, read in `session`'s snapshot.
pub fn read_placement(session: &Session, db: &str, coll: &str) -> Result<Option<Placement>> {
    let cur = match session.open_cursor(IDENTS_TABLE, None) {
        Ok(c) => c,
        Err(e) if e.is_not_found() => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    cur.set_key_ss(db, coll);
    match cur.search() {
        Ok(()) => Ok(Placement::from_doc(&decode_doc(&cur.get_value_u()?)?)),
        Err(e) if e.is_not_found() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn write_placement(session: &Session, db: &str, coll: &str, p: &Placement) -> Result<()> {
    let blob = encode_doc(&p.to_doc())?;
    let cur = session.open_cursor(IDENTS_TABLE, None)?;
    cur.set_key_ss(db, coll);
    cur.set_value_u(&blob);
    cur.insert()?;
    Ok(())
}

pub fn remove_placement(session: &Session, db: &str, coll: &str) -> Result<()> {
    let cur = session.open_cursor(IDENTS_TABLE, None)?;
    cur.set_key_ss(db, coll);
    match cur.remove() {
        Ok(()) => Ok(()),
        Err(e) if e.is_not_found() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Every placement, `(db, coll, placement)`.
pub fn all_placements(session: &Session) -> Result<Vec<(String, String, Placement)>> {
    let cur = session.open_cursor(IDENTS_TABLE, None)?;
    let mut out = Vec::new();
    while cur.next()? {
        let (db, coll) = cur.get_key_ss()?;
        if let Some(p) = Placement::from_doc(&decode_doc(&cur.get_value_u()?)?) {
            out.push((db, coll, p));
        }
    }
    Ok(out)
}

/// The highest `_mdb_catalog` RecordId in use (0 for an empty catalog):
/// the next row takes one more.
pub fn max_catalog_id(session: &Session) -> Result<i64> {
    let cur = session.open_cursor(CATALOG_TABLE, None)?;
    if cur.prev()? {
        Ok(cur.get_key_q()?)
    } else {
        Ok(0)
    }
}

/// Write `_mdb_catalog` row `id`.
pub fn write_entry(session: &Session, id: i64, entry: &Document) -> Result<()> {
    let blob = encode_doc(entry)?;
    let cur = session.open_cursor(CATALOG_TABLE, None)?;
    cur.set_key_q(id);
    cur.set_value_u(&blob);
    cur.insert()?;
    Ok(())
}

/// Remove `_mdb_catalog` row `id`, if it is there.
pub fn remove_entry(session: &Session, id: i64) -> Result<()> {
    let cur = session.open_cursor(CATALOG_TABLE, None)?;
    cur.set_key_q(id);
    match cur.remove() {
        Ok(()) => Ok(()),
        Err(e) if e.is_not_found() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Read `_mdb_catalog` row `id`.
pub fn read_entry(session: &Session, id: i64) -> Result<Option<Document>> {
    let cur = session.open_cursor(CATALOG_TABLE, None)?;
    cur.set_key_q(id);
    match cur.search() {
        Ok(()) => Ok(Some(decode_doc(&cur.get_value_u()?)?)),
        Err(e) if e.is_not_found() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// One index as mongod lists it in `md.indexes`: `{spec, ready, multikey,
/// multikeyPaths}`. `multikey_paths` holds, per key field, one byte per
/// path component, 1 where that component is an array (mongod's
/// `MultikeyPaths` as `binData`).
pub struct CatalogIndex {
    pub spec: Document,
    pub ready: bool,
    pub multikey: bool,
    pub multikey_paths: Vec<(String, Vec<u8>)>,
}

/// A collection's `_mdb_catalog` value, field for field as mongod 8.2.11
/// writes it: `{md: {ns, options, indexes}, idxIdent, ident, ns}`.
pub fn entry_doc(
    ns: &str,
    options: Document,
    indexes: &[CatalogIndex],
    ident: &str,
    idx_idents: &[(String, String)],
) -> Document {
    let indexes: Vec<Bson> = indexes
        .iter()
        .map(|ix| {
            let mut paths = Document::new();
            for (field, bytes) in &ix.multikey_paths {
                paths.insert(
                    field.clone(),
                    Bson::Binary(bson::Binary {
                        subtype: bson::spec::BinarySubtype::Generic,
                        bytes: bytes.clone(),
                    }),
                );
            }
            Bson::Document(doc! {
                "spec": ix.spec.clone(),
                "ready": ix.ready,
                "multikey": ix.multikey,
                "multikeyPaths": paths,
            })
        })
        .collect();
    let mut idx_ident = Document::new();
    for (name, ident) in idx_idents {
        idx_ident.insert(name.clone(), ident.clone());
    }
    doc! {
        "md": {"ns": ns, "options": options, "indexes": indexes},
        "idxIdent": idx_ident,
        "ident": ident,
        "ns": ns,
    }
}

/// The per-field multikey path bytes mongod records for an index's key
/// pattern: one byte per dotted component of each field, 1 where the
/// component held an array. `array_components` names the components (by
/// field, then position) this server saw arrays at.
pub fn multikey_paths(
    key: &Document,
    array_components: &[(String, usize)],
) -> Vec<(String, Vec<u8>)> {
    key.keys()
        .map(|field| {
            let mut bytes = vec![0u8; field.split('.').count()];
            for (f, i) in array_components {
                if f == field && *i < bytes.len() {
                    bytes[*i] = 1;
                }
            }
            (field.clone(), bytes)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idents_are_named_as_mongod_names_them() {
        let ident = new_collection_ident();
        assert!(ident.starts_with("collection-"));
        let uuid = &ident["collection-".len()..];
        let parts: Vec<_> = uuid.split('-').map(str::len).collect();
        assert_eq!(parts, [8, 4, 4, 4, 12]);
        assert_ne!(ident, new_collection_ident());
    }

    #[test]
    fn an_entry_has_mongods_fields_in_mongods_order() {
        let e = entry_doc(
            "app.c",
            doc! {"uuid": 1},
            &[CatalogIndex {
                spec: doc! {"v": 2, "key": {"_id": 1}, "name": "_id_"},
                ready: true,
                multikey: false,
                multikey_paths: multikey_paths(&doc! {"_id": 1}, &[]),
            }],
            "collection-x",
            &[("_id_".into(), "index-y".into())],
        );
        assert_eq!(
            e.keys().collect::<Vec<_>>(),
            ["md", "idxIdent", "ident", "ns"]
        );
        let md = e.get_document("md").unwrap();
        assert_eq!(md.keys().collect::<Vec<_>>(), ["ns", "options", "indexes"]);
        let ix = md.get_array("indexes").unwrap()[0].as_document().unwrap();
        assert_eq!(
            ix.keys().collect::<Vec<_>>(),
            ["spec", "ready", "multikey", "multikeyPaths"]
        );
    }

    #[test]
    fn multikey_paths_mark_the_array_components() {
        let key = doc! {"a.b": 1, "c": 1};
        assert_eq!(
            multikey_paths(&key, &[("a.b".into(), 1)]),
            vec![("a.b".to_string(), vec![0, 1]), ("c".to_string(), vec![0])]
        );
    }
}
