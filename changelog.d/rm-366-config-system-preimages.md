### Change stream pre-images live in config.system.preimages

The Rust MongoDB server now keeps change stream pre-images where mongod 8.2
keeps them: as documents of a clustered `config.system.preimages`
collection, created as the server steps up, with mongod's shape —
`{_id: {nsUUID, ts, applyOpsIndex}, operationTime, preImage}` — so a client
can read, count and remove them as it would on mongod. A transaction's
pre-images share the time of its one `applyOps` oplog entry and number
their place in it, as mongod's do. `fullDocumentBeforeChange` and the
post-images built from them are read from the collection by the `_id` the
oplog entry names.

They expire as mongod expires them: with the oplog entry of their write,
and, once `changeStreamOptions.preAndPostImages.expireAfterSeconds` is set
through `setClusterParameter`, after that many seconds. On a store whose
data tables are not logged (how `secantusd-rs` creates one), crash recovery
records the pre-images of the writes it replays from the documents as they
stood before each write, as mongod's oplog application does. A store
written by an earlier build has its pre-images moved into the collection
the first time it opens.

#### Fixed

- `secantus-storage`: pre-images are written to `config.system.preimages`
  in the write's own transaction (the synchronous oplog) or before the
  drainer writes the entry (the asynchronous oplog), never to the former
  `table:secantus_preimages`, which an open now migrates and empties. Entries
  of an `applyOps` group carry their pre-image's slot so a read finds it
  directly. Pruning removes the pre-images of pruned entries;
  `set_pre_image_expire_after_seconds` adds mongod's age limit; crash
  recovery rebuilds what the last stable checkpoint did not hold. The
  PostgreSQL server links this crate; it records no pre-images.
- `secantus-commands`: a set member creates `config.system.preimages`
  (`{clusteredIndex: true}`, unreplicated) at startup; `listIndexes` lists
  the legacy clustered form's `_id_` index as mongod does;
  `setClusterParameter` (and startup) hand `changeStreamOptions`' expiry to
  the storage.
- Fixes RM-366 (tests/backlog/test_rm_found_2026_10_28.py::test_pre_images_are_kept_in_config_system_preimages,
  ::test_config_system_preimages_is_listed_as_mongod_lists_it,
  ::test_a_transaction_s_pre_images_share_its_applyops_entry_s_time;
  crates/secantus-storage/tests/pre_images.rs).
- Filed RM-389: a `deleteMany` over a pre-image collection is batched into
  one `applyOps`, where mongod writes a `d` entry per document.
