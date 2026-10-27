### Rust PostgreSQL server: the system catalogs' scan counters count what PostgreSQL's caches read

`pg_stat_sys_tables` and `pg_stat_sys_indexes` listed every catalog, but every
counter stayed at zero: this server computes its catalogs rather than scanning
them, so nothing counted a read of `pg_class`. PostgreSQL 15.19 counts every
catalog scan its per-backend caches make — a new session reading its database,
role and settings, a first query on a table building the table's and its
indexes' descriptors and looking up its name, operators and statistics — and
nothing at all for the same query run again, because by then everything it
needs is cached.

The server now models those caches the way PostgreSQL keeps them: each
session holds the set of catalog-cache entries (positive and negative),
catalog-cache lists, relation descriptors, index and extended-statistics lists,
operator-class and type-cache entries it has loaded, and a statement asks for
each key PostgreSQL's parser, planner and executor would look up, in the
conditions of their C sources. A miss counts the index scan (or, for
`pg_am`, the heap scan) PostgreSQL makes, with the rows it returns; a hit
counts nothing. DDL invalidates as `inval.c` does — the changed rows' entries
and their caches' lists, the changed relation's descriptor — at once in the
changing session and in the others when its transaction commits, and an
out-of-place update or a delete leaves the catalog index entries a later scan
returns once without fetching. Across 21 scenarios measured on a private
PostgreSQL 15.19 cluster (new sessions, `select 1`, a table by its key, a table
without an index, functions, casts, a catalog by name, INSERT/UPDATE/DELETE,
`count(*)`, ORDER BY, GROUP BY, LIMIT, a join, a view, a non-superuser role, and
another session's ALTER TABLE), every per-catalog `idx_scan`, `idx_tup_read`
and `idx_tup_fetch` (and `seq_scan` / `seq_tup_read`) delta matches exactly —
all but the ALTER statements' own reads, which are not walked yet (RP-290).
Neither are system views' rule queries or psql's `\d` pattern query (RP-290),
the block counters of catalog reads (RP-291), or the first session after a
start, which PostgreSQL serves without its relation cache init files
(RP-292). A repeated statement costs a hash lookup; a statement seen for the
first time with only its literals changed costs the same, and on an
optimised build a primary-key lookup measured the same before and after.

#### Fixed

- `crates/secantus-pg/src/catcache.rs` (new): a backend's system caches as
  sets, the shared invalidation queue (applied at the next statement, reset
  when a backend falls behind it), and dead catalog index entries. Fixes
  RP-225 (`tests/backlog/test_rp_batch55.py::test_scenario[a new session's
  catalog reads are counted]`; the exact counts are
  `tests/backlog/test_rp_catalog_reads.py::test_catalog_reads`).
- `crates/secantus-pg/src/catalog_reads.rs` (new): the lookups of backend
  start (`InitPostgres` with the relation cache init files in place),
  `namespace.c`, `relcache.c`, function and operator resolution and
  coercions (`parse_func.c`, `parse_oper.c`, `parse_coerce.c`), collation
  assignment, the planner's size, width and selectivity estimates, index
  matching and B-tree correlation, the type cache (`lookup_type_cache`,
  `GetDefaultOpClass`) behind ORDER BY, GROUP BY, `IN` lists and join
  clauses, a view's rule, a SQL function's body, and the executor's
  permission checks, result types and scans of catalogs read directly. A
  statement already walked since the last invalidation is replayed from a
  per-session memo, so a repeated statement costs a hash lookup. The walk
  runs once the statement has (its catalog facts read in the state the
  statement left; a first version that read them before the statement ran
  cached catalog rows the statement then answered from, which the corpora
  and RP-205's regression test caught); a cancelled statement, or one refused
  in an aborted block, is not walked, nor is a text over 64 KiB (RP-290).
- `crates/secantus-pg/src/catalog_facts.rs` (new): PostgreSQL 15.19's own
  `pg_type`, `pg_proc`, `pg_operator`, `pg_amop`, `pg_amproc`, `pg_opclass`,
  `pg_aggregate` and casts, and which catalogs the init files hold and
  `initdb` analysed, read once from the server's existing dumps.
- `crates/secantus-pg/src/pgstat.rs`: a shared catalog's counts (`pg_database`,
  `pg_authid`, `pg_db_role_setting`, ...) are kept under database 0, as
  PostgreSQL keeps them, so every database sees them.
- `crates/secantus-pg/src/lib.rs`: a session's start reads are reported as it
  first goes idle, before its first ReadyForQuery, as PostgreSQL reports them.
