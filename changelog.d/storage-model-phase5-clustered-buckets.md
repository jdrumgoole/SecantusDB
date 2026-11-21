### A document is the table's whole value, and a clustered collection is keyed by its `_id`

A collection's table now holds each document as its value, with nothing
around it, as mongod's does. Until now every row was `[id_key length][id_key]
[BSON]`, the key being there for the undo log and the PostgreSQL read overlay;
both now derive it from the document's `_id` when they write, so a scan no
longer strips a prefix.

A clustered collection (one made with `clusteredIndex`, the buckets of a
time-series collection, and `config.system.preimages`) is now laid out as
mongod lays it out. Its table is keyed by the KeyString of `_id` (no TypeBits
and no end byte: `_id: 1` is `2b02`, `"s"` is `3c7300`, an ObjectId is `64`
and its twelve bytes), `key_format=u`, and it has no `_id_` table: the row's
key is the `_id`. A secondary index entry on it ends in those key bytes and
their length, as mongod's does (`x: 1` of `_id: 1` is `2b0204 2b02 02`). The
buckets table is `block_compressor=zstd`, mongod's choice for
`system.buckets.*`. `showRecordId` on a clustered collection returns the key
bytes as BinData, as mongod 8.2.11 does. Its `create` oplog entry carries the
`clusteredIndex` and no `idIndex`.

Pre-images are read by `{nsUUID, ts, applyOpsIndex}` in key order, and the
expiry walk stops at the first unexpired pre-image of a collection and goes on
at the next collection's, as mongod's per-collection truncate markers do.

**This changes the on-disk format again** (store format 7): a store written
before it is refused at open.

#### Changed

- `secantus-core`: `RecordId`, a counter or key bytes, in place of `i64`
  wherever a collection's rows are named: the `Storage` trait's scans,
  `records_by_id`, `index_entries_from` and `record_ids_of`, the explain
  stream and `$recordId`. Capped and oplog positions stay counters.
- `secantus-storage`: the frame is gone (`frame_doc_value`,
  `unframe_doc_value`, the time-series key suffix); the placement records
  whether a collection is clustered; `_id` lookups of a clustered collection
  read its own table; collection `indexSizes` leaves out the clustering key.

#### Tests

- `crates/secantus-storage/tests/clustered.rs`: the keys against what mongod
  8.2.11 wrote, the absence of `_id_`, zstd buckets, duplicate `_id`,
  update and delete, and a secondary index's entry bytes.
