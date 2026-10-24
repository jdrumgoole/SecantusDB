### Rust PostgreSQL server: COPY FROM checks its rows and loads faster, one pg_subscription for the cluster, and timestamptz under a session zone

COPY FROM now checks each row's constraints and fires its triggers, as an
INSERT does, and buffers rows as PostgreSQL 15.19 does: `pgbench -i`
takes 42% less time. `pg_subscription` lists every database's
subscriptions. An MCV list over timestamps has its datums. Under a session
time zone other than UTC, a `timestamptz` that an `INSERT ... SELECT`
computes, or that bounds a `tstzrange`, keeps its instant.

#### Fixed

- COPY FROM checked no NOT NULL, CHECK or foreign key constraint and fired
  no trigger, so it could store rows an INSERT refuses (RP-241). Each row
  now goes through what an INSERT's does, in PostgreSQL's order:
  - BEFORE ROW triggers, then NOT NULL and CHECK, as each row is read;
  - the unique and exclusion checks the storage indexes do not make, when
    the buffered rows are inserted;
  - foreign keys and AFTER ROW triggers at the end of the COPY;
  - BEFORE and AFTER STATEMENT triggers once each.
  So the error reported is the one PostgreSQL meets first. A CHECK on line
  2 wins over a bad value on line 3, and a bad value on line 3 wins over a
  duplicate key or a missing foreign key earlier on.

  Fixes RP-241 (tests/backlog/test_rp_batch64_copy.py::test_scenario[COPY FROM checks NOT NULL CHECK and foreign keys],
  tests/backlog/test_rp_batch64_copy.py::test_scenario[COPY FROM reports the error PostgreSQL meets first],
  tests/backlog/test_rp_batch64_copy.py::test_scenario[COPY FROM fires row and statement triggers]).
- In a statement-level trigger, `new.d` and `old.e` read a NULL of the
  column's type, where they were 0A000 `ColumnRef is not supported yet`.

  Fixes RP-243 (tests/backlog/test_rp_batch64.py::test_scenario[a statement trigger's NEW is a NULL row]).
- Subscriptions are kept once for the cluster, beside the databases,
  where each database kept its own (part of RP-169).
  - **Every database's rows.** `pg_subscription`, `pg_replication_origin`,
    `pg_stat_subscription` and `pg_stat_subscription_stats` list every
    database's subscriptions. Before, they showed only the current
    database's, so `subdbid` was always the current database.
  - **Names resolve per database.** A subscription's name is looked up
    among its own database's, so two databases may each have one of a
    name, as in PostgreSQL. A replication origin's id is the lowest free
    one in the cluster.
  - **DROP DATABASE** refuses a database that still has subscriptions with
    55006, `database "d" is being used by logical replication
    subscription`, and PostgreSQL's `There are 2 subscriptions.` detail.
    Before, dropping it silently lost them.
  - **Oids.** A subscription's oid is drawn from the cluster's one oid
    counter, like every other object's (RP-137).
  - **Existing stores.** A subscription an earlier build kept in its
    database is still read as that database's, and moves to the shared
    catalog the next time it changes.

  Fixes part of RP-169 (tests/backlog/test_rp_batch51.py::test_scenario[pg_subscription lists every database's subscriptions],
  tests/backlog/test_rp_batch64.py::test_scenario[each database names its own subscriptions]).
- An extended statistics MCV list over a `timestamp` or `timestamptz`
  column writes PostgreSQL's datums: microseconds since 2000-01-01. Before,
  `stxdmcv` was NULL. Two timestamps a fraction of a millisecond apart are
  now two values, where the hidden microseconds were dropped (part of
  RP-099).

  Fixes part of RP-099 (tests/backlog/test_rp_batch50.py::test_scenario[an MCV list's bytes over timestamps],
  tests/backlog/test_rp_batch64.py::test_scenario[an MCV list over timestamps keeps their microseconds]).
- Under a session zone other than UTC, two `timestamptz` paths stored an
  instant shifted by the zone's offset (RP-240). Both now keep the instant.
  - **INSERT ... SELECT.** A `timestamptz` the query computed
    (`select timestamptz '2021-06-01 00:00+02'`, `z + interval '1 hour'`)
    was read back as a wall-clock `timestamp` in the session's zone. So was
    a `timestamptz[]` copied from a column. Each target column's value is
    now assigned from the source query's own type.
  - **`tstzrange(timestamptz, ...)`** shifted each bound in the same way,
    even in a plain SELECT. A `timestamp` bound is still read in the
    session's zone, as PostgreSQL's cast does.

  Fixes RP-240 (tests/backlog/test_rp_batch64.py::test_scenario[INSERT ... SELECT keeps a timestamptz's instant],
  tests/backlog/test_rp_batch64.py::test_scenario[a tstzrange of timestamptz bounds keeps their instants]).

#### Changed

- COPY FROM buffers its rows as PostgreSQL's `CopyMultiInsertBuffer` does,
  inserting them 1,000 rows or 64 kB at a time. Before, it loaded each
  CopyData message, which `pgbench` and `psql` send one per row. A table
  with triggers still loads each message as it comes, as PostgreSQL inserts
  row by row there. `pg_stat_progress_copy` still counts each row as it is
  read. With two smaller savings, `pgbench -i -s 1` takes 24 s where it
  took 42 s (a debug build on an M-series Mac):
  - **The session's settings** are installed once per COPY, not once per
    message.
  - **A unique index's build** reads only the key's fields of each row.
  - **VACUUM's and ANALYZE's** live-tuple count is a count, not every row
    decoded.

#### Known gaps

- A COPY error carries no CONTEXT naming its line (`COPY t, line 2,
  column a: "x"`) (RP-242, open).
