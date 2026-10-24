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
