### Rust server: the oplog stops re-encoding documents it already has

Every write on the Rust server appends an oplog entry so that change streams,
`local.oplog.rs`, and point-in-time recovery can replay it — a cost a bare
standalone `mongod` (which keeps no oplog) never pays. Profiling the write path
showed that entry was surprisingly expensive: it was **26% of an insert, a third
of a delete, and nearly half of an `update_many`**. The surprise was *where* the
cost lived — not in the WiredTiger write of the entry (which is essentially free
once the change is already in the batch's transaction) but in **building and
BSON-encoding the entry document on the CPU**. An insert's oplog entry carries the
whole inserted document in its `o` field, and the server was serialising that
document a second time even though it had just encoded the identical bytes for the
collection table.

The oplog now assembles each CRUD entry as raw BSON and splices the document body
straight through: an insert's `o` reuses the stored blob, a replacement update's
`o` reuses the `new` blob already computed for the collection write, and the small
`o2` / diff pieces are the only bytes encoded fresh. The change-stream diff walk
also stopped deep-cloning both the pre- and post-image just to compare them. The
stored entries are byte-for-byte the same, so change streams, pre-images, PITR
replay, and the oplog view are unchanged — the whole oplog and change-stream test
suites pass untouched. On an unloaded machine this moved single-writer **inserts to
mongod parity (1.17× → ~1.0× of `mongod`)** and **deletes from 1.43× to ~1.23×**,
with a smaller gain on updates (whose oplog cost is dominated by the inherent
`$v:2` diff computation). A couple of read-scan allocations were trimmed on the
same pass (a full `find({})` no longer re-runs a per-document match that always
succeeds, and the scan no longer clones each row's `_id`-key only to discard it).

#### Changed
- `Storage::emit_oplog` (Rust) takes an `OplogEntry` that is either an owned
  `Document` (the rare DDL / noop / `findAndModify` paths, encoded as before) or a
  pre-assembled raw `RawDocumentBuf` (the hot `insert` / `update` / `delete` /
  capped-eviction paths). The raw builder writes `op` / `ns` / `ui` / `o` / `o2` in
  mongod field order and splices the pre-encoded `o` / `o2` bytes, so the document
  body is never re-serialised; `ts` and `wall` are appended last, matching the
  historical byte layout.
- `secantus_core::diff::compute_update_description` walks the pre-/post-images
  directly (`walk_docs`) instead of wrapping each in an owned `Bson::Document`
  clone.
- `Storage::find_matching` short-circuits an empty filter (`find({})`) instead of
  running a foregone `RawDocument::from_bytes` + `matches_raw` per document, and the
  read-only collection scan (`scan_blobs_natural`) reuses each value's allocation
  rather than cloning the blob a second time.
