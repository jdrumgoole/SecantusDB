### Collection sizes are kept in mongod's `sizeStorer`, keyed by ident

The record count and data size of every collection now live where mongod
8.2.11 keeps them: a `sizeStorer` table, created as mongod creates it
(`key_format=u,value_format=u`, WiredTiger's defaults otherwise, logged),
keyed by the ident's table URI as raw bytes (`table:collection-<UUID>`), with
the value `{numRecords: NumberLong, dataSize: NumberLong}` -- 43 bytes, the
same bytes mongod writes for the same documents. As on mongod, there is a row
for each collection whose sizes have moved (an emptied collection keeps a
`0, 0` row, one never written has none), one for the oplog and one for
`_mdb_catalog`, and none for an index. A rename keeps the collection's ident,
so its row stays where it was; a drop removes it with the ident.

What mongod's size storer does not hold -- each index's entry bytes, which
`collStats` reports, and the marker saying the last close was clean -- stays
in this server's own `secantus_size_storer`, now keyed by the index's ident
too. A store that did not close cleanly is still counted again at open by one
walk, which gives a mongod replica-set member's answer after a crash.

`pg_relation_filepath` names the relation's own file, relative to the data
directory (`collection-<UUID>.wt` for a table, `index-<UUID>.wt` for an
index). Two of its answers were wrong and are fixed: a primary key answered
NULL instead of its index's file, and a name was looked up only among tables
and views, so an index's, a sequence's or a catalog's name failed with
42P01; the argument is a `regclass` and is now read as PostgreSQL reads one.

**This changes the on-disk format again** (store format 8): a store written
before it is refused at open.

#### Changed

- `secantus-storage`: the size storer's rows are written to `sizeStorer` by
  ident and read back through the catalog's placements; `size_storer_value`
  encodes a row as mongod does; `raw_table_rows` reads a raw-keyed table for
  layout tests.
- `tools/probes/mongod_store_layout.py`: reads `sizeStorer` from both stores
  (the phase 5 tolerance of a store without one is gone) and labels its rows
  by namespace.

#### Fixed

- `secantus-pg`: `pg_relation_filepath` of a primary key, and of an index,
  sequence or catalog given by name.

#### Tests

- `crates/secantus-storage/tests/size_storer.rs`: the rows and the table
  configuration against what mongod 8.2.11 wrote (`app.a`'s row byte for
  byte), a rename keeping its row and a drop removing it, and a process killed
  with SIGKILL being counted again at open.
- `crates/secantus-storage/src/store_format.rs`: stores at formats 4 to 7 are
  refused by name.
- `tests/test_rust_pgserver_slice.py::test_relation_filepath_names_each_relations_own_file`.
- `tests/backlog/test_rm_storage_model.py::test_store_has_mongods_files`
  (part of RM-723) now passes and is a regression test.
- Filed: RP-1075 (a catalog's `pg_relation_filepath` is NULL), RP-1076
  (every sequence names one file), RP-1077 (`pg_relation_filenode` is
  refused), with gap tests in `tests/backlog/test_rp_storage_model.py`.
