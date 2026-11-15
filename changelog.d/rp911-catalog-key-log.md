### The statement after DDL re-reads only the catalog rows the DDL changed

The PostgreSQL server used to read and compare its whole table catalog and
composite-type catalog again after every DDL statement, so the first
statement after a `CREATE TABLE` grew with the number of tables. The
storage now keeps a log of the catalog documents each commit changed, by
key, the record PostgreSQL's `inval.c` keeps, and the server patches its
previous copy of each catalog with just those documents.

#### Changed

- `secantus-storage`: every committed write of a `__`-prefixed collection's
  document logs its `_id`, read off the write's oplog entry, at the
  statement or user transaction's commit (`Storage::catalog_doc_serial`,
  `Storage::catalog_doc_changes_since`). A drop or rename of a catalog
  collection, and any write whose documents are not known, log the whole
  collection or database. Both Rust servers link this; only the PostgreSQL
  server reads the log.
- `secantus-pg`: the committed composite catalog and table catalog are
  brought up to date by re-reading the logged documents (up to 64), and
  read in full only when there is no earlier copy, the log no longer
  reaches it, or a change names no single document. RP-911 in part.
- Tests: `tests/test_rust_pg_catalog_key_log.py` (DDL on one connection
  read back on another: create, alter, rename, drop, a committed and a
  rolled-back block, a composite type, more changes than a patch takes).
