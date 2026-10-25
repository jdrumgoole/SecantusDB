### The Rust PostgreSQL server: partial and hash indexes under SERIALIZABLE, index storage parameters, and savepoints that undo index DDL

A SERIALIZABLE read whose WHERE implies a partial index's predicate, or that
a hash index serves, now reads through that index, as PostgreSQL 15.19's
planner chooses. It takes SIREAD locks on the pages it reads rather than on
the whole table, so a transaction fails where and when it fails in
PostgreSQL. Two transactions on keys in different hash buckets now both
commit.

An index's storage parameters (`WITH (fillfactor = 70)`) are now checked,
kept, shown in `pg_class.reloptions` and `pg_get_indexdef`, and changed by
`ALTER INDEX ... SET` / `RESET`. `ROLLBACK TO SAVEPOINT` now undoes an
index's DDL. Before, rolling back to a savepoint did not undo `CREATE INDEX`,
and undoing a `DROP TABLE` lost the table's indexes for good.

#### Fixed

- `secantus-pgplan`: the cost model prices a partial index
  (`costsize::Index` gains `tuples` and `predicate`).
  - A partial index whose predicate the query implies (`predOK`) serves a
    scan with no index qual of its own.
  - Its selectivity includes the predicate's clauses
    (`add_predicate_to_index_quals`).
  - Its tuple estimate caps the index tuples, the pages read and the
    descent, as in `genericcostestimate` and `btcostestimate`.
- `secantus-pg`: SSI's scan choice adds every partial B-tree whose predicate
  the WHERE implies.
  - Each is sized as `estimate_rel_size` sizes it: its pages, and its
    tuples at `pg_class`'s density, or from its columns' widths when
    `pg_class` holds nothing past the metapage.
  - It no longer costs a partial index as if it were a full one.
  - Fixes RP-253
    (`tests/backlog/test_rp_isolation.py::test_ssi_partial_index`).
- `secantus-pg`: hash indexes for SERIALIZABLE (`hash_index.rs`). A
  hash index scan locks the primary page of each key's bucket
  (`_hash_first`), and an insert checks that page first (`_hash_doinsert`).
  - Which bucket a key is in follows PostgreSQL's metapage:
    `_hash_init`'s bucket count from the estimated rows, the masks, the
    default fill factor's split point, `_hash_expandtable`'s splits (which
    move the old bucket's locks to the new one) and VACUUM's recount.
  - The key's hash is the type's standard hash function.
  - Only hash indexes on a type whose hash is reproduced are modelled. A
    scan never chooses any other hash index, so it locks the whole table
    and misses no conflict.
  - Fixes the hash part of RP-254
    (`tests/backlog/test_rp_isolation.py::test_ssi_hash_index`). GIN and
    GiST stay open.
- `secantus-pgplan`: `hashpart::standard_hash`, a value's standard hash
  (the low 32 bits of its extended hash with seed 0), and a hash index in
  the cost model (`Index::hash`: `genericcostestimate`, `=` and `IN` only).
- `secantus-pg`: index storage parameters. Fixes RP-261
  (`tests/backlog/test_rp_batch77.py::test_scenario[an index's storage
  parameters are kept and printed]` and its two neighbours).
  - CREATE INDEX checks `WITH (...)` against the access method's
    parameters, as PostgreSQL 15's `reloptions.c` defines them, and keeps
    it with the index.
  - `pg_class.reloptions` and `pg_get_indexdef`'s `WITH (...)` show it,
    each value bare or quoted as `flatten_reloptions` prints it.
  - `ALTER INDEX ... SET` / `RESET` change it inside the transaction.
  - A hash index's `fillfactor` sets its bucket split point when it is
    built.
- `secantus-pg`: `ROLLBACK TO SAVEPOINT` puts back the table's storage
  indexes as they were. It drops the ones made since, makes again the ones
  dropped, and remakes one renamed or altered, which also restores the
  indexes of a table whose DROP it undoes. Fixes RP-262
  (`tests/backlog/test_rp_batch77.py::test_scenario[ROLLBACK TO SAVEPOINT
  undoes an index's DDL]`).
- `secantus-pgplan`: a table's storage parameter given bare is checked as
  `true` ("invalid value for integer option"); one given twice is "specified
  more than once"; an out-of-bounds or enum value's error carries
  PostgreSQL's DETAIL. Fixes RP-263
  (`tests/backlog/test_rp_batch77.py::test_scenario[a table's storage
  parameter given bare or twice]`).
- `secantus-pgplan`: a missing relation in `ALTER TABLE`, `INDEX`, `VIEW`,
  `SEQUENCE`, `MATERIALIZED VIEW` or `FOREIGN TABLE` has no position, as in
  PostgreSQL. Fixes RP-264
  (`tests/backlog/test_rp_batch77.py::test_alter_missing_relation_has_no_position`).
- `secantus-storage` (shared with the MongoDB server): `set_index_options`
  writes inside an open user transaction, so a rollback undoes it. Its
  MongoDB caller, `collMod`, never runs in one.
