### pgbench and psql run clean — the SQL server's stress smoke lands

Unmodified `pgbench` now drives SecantusDB end to end: the full init cycle
(multi-table `DROP TABLE`, table creation, a 100,000-row client-side `COPY`,
`VACUUM`, and `ALTER TABLE … ADD PRIMARY KEY`), then the TPC-B transaction
script in all three protocol modes — simple, extended, and prepared — plus a
concurrent select-only lane. `psql`'s catalog family (`\dt`, `\d table`,
`\di`, `\l`, `\dn`) runs without error. All of it is packaged as `invoke
sql-stress` (the G7 gauge of the SQL conformance portfolio), weekly in CI,
with the invariant that any error or dropped connection is a bug.

Getting there closed a string of real gaps: multi-name `DROP TABLE a, b, c`;
`VACUUM` accepted; `ALTER TABLE ADD PRIMARY KEY` as a true migration
(validates NOT NULL and uniqueness, then re-keys every existing row onto the
column value); PG's unknown-type literal coercion in arithmetic (`abalance +
$1` with an untyped text parameter — how pgbench binds everything); the
`OPERATOR(pg_catalog.~)` regex spelling with `COLLATE`; schema-qualified
`array_to_string`; comma-join scalar subqueries (psql's collation lookup);
literal `IN` lists in `JOIN ON`; and the pg_catalog surface psql reads —
owner/toast/statistics columns on `pg_class`, encoding and collation on
`pg_database`, `pg_policy`, and present-but-empty `pg_trigger` /
`pg_statistic_ext` / `pg_inherits` / `pg_rewrite` / publication catalogs.

One documented boundary: under concurrent writers to the same row,
WiredTiger's optimistic concurrency surfaces a PG-SERIALIZABLE-style `40001`
serialization failure rather than blocking like READ COMMITTED. Retry-capable
clients handle this normally; the smoke keeps its write lanes single-client
and the retry-semantics question is tracked in the backlog.

#### Added

- `sqlstress_validation/` + `invoke sql-stress` + weekly `validate.yml` row
  (installs postgresql-contrib for pgbench/psql).
- `sql/executor.py`: `ALTER TABLE … ADD [CONSTRAINT] PRIMARY KEY` with row
  re-keying and 23502/23505/42P16 validation.
- `sql/planner.py` + `sql/engine.py`: multi-name `DROP TABLE`; `VACUUM`;
  `OPERATOR(pg_catalog.~ / ~*)` (+ negations) rewritten to regex matches;
  literal `IN` lists in join `ON`.
- `sql/scalar.py`: unknown-text numeric coercion in arithmetic (22P02 on
  garbage), `pg_get_userbyid`, `pg_encoding_to_char`, schema-qualified
  `array_to_string`, comma-join (cartesian) scalar subqueries.
- `sql/virtual.py`: `pg_class` owner/toast/check/flag columns, `pg_database`
  encoding/collation/ACL, `pg_namespace` owners, `pg_index` validity flags,
  `pg_policy`, and empty `pg_trigger` / `pg_statistic_ext` / `pg_inherits` /
  `pg_rewrite` / `pg_publication*` catalogs.
