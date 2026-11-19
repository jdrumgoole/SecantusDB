### Each secondary index in a table of its own, keyed by mongod's KeyString

Every index other than `_id_` now lives in a WiredTiger table of its own,
named as mongod names them (`index-<UUID>`), made with mongod 8.2.11's index
configuration. An entry's key is the KeyString of the index key's values
followed by the document's RecordId, and its value is the TypeBits, exactly
as mongod writes them. Until now every index shared one table, keyed by this
server's own sort-key bytes. Dropping an index or its collection drops its
table instead of deleting its entries one at a time, and renaming a
collection moves its index tables with it.

**This changes the on-disk format again** (store format 3): a store written
before it is refused at open.

#### Changed

- `secantus-storage`: one `index-<UUID>` table per secondary index, listed in
  the collection's placement and in `_mdb_catalog`'s `idxIdent`; a table an
  index build, a rollback or a crash leaves behind is dropped (once nothing
  reads it) or at the next open. A read through an index dropped after the
  planner chose it falls back to a collection scan.
- Which keys a document contributes (multikey elements, sparse and partial
  indexes, collation) is unchanged; only their encoding is. A string passes
  through the index's collation sort key, as mongod's `StringTransformFn`
  does, and a descending field's bytes are inverted.
- Index bounds are KeyString bytes: a value end is the field's own bytes, a
  type bracket the KeyString type byte. The explain stream reads entries
  decoded from their KeyStrings and resumes after an entry by its key.
- Dropped tables are dropped with `force`, so a session that only holds a
  cached cursor on one no longer keeps it from going.
- `$listCatalog`'s `idxIdent` and `pg_relation_filepath` name an index's own
  table. The `_id_` index is still the `_id -> RecordId` map until it
  becomes an index table of its own.

#### Tests

- `crates/secantus-storage/tests/index_tables.rs`: each index kind's entries
  against `secantus_core::keystring`, the catalog's `idxIdent`, and what a
  drop, a rename, a rollback and an open leave behind. The tests that read
  the shared entries table now read the index's own.
