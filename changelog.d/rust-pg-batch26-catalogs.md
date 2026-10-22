### Rust PostgreSQL server: PostgreSQL's own client tools work, and pg_dump's output matches

PostgreSQL 15's `pg_dump`, `pg_dumpall --globals-only`, `pgbench -i` and
psql's `\l`, `\l+`, `\dd`, `\dc`, `\dL`, `\dy` and `\db` now run against the
Rust PostgreSQL server as they do against PostgreSQL 15.19. `pg_dump` of a
schema with sequences, identity and generated columns, foreign keys, checks,
enums and domains, functions and triggers, views and a materialized view,
partitions, inheritance, grants and row-level security produces the same
text, byte for byte, as against PostgreSQL, and a PostgreSQL dump restores
here and dumps back unchanged.

181 of the 203 `pg_catalog` / `information_schema` relations PostgreSQL 15.19
lists now answer (from 76). The `information_schema` views and the `pg_catalog`
system views the server does not compute itself run PostgreSQL's own view
definitions over this server's catalogs; the built-in rows of `pg_aggregate`,
`pg_amop`, `pg_amproc`, `pg_opfamily`, `pg_conversion`, `pg_init_privs` and
the `sql_*` tables are PostgreSQL 15.19's, dumped by
`tools/probes/pg15_system_catalog_dump.py`.

#### Added

- `secantus-pgplan`: system views from PostgreSQL 15.19's definitions
  (`pg15_system_views.tsv`), expanded as the rewriter expands a view, and the
  `information_schema` SQL helpers (`_pg_char_max_length`, `_pg_truetypid`,
  ...) inlined as PostgreSQL inlines a SQL function. `information_schema`'s
  five domains (`sql_identifier`, `cardinal_number`, ...) with their oids.
- `secantus-pg`: `pg_aggregate` (with user aggregates), `pg_amop`,
  `pg_amproc`, `pg_opfamily`, `pg_conversion`, `pg_init_privs`,
  `pg_shdescription` (COMMENT ON DATABASE / ROLE), `pg_db_role_setting`,
  `pg_largeobject`, `pg_shdepend`, `pg_depend` (owned and identity sequences,
  indexes, partitions, inheritance, column defaults),
  `pg_available_extensions` / `_versions`, `pg_stat_gssapi`,
  `pg_stat_database_conflicts`, `pg_stat_recovery_prefetch`, and the
  catalogs no statement here can write (`pg_transform`, `pg_seclabel`,
  `pg_subscription`, the replication views, ...), empty.
- PostgreSQL's twelve predefined roles and three built-in memberships, with
  `CREATE / ALTER / DROP ROLE`'s refusals for them.
- `ALTER TYPE / DOMAIN ... OWNER TO`; `ALTER TABLE ... ALTER COLUMN ... ADD
  GENERATED ... AS IDENTITY` (and `SEQUENCE NAME` for an identity);
  `pg_get_function_arg_default`, `pg_my_temp_schema`, `pg_is_in_recovery`,
  `pg_postmaster_start_time`, `pg_tablespace_location`; `pg_get_serial_sequence`
  and `set_config` anywhere in a query.
- Refused by name (0A000), as decided: `pg_backend_memory_contexts`,
  `pg_shmem_allocations`, `pg_stat_bgwriter`, `pg_stat_wal`, `pg_stat_slru`,
  `pg_stat_archiver`; and, until their items are done, the statistics
  (RP-099), progress (RP-100) and configuration-file (RP-101) views.

#### Fixed

- Fixes RP-071 (`tests/backlog/test_rp_found_2026_10_20.py::test_client_tools`,
  `tests/backlog/test_rp_batch26.py::test_pg_dump_matches` and its `RP-071`
  scenarios): `name` subscripts (`typname[0]`), a FROM function over the
  outer row (`unnest(evttags)`), `tableoid` in a JOIN's ON clause, names
  qualified by `search_path` in `pg_get_viewdef`, `pg_get_constraintdef`,
  `pg_get_triggerdef`, `pg_get_expr` and `format_type` (pg_dump sets it
  empty), schema and function comments in `pg_description`, `nspacl`,
  system columns' `attacl`, `attstorage` / `attlen` of enum and domain
  columns, inherited columns' `attinhcount`, aclitem / oid / `"char"` /
  regproc column types in `pg_proc`, `pg_type`, `pg_tablespace`,
  `pg_operator`, `pg_am`, `pg_extension`, `pg_statistic_ext`; `btree_gist`
  is 1.7, as 15.19 ships it.
- RP-070 is now only the cumulative statistics views
  (`tests/backlog/test_rp_found_2026_10_20.py::test_catalog_relations_are_queryable`
  restated as decided; the rest pinned by `tests/backlog/test_rp_batch26.py`'s
  `RP-070` scenarios).
- Fixes RP-092 (`tests/backlog/test_rp_batch26.py::test_memberships_are_the_clusters`):
  a role membership (GRANT role TO role) was kept per database, so it was
  invisible from every other one; it is the cluster's now, as `pg_auth_members`
  is shared (an older store's per-database records are still read).
- Fixes RP-093 (`...::test_scenario[a renamed table keeps its row type]`):
  `ALTER TABLE ... RENAME` left the table's row type under the old name, so
  `'new'::regclass` failed and `pg_class` listed the old name as a composite
  type.
- Fixes RP-094 (`...::test_copy_of_a_table`): `COPY table TO` wrote a
  `timestamptz` without its offset; the text formats now render each value as
  a SELECT of the table does.
- Fixes RP-095 (`...::test_scenario[set operations resolve one type]`): a set
  operation's column type is PostgreSQL's `select_common_type` (`oid UNION
  regclass` is oid, `int4 UNION regclass` regclass), each side is converted
  to it (a `char(n)` loses its padding as text), and a pair with no implicit
  cast is 42846.
- Fixes RP-096 (`...::test_scenario[materialized views]`): `COMMENT ON
  MATERIALIZED VIEW`, `ALTER MATERIALIZED VIEW ... RENAME`, 55000 for reading
  one never populated, and REFRESH under an empty `search_path`.
- Fixes RP-097 (`...::test_scenario[ALTER TABLE on a sequence]`):
  `ALTER TABLE seq OWNER TO` wrote a table's catalog row for the sequence,
  which a later pg_dump showed as `CREATE TABLE`; it records the sequence's
  owner, and other actions are 42809.
- A pattern match (`LIKE`) and every operator over a domain resolve as its
  base type.
- Fixes RP-108 (`tests/backlog/test_rp_batch26.py::test_scenario[the server's version]`):
  the server reports PostgreSQL 15.19, the release its answers are measured
  against (`server_version` 15.19, `server_version_num` 150019, `version()`
  `PostgreSQL 15.19 (SecantusDB) ...`), where it said 15.0 (Joe, 2026-10-22).

#### Build

- (Landed with batch 25.) `secantus-pgplan`'s ICU crates are held to the 2.3
  series its baked collation data was generated for, which a newly published
  `icu_collections` 2.4.0 broke; `rustls-webpki` 0.103.16 and `webpki-roots`
  1.0.10 (RUSTSEC-2026-0382, RUSTSEC-2026-0381).
