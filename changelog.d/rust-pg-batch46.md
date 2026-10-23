### Rust PostgreSQL server: the system columns `ctid`, `xmin`, `cmin`, `xmax`, `cmax`

Every row of a stored table now answers PostgreSQL 15.19's system columns.
They come from per-row state kept as PostgreSQL keeps it (Joe, 2026-10-22:
persist the per-row state). EF Core's and Npgsql's `xmin` concurrency
token, and the `delete ... where ctid = ...` idiom, work.

#### Fixed

- Fixes RP-069
  (`tests/backlog/test_rp_found_2026_10_20.py::test_system_columns`,
  `tests/backlog/test_rp_batch46.py::test_scenario[a row version's ctid, xmin, cmin and xmax]`,
  `[a heap's pages, a rolled-back row's place, and a conflict that takes none]`,
  `[a rewrite, a partition's own heap, VACUUM FULL, CLUSTER and fillfactor]`
  and `[a system column's name is taken]`): `ctid`, `xmin`, `xmax`, `cmin`
  and `cmax` were 42703.
  - **`xmin`** is the writing transaction's xid, assigned at its first write.
  - **`cmin` / `cmax`** are the command id within that transaction. A row
    write uses one command id, even one that wrote nothing. Each DDL kind
    uses the count PostgreSQL's does, as measured.
  - **`xmax`** is a live row's 0.
  - **`ctid`** is the tuple's place in the heap PostgreSQL would have built:
    - **Tuple size.** Each tuple's length follows `heap_form_tuple`
      (header, null bitmap, each attribute at its type's alignment, short
      varlena headers, out-of-line TOAST), on 8 kB pages.
    - **Placement.** An INSERT goes on the last page while the tuple and
      the fillfactor's reserve fit. An UPDATE's new version goes on its
      own page while it fits.
    - **Not transactional.** A rolled-back row's place stays taken, as an
      aborted tuple's does. An `ON CONFLICT` that finds its key takes none.
      The free space is kept in the server process, derived from the rows'
      ctids when a table is first touched, and never committed to the
      store. A commit there would read as another session's to a
      REPEATABLE READ snapshot.
    - **Rewrites.** TRUNCATE starts a new heap. A rewriting ALTER (a type
      change, a serial column) copies the rows in scan order to a new heap.
      So do VACUUM FULL and CLUSTER, keeping each row's `xmin`.
    - **Partitions.** Each partition has a heap of its own.
  - **Reserved names.** A system column's name is now refused as a column
    name (42701, in CREATE TABLE, ADD COLUMN and RENAME COLUMN). It is
    42703 as an INSERT target and 0A000 as an UPDATE target.
- Fixes RP-194 (`tests/backlog/test_rp_batch46.py::test_scenario[CREATE TABLE WITH options are the table's]`):
  `CREATE TABLE ... WITH (fillfactor = 70, ...)` checked its options but
  never recorded them, so `pg_class.reloptions` was NULL. They are now the
  table's, as `ALTER TABLE ... SET` makes them.

- A table the PostgreSQL server wrote still reads, through the Rust MongoDB
  server, as its columns alone: that server leaves out the hidden
  `__sys_*` fields each row now carries, in any collection the SQL catalog
  lists (Joe, 2026-10-23; `secantus-storage-adapter`, the MongoDB server's
  storage bridge). The SQL-to-MongoDB hand-off test pins it
  (`tests/test_rust_pgserver_slice.py::test_a_mongodb_client_reads_a_table_the_rust_sql_server_wrote`),
  along with a MongoDB collection that keeps a field sharing the prefix.

- RP-002's cancel during a parse, with user code in the database, was still
  answered up to 0.3 s late on CI
  (`tests/backlog/test_rp_triage.py::test_cancel_during_parse_is_honoured_with_user_code_present`).
  Passes that read the statement's text before and after planning had no
  cancellation point: the user-function scan, the plan cache's literal scan,
  and its template learning, which parsed a 200,000-item `IN` list again
  (0.8 s). They are now fast or under the cancel hook.

#### Known

- RP-192, filed: a sequential scan reads rows in insertion order, not ctid
  order (`tests/backlog/test_rp_batch46.py::test_scenario[a scan reads rows in ctid order]`).
- RP-193, filed: VACUUM frees no dead tuple's place, and HOT pruning is not
  modelled (`tests/backlog/test_rp_batch46.py::test_scenario[VACUUM frees a dead tuple's place]`).
