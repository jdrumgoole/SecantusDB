### Rust PostgreSQL server: oids in creation order

Every object's oid is drawn from one counter for the whole store, as
PostgreSQL's are, so oids rise in the order objects are made.

#### Fixed

- Oids follow creation order (RP-137).
  - **One counter.** Tables, types, sequences, views (with their row type
    and `_RETURN` rule), functions and indexes all draw from it. A primary
    key's, a UNIQUE constraint's and an EXCLUDE constraint's index draw
    theirs after the table's, and a serial column's sequence draws its oid
    before the table's.
  - **Before**, each kind of object had a band of its own. Most were a hash
    of the name, and an index's oid was placed after its table's. So
    `ORDER BY oid` put sequences, views and indexes in another order than
    PostgreSQL's. DROP ROLE's DETAIL came out in that order too.
  - **DROP SCHEMA ... CASCADE** names a schema's members in oid order
    (`table s.t`, `function s.f()`, `view s.v`), not grouped by kind.
  - **Existing stores.** The counter starts above every oid the old bands
    handed out, and an object made before this keeps the oid it had.

  Fixes RP-137 (tests/backlog/test_rp_batch30.py::test_scenario[DROP ROLE lists dependents in creation order],
  tests/backlog/test_rp_batch31.py::test_scenario[DROP SCHEMA lists its members in creation order],
  tests/backlog/test_rp_batch59.py::test_scenario[relations in oid order are in creation order],
  tests/backlog/test_rp_batch59.py::test_scenario[an index's oid is the same in every catalog]).
- A sequence made in an open block, then read, stays a relation to the
  block's next statement. Judging the read's isolation looked the name up
  while the block's transaction was busy. That lookup cached the miss for
  the whole server, so a following `ALTER TABLE s OWNER TO` answered 42P01.
  Pinned by tests/backlog/test_rp_batch59.py::test_scenario[a sequence read in the block it was made in stays a relation].
- A primary key can be dropped (`ALTER TABLE t DROP CONSTRAINT t_pkey`),
  as on PostgreSQL 15.19. Before, it answered 42704.
  - **The rows.** Each row's key values go back to fields of their own and
    the rows keep their ctids. The columns keep NOT NULL, and the key's
    values may repeat afterwards.
  - **Foreign keys.** One that references the key refuses the drop with
    PostgreSQL's 2BP01 and DETAIL. With CASCADE it goes too, with
    PostgreSQL's notice. Dropping a UNIQUE constraint a foreign key
    references now does the same.

  Fixes RP-234 (tests/backlog/test_rp_batch59.py::test_scenario[a primary key can be dropped],
  tests/backlog/test_rp_batch59.py::test_scenario[a foreign key over the key refuses its drop],
  tests/backlog/test_rp_batch59.py::test_scenario[a composite key dropped and its rows kept],
  tests/backlog/test_rp_batch59.py::test_scenario[a dropped key leaves the rows where they were]).
- A table made in an open block answers its system columns (`ctid`, `xmin`,
  `cmin`, `xmax`, `cmax`). Before, they were 42703 until the block
  committed. Fixes RP-235 (tests/backlog/test_rp_batch59.py::test_scenario[a table made in the block answers its system columns]).
