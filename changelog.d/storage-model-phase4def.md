### The `_id` index is a table of its own, and uniqueness lives in the index tables

The `_id_` index is now a WiredTiger table of its own, `index-<UUID>`, made as
mongod 8.2.11 makes it (`formatVersion=8`): an entry's key is the KeyString of
`_id` alone and its value is the document's RecordId followed by the TypeBits,
byte for byte what mongod wrote in the phase 0 fixture. It replaces the
`_id -> RecordId` table every collection shared. A collection with a default
collation keys its `_id` by it, so an `_id` the collation calls equal to
another is a duplicate, and a document `_id` is compared by value (`{a: 1}`
and `{a: 1.0}` are one key), with no claim of any kind beside the index.

A unique index checks for a duplicate as mongod does for an index with the
RecordId in its key: it inserts the KeyString of the value alone with
`overwrite=false` (so WiredTiger itself refuses it to a concurrent writer of
the value), removes it again in the same transaction, and looks for an entry
that starts with it under another RecordId. The table of unique-key claims is
gone, and with it the claim take-over, the incarnation names a transaction's
new collection kept its rows under, and the copy a rename made of them: a
rename moves the `_id` and index tables with the collection and writes
nothing.

**This changes the on-disk format again** (store format 4): a store written
before it is refused at open.

#### Changed

- `secantus-storage`: `_id_` in the placement and in `_mdb_catalog`'s
  `idxIdent`, created with the collection (not for `system.profile` or a
  time-series collection, which keeps measurements that repeat an `_id`).
  `find_by_id`, the `_id` point and range lookups, deletes, capped eviction,
  `validate`, repair, compaction and the size counts read and write it.
  `idIndexSize` is the `_id_` entries' key bytes.
- `index_entries_from` reads `_id_` like any other index (`IndexPos::Values`
  and `Resume`; `IndexPos::Raw` is gone), and the explain stream treats it so.
- The legacy readers and upgrade paths for formats a store can no longer be
  in are removed: the number-key and `_id`-key re-keying, the entry-format
  refusal and the rebuild of indexes written under older rules, and the walks
  that took the `_id` claims of stored documents.

#### Tests

- `crates/secantus-storage/tests/id_index_table.rs`: the `_id_` tables of
  mongod's fixture rebuilt row by row, key and value; a collated `_id`; a
  rename and a drop; no `secantus_natural_seq` or `secantus_unique_keys`.
- `crates/secantus-storage/tests/unique_in_index_tables.rs`: a unique, a
  `prepareUnique`, a partial and a collated unique index refuse a duplicate
  with mongod's error, and two transactions cannot both commit one value.
