### mongod 8.2 opens a store SecantusDB wrote

A data directory `secantusd-mdb` wrote and stopped cleanly is now one mongod
8.2.11 starts on as the replica set's one member (under the store's set name
and member address, as it would its own), steps up to primary, and validates:
`validate({full: true})` passes on every collection of the storage plan's
reference scenario (collections and indexes of every kind this server keys
as mongod does, capped, clustered and time-series collections, pre-images, a
view, a drop, a rename and a transaction), every document and every index
walk reads back as SecantusDB served it, and a change stream resumes on
mongod from a resume token SecantusDB issued. Reaching it took four changes
to the store, each found by starting mongod on our files and reading its log.

A view, and a time-series collection, is now what it is on mongod: a document
of its database's `system.views` collection, with no table and no catalog
entry of its own; a time-series collection's buckets carry mongod's catalog
options (the bucket validator and the `timeseries` options). A multikey index
holds mongod's keys only, without the whole-array entry it also held, and
records its multikey paths when it is built as well as when it is written.
A clean stop clears `oplogTruncateAfterPoint`, as mongod's does, and a new
store's feature-compatibility version and set configuration are in its first
checkpoint, as mongod's initiation leaves them.

**This changes the on-disk format again** (store format 9): a store written
before it is refused at open. Opening a store mongod wrote is still refused,
by name; it is the next step.

#### Changed

- `secantus-storage`: a view or time-series namespace keeps its registry row
  but gets no placement, table or `_mdb_catalog` row; `system.views` is kept
  in step with the registry at each view DDL and at every open
  (`sync_system_views`); a buckets collection's catalog entry has mongod's
  options and `timeseriesBucketsMayHaveMixedSchemaData: false`.
- `secantus-storage`: index entries are mongod's keys alone; an equality to
  an array reads the array's and its first element's entries (an empty
  array's `undefined`), the points mongod's bounds name.
- `secantus-storage`: the catalog's `multikeyPaths` are recorded by index
  builds; a `2d`, `hashed`, text or wildcard index's catalog entry has no
  `multikeyPaths` and mongod's `multikey` flag.
- `secantus-storage`: `Storage::close` clears
  `local.replset.oplogTruncateAfterPoint`; the store's initiation writes the
  feature-compatibility version before its first checkpoint.
- `secantus-commands`: the replica-set configuration a new store records is
  made durable by a checkpoint, as `replSetInitiate`'s is.
- `secantus-core`: `viewdefs`, how mongod records a view and a time-series
  collection.
- Store format 9; formats 4 to 8 are refused by name.

#### Fixed

- A store killed before its first periodic checkpoint had no
  feature-compatibility version and no set configuration in its checkpoint:
  mongod refused it (fassert 40652), or started uninitiated and dropped the
  tables of every collection made since as unknown idents.
- A clean stop left `oplogTruncateAfterPoint` past the oplog's last entry;
  mongod, opening the store, truncated, forced its timestamps back and
  aborted on its next `oldest_timestamp`.

Regression tests: `tests/backlog/test_rm_storage_model.py::test_mongod_opens_a_secantus_store`
and `::test_mongod_opens_the_phase_0b_store` (RM-723's forward direction).
Filed: RM-741 to RM-746 (`2d`, `2dsphere` and wildcard keys, users and roles,
a view's oplog form, the stable timestamp after a crash); RM-723 stays open
for the reverse direction.
