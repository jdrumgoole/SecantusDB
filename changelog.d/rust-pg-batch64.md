### Rust PostgreSQL server: pg_subscription is one catalog for the cluster

`pg_subscription` lists every database's subscriptions, each with its
`subdbid`, as PostgreSQL 15.19's shared catalog does.

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
