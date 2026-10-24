### Rust PostgreSQL server: one pg_subscription for the cluster, timestamps in MCV lists and timestamptz under a session zone

`pg_subscription` lists every database's subscriptions, as PostgreSQL
15.19's shared catalog does. An MCV list over timestamps has its datums.
Under a session time zone other than UTC, a `timestamptz` that an
`INSERT ... SELECT` computes, or that bounds a `tstzrange`, keeps its
instant.

#### Fixed

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
