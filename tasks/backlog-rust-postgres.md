# Backlog: the Rust PostgreSQL server

A living list of where this server does not yet behave like the real
product it stands in for: stubs, stopgaps, deferred work and known bugs.
Update it when you stub something, defer a slice, or find a limitation, and
delete an item's lines when you fix it -- finished work lives in git
history and `changelog.d/`, not here. Don't add work that already has a fix
in flight; that lives on its branch and PR.

The backlog is four files, one per server:

| file | server |
| --- | --- |
| `tasks/backlog-rust-mongodb.md` | the Rust MongoDB server, and the Rust storage both Rust servers use; also CI, build and release items no one server owns |
| `tasks/backlog-rust-postgres.md` | the Rust PostgreSQL server |
| `tasks/backlog-python-mongodb.md` | the Python MongoDB server (and the admin UI) |
| `tasks/backlog-python-postgres.md` | the Python PostgreSQL / SQL server |

An item about more than one server is in each file it concerns. Section
numbers (`7.077`, `3.1`, ...) are the ones the single `tasks/backlog.md`
used, kept so the code comments that cite them still resolve.

**`- [ ]` is open work, and every item here is open.** An open item's bold
headline must not read as finished: if completed context belongs in it,
lead with `**OPEN —` (`tests/test_backlog_markers.py`).

**Every item carries `Backlog-ID:` and `Verify:` lines.** `Verify:` names a
gap test under `tests/backlog/` that asserts the real product's answer and so
fails while the item is open, or says why none can (`none -- ...`).
`python tools/backlog.py verify` runs them all; the `backlog` skill
(`.claude/skills/backlog/SKILL.md`) is the protocol for filing, working and
closing an item.

**Reproduce an item before working it** -- a probe, the failing test, the
query -- and size it from that, not from the text. Entries here have
described work as missing that was done, and understated bugs by naming one
symptom of several. Correct a superseded entry rather than adding a newer
one beside it.

## Open from the 2026-08-26 triage

- **Scope decision — psycopg's `test_generators.py::test_cancel` and the
  `proxy`-marked `test_cancel_safe_error` / `test_cancel_safe_timeout` fail on
  macOS in the CLIENT.** macOS libpq sends the cancel on a not-yet-connected
  non-blocking socket and waits forever; native PostgreSQL 15 fails the same
  call sequence (7 of 10 rounds), and `pproxy` cannot start under this
  venv's uvloop on Python 3.12. `psycopg_validation/include_paths.py` keeps
  them out of the macOS gauge. Nothing to do server-side.
- **Scope decision — a `LANGUAGE internal` wrapper over a C function with no
  SQL form** (an I/O function such as `int4out`, an aggregate transition
  function) is a catalog row that answers 42883 when called. PostgreSQL calls
  the C symbol with whatever the declaration says, so a wrapper over the
  wrong signature returns garbage or crashes the backend: there is no defined
  behaviour to imitate. Wrappers over C functions that have a SQL form work.
- **Scope decision — dotted / `$`-named columns.** Their answers match
  PostgreSQL, but a predicate over such a column runs per row and a plain
  CREATE INDEX on one builds a path index no query uses. The shared storage
  reads a dot as a path separator and a leading `$` as an operator in every
  filter and index key spec (MQL semantics the Rust MongoDB server depends
  on), so an index-backed read needs a literal-key index path in
  `secantus-storage` or a stored-name escape, the second an on-disk format
  change both PG servers share. Not justified by a name SQLAlchemy's suite
  uses only to prove quoting works.
- **Scope decision — pgjdbc `BlobTransactionTest`** needs `CREATE FUNCTION
  lo_manage() ... LANGUAGE C` (the `lo` contrib trigger). `LANGUAGE C` loads
  a shared library into the server process, which a server written in Rust
  has no ABI to host; the `lo` contrib module is not shipped.
- **Scope decision — direct writes to a catalog** (an UPDATE that changes a
  catalog row, a DELETE that matches one, an INSERT into a catalog) are
  0A000: the catalogs are computed from the store, so there is no row to
  change. An UPDATE leaving rows as they are reports them, as pgjdbc's
  updatable-result-set write-back needs.
- **Scope decision — a subscription never reaches its publisher.**
  Logical replication between servers is multi-node, which `CLAUDE.md`
  puts out of scope. `CREATE / ALTER / DROP SUBSCRIPTION` run as
  PostgreSQL 15.19 runs them up to the point where it would connect, so
  every check before that answers as PostgreSQL does. What would connect is
  0A000, named: `CREATE SUBSCRIPTION` with `connect = true` (its default),
  `REFRESH PUBLICATION` and a refreshing `SET / ADD / DROP PUBLICATION` on
  an enabled subscription, `ENABLE`, and `DROP` of a subscription that
  names a slot (outside a block; inside one PostgreSQL's 25001 comes
  first). PostgreSQL answers 08006 when its publisher cannot be reached.
- **Not the server — pgjdbc `ConnectionTest::pGStreamSettings`** asserts the
  CLIENT socket's receive buffer and flickers on its own.
- **Scope decision — the sqllogictest gauge on the Rust server**
  (`SECANTUS_GAUGE_SERVER=rust`, report `slt-raw-rust-server.json`): the lines
  it still reports are SQLite expectations PostgreSQL 15 also fails (`IN ()`
  is a syntax error; DELETE through a simple view succeeds;
  `col / CAST(32 AS REAL)` is a real; `415.0000000000000000` for numeric; an
  ungrouped column under COALESCE is 42803). The Python lane marks the same
  ones as expected divergences.
- **Scope decision — EXPLAIN** prints PostgreSQL's plan SHAPE with zero
  costs, and which side of a hash join is built follows this server's join
  order. Costs and join sides are PostgreSQL's cost model (statistics, page
  counts), which this server does not have. A PARTIAL expression index is not
  used for a WHERE comparing the expression with a constant (its field is
  absent outside the predicate) -- a plan choice; the WHERE still runs, as a
  scan.
- **Scope decision — planning-time constant-folding errors.** PostgreSQL
  raises `select 1/0 from (values (1)) v(a) where false` and `select f(0)
  from t where false` because `eval_const_expressions` folds every immutable
  subexpression over constants while planning, whether or not a row reaches
  it; here each answers zero rows. PostgreSQL's manual (4.2.14) calls this a
  quirk applications must not rely on, and folding anything PostgreSQL does
  not would raise errors it does not -- worse than zero rows. No answer a
  query returns differs.
- **Known residual — PL/pgSQL `SQL expression` CONTEXT frame**
  (`tools/probes/plpgsql_expr_context.py`, 58 of 60 shapes match PostgreSQL
  15.19): `(array[1,2])[1]/0` (libpg_query deparses the subexpression without
  its parentheses, so it cannot be run alone) and `immutable_sql_fn()/0`
  (PostgreSQL inlines an IMMUTABLE SQL function before folding; a user
  function is not folded here).
- **Scope decision — foreign data**: no FDW handler exists, so a foreign
  table refuses reads and writes with PostgreSQL's own `55000` for a
  handler-less wrapper; `postgres_fdw` / `file_fdw` are contrib modules this
  server does not ship.
- **Scope decision — libc collations**: only `C` / `POSIX` exist here
  (`COLLATE "en_US"` is 42704); which libc locales a server offers is the
  host's locale list, and a single-node surrogate does not reproduce the
  host's. ICU collations cover the locale-ordering need: all 907 of
  PostgreSQL 15.19's are served (RP-005, 2026-10-18).
- **Scope decision — the comparison functions whose magnitude is the
  platform's answer the SIGN only** (Joe, 2026-10-16): `bttextcmp`,
  `bpcharcmp` and `jsonb_cmp` compare under the database collation through
  `strcoll` (`bttextcmp('b', 'a')` is 14 on the macOS reference), and
  `tsvector_cmp` / `tsquery_cmp` by their storage layouts; each answers
  -1 / 0 / 1 in PostgreSQL's order, which is all PostgreSQL promises.
- **Scope decision — pgcrypto Blowfish and CAST5** PGP messages are verified
  against GnuPG, because both reference servers' OpenSSL 3 builds refuse
  those ciphers. Do not "fix" them to agree with a PG error.
- **Scope decision — constraint `oid`s** are this server's own numbers. An
  oid is an allocation counter (PostgreSQL's differ between two clusters
  given the same DDL); only consistency across the catalogs matters, which
  the corpora check through `conrelid` / `conindid` joins.
- **Scope decision — jsonb equality and UNIQUE do not use an index.** jsonb
  compares by value (`json::jsonb_value_key`), so an equality on a jsonb
  column no longer uses a btree index on it, and a jsonb UNIQUE constraint
  reads the table per write statement. A lookup over the stored text cannot
  be made sound (one value has unboundedly many spellings: `1`, `1.0`,
  `1e0`), and a value-keyed index is an on-disk format change for both PG
  servers. The costs stay as they are, correct.
- **Scope decision — a transaction is limited by WiredTiger's cache** (Joe,
  2026-10-25, RP-256). A transaction's uncommitted writes stay in the cache
  until it commits, where PostgreSQL's heap and WAL take any size to disk.
  The PG server raises the eviction triggers to 50%, so one transaction
  holds about 1,000,000 small rows at the 4G cache (2,000,000 fail). Past
  that it fails XX000 "Transaction is too large and will not fit in the
  storage engine cache". `ALTER TABLE ... ADD PRIMARY KEY` re-keys every
  stored row, so it counts each row again. Removing the limit means
  spilling a transaction's writes outside WiredTiger, a large project in
  the shared storage.
- **Scope decision — REPEATABLE READ answers 40001 after a wait when ANY
  transaction committed since its snapshot**, where PostgreSQL goes on unless
  the ROW changed. WiredTiger cannot continue a transaction after a conflict,
  so going on means a new snapshot, which under REPEATABLE READ is invisible
  only when nothing it could read later committed -- unknowable for the rest
  of the transaction. Narrowing to the conflicting table would let a later
  read see rows committed after the snapshot: a wrong answer, worse than a
  40001 every client must retry anyway. Two things are not counted as
  commits (batch 76): the checkpointer's saves of the page models, which no
  snapshot reads, and a duplicate key. A key another transaction committed
  is PostgreSQL's 23505 whatever the snapshot, and that error ends the
  block, so the moved snapshot is never read.

## 3. Deferred work (skipped from a slice, ready to come back)

- **CLOSED (scope record, batch 68, 2026-10-07) — RUST pgserver:
      reading ONE ROW BY PRIMARY KEY, the per-statement gap to PostgreSQL.**
      Release build against PostgreSQL 15.19, libpq `PQexecPrepared`: PK read
      ~41-43 us against PostgreSQL's 25.4; extended `select 1` 32.0-33.1
      against 21.4-22.1; simple `select 1` 27.9-28.8 against 22.7-23.0. The
      gap is CPU on the connection's thread, spread over many pieces none
      above ~1.4 us (the two socket syscalls PostgreSQL also makes, the
      storage read, Describe, the extended group's open / commit,
      `install_user_types`, malloc / free). No single lever is left; closing
      the gap would mean restructuring the statement path (PostgreSQL runs a
      cached plan in a process-per-connection backend with no hand-offs),
      not fixing a hot spot. Two things already tried and rejected, so they
      are not re-tried: skipping the per-statement transaction group for
      statements that touch no storage (moves `select 1` by ~3 us, leaves
      every real query where it was, and risks statement-level snapshot
      consistency), and a per-session WiredTiger cursor cache (cursor opens
      3 -> 2 per row read, no measurable change).

- [ ] **OPEN — `test_connect_error_multi_hosts_each_message_preserved` failed
      twice in a full psycopg-gauge run; cause UNKNOWN, and socket exhaustion
      is now RULED OUT (measured 2026-09-19).** The test connects to two RFC
      5737 reserved addresses and requires both attempts to fail with
      `connection timeout expired`. It never touches this server. It runs in
      the gauge (re-enabled by PR #1484; standalone it connects 6/6 in 4.04s
      with the expected error). The unexplained part is that two runs failed
      it in **0.04s** — an immediate `connect()` error rather than a timeout.

      Ruled out by measurement during a full gauge run: ephemeral-port
      exhaustion (peak 1,155 of 16,384, sawtoothing, no leak) and descriptor
      exhaustion (`ulimit -n` 1,048,576; weaker evidence, the fd sample
      followed the runner rather than its pytest child).
      **If it recurs, capture the exception text and errno before theorising** —
      that names the failure and distinguishes the remaining possibilities.
      Backlog-ID: RP-006
      Verify: none -- client-side psycopg test flake connecting to reserved addresses; never touches the server

- [ ] **OPEN — Rust PostgreSQL server on LINUX: durable writes are
      1.2-1.4x slower than PostgreSQL at eight clients and reads ~1.1x
      (0.1.0-beta.15, 2026-10-13); one client's statement costs ~3x
      PostgreSQL's CPU.** Different from the macOS entry below: on Linux the
      cost of one statement is the problem, not concurrency.

      **Current state** (DigitalOcean `c-16`, Xeon 8280, ~12,200 `fdatasync`
      a second, PostgreSQL 16 defaults, neither server over TLS, seven
      trials, eight clients, PostgreSQL / 0.1.0-beta.15): insert 20,106 /
      15,866 (1.27x); insert into a shared table 20,753 / 16,703 (1.24x);
      update 22,843 / 16,655 (1.37x); select 54,395 / 48,849 (1.11x). A rate
      is comparable only within one run. The site publishes the 8-client
      rows only (Joe's call, 2026-10-12); with clients on separate droplets
      the 1- and 4-client rows are steady (2% to 9% spread on this server)
      and publishing them is Joe's call.

      **How to measure.** `invoke do-pg-bench` (clients on the two client
      droplets over the private network by default; `sslmode=disable` in
      every DSN, checked against `pg_stat_ssl`). A `c-16` is not one
      machine: each run records the processor and `pg_test_fsync`'s rate in
      `facts.txt`; compare two runs only when those agree. `do-pg-bench`
      refuses a server disk under 5,000 `fdatasync` calls a second, and
      `invoke do-pg-up` replaces droplets until one passes. Over loopback,
      1- and 4-client rows move with CPU placement (this server loses 15% to
      25% unpinned, PostgreSQL 2% to 6%) and with the disk's own sync-rate
      spread (~13%); `pg_concurrency.py` has no pinning option.

      **The single-connection cost** (2026-10-13, data in
      `bench/results/pg-droplet/2026-10-13-single-connection-cost/`): no
      thread hand-off (one server thread wakes once a statement); it is CPU
      on that thread, 61 us a statement against PostgreSQL's 22 — ~97,000
      user-mode instructions to 35,000, at 0.54 IPC to 0.91, with 684 branch
      mispredictions to 89 and 2.4x the instruction-cache misses. The
      symbolised profile is flat (nothing above 3%, the allocator first).
      The release build already has fat LTO, one codegen unit and a profile
      (`scripts/build_pg_release.sh`); the placement sensitivity follows from
      the longer run per statement.

      **Three slow update trials, never seen again, cause unknown.** In the
      0.1.0-beta.13 run of 2026-10-13, eight clients updating by primary key
      gave 12,418 and 12,209 a second and then 8,019, 8,156 and 9,000, one
      server process throughout. Fifty-four further trials on three droplets
      had none (`bench/results/pg-droplet/2026-10-13-slow-update-trials/`).
      The benchmark now records the disk's flush time and the server's CPU a
      statement for every trial and prints an unsteady row trial by trial,
      so the next one says which it was.

      **What is left on the prepared write path, in order of what the
      profile offers:**
      - a stored value into any other column type (`varchar(n)`, `smallint`,
        `numeric`, `timestamptz`, `bool`...) is still planned per Execute,
        because the planner checks or converts it. Templating those means
        running the column's check at substitution time;
      - a parameter of a kind with no stand-in (bool, date, numeric, bytes,
        NULL) is never templated at all;
      - once any table in the database has row-level security, nothing is
        templated;
      - `execute_statement` outside `update_matching` (~20% of an UPDATE);
      - an int bound for a `float8` or `numeric` column is still planned
        per Execute (only int32 -> `bigint` is carried).

      **What is left on the unprepared statement** (measured on the
      development Mac only, NOT re-measured on Linux; `bench/
      pg_statement_cost.py`'s `select_row_var` stage):
      - a statement whose shape is not templated still pays one parse,
        and 70% of a parse is libpg_query packing its tree to protobuf
        (`protobuf_c_message_pack` walks all ~270 alternatives of the
        `Node` oneof for every node, twice). Not ours to change, and
        `pg_query` 6.2 has no parse that skips it;
      - a decimal or exponent literal, a negative number written against
        its operator (`k =-5`), a string with a backslash or an `E''`
        prefix: not taken out by `plan_cache::literal_family`, planned
        every time;
      - the plan cache is per connection and holds 256 entries, cleared
        whole when full.
      Backlog-ID: RP-007
      Verify: none -- Linux throughput/CPU-per-statement benchmark campaign

- **ACCEPTED AS A LIMIT 2026-10-07 (Joe), and may be revisited — Rust
      PostgreSQL server on macOS: a READ statement costs ~1.45-1.6x
      PostgreSQL's, and durable writes reach ~3x scaling at 8 clients against
      PostgreSQL's ~4x (batch 72).** Not being worked: no lever above ~1us is
      known for reads, and the write gap is the hand-off between one group's
      log write and the next (the disk is ~55% busy at N=8). Joe kept the
      option to reopen it; a session that does should re-measure batch 72's
      numbers first (`bench/pg_concurrency.py`, `bench/pg_statement_cost.py`
      against a release `secantusd-pg`, `SECANTUSD_PG` set from a worktree,
      no other suite running). Constraints that shape any reopening:
      - **Group commit that acknowledges before the flush was tried and
        DROPPED** (Joe: match PostgreSQL visibility) — it made a row visible
        to other sessions before its flush. WiredTiger prepared transactions
        are refused on logged tables, so they are no way round it.
        `test_concurrent_acknowledged_commits_survive_a_kill_in_durable_mode`
        is the guard.
      - **Do NOT "lift the global lock" as a concurrency fix** — it guards
        DDL and admin paths, not row writes, and protects the oplog's
        strictly-monotonic `seq` / event order and the durable-close /
        checkpoint path.

### 3.1 Authentication

- [ ] **RUST PG server: what the login rules leave.** TLS, SCRAM channel
  binding, client certificates, `--tls-require`, `pg_stat_ssl`,
  refusing unknown users (the default; `--trust-unknown-users` opts out),
  `--hba-file`, revocation lists, user name maps,
  `--tls-client-name DN` and reload are in (`crates/secantus-pg/src/tls.rs`,
  `scram_binding.rs`, `hba.rs`, `crl.rs`, `ident.rs`, `live.rs`). The three
  login comparisons against PostgreSQL 15 (`tools/probes/pg_login_options.py`,
  `pg_login_tls_files.py`, `pg_login_hba.py`) run in
  `tests/test_rust_pg_login_differential.py` (CI's `pg-oracle` lane; locally
  only with `SECANTUS_PG15_BIN` set). Open:
  - **Without a `pg_hba.conf` a role with no password is trusted** (with
    one, a `scram-sha-256` line refuses it). A user there is no role for is
    refused, as on PostgreSQL, unless `--trust-unknown-users` is given.
  - **With `--hba-file`:**
    - **Refused at startup, by line number**: the methods that need an
      outside service (`peer`, `ident`, `gss`, `sspi`, `ldap`, `radius`,
      `pam`, `bsd`) and `include` directives (PostgreSQL 16's).
    - **A `map=` on a method other than `cert` is refused**; PostgreSQL
      takes it for `ident`, `peer`, `gss` and `sspi` too, none of which is
      read here.
  - **TLS files:**
    - **A revocation list's issuing distribution point named relative to
      its issuer is refused at startup** (`crl.rs`); one with a full name,
      a delta list and a reasons-only or indirect list answer as
      PostgreSQL 15.19 does.
    - **A self-signed root is never checked against the revocation lists**,
      as OpenSSL 3.6 has it (`check_cert_crl` skips it). PostgreSQL linked
      against an older OpenSSL (3.0.13 on Ubuntu 24.04) checks the root as
      well, so a list scoped to user certificates or to a distribution point,
      issued by the root, refuses every chain there and none here. The login
      probe issues its scoped lists from an intermediate, where the two
      versions agree.
    - **A user name map's patterns are read as the SQL `~` operator reads
      them** (the `regex` crate, through `pg_regex_source`'s POSIX-class
      translation, 2026-10-15), so they share that engine's distance from
      PostgreSQL's advanced regular expressions.
  - **The libpq scenarios (`attempt`) in the login probes do not retry**
    when the answer is the close; `raw_first` does (up to four times), after
    `pg_login_tls_files.py` once failed 479 of 480 in CI when the client
    read PostgreSQL's close before its `unknown ca` alert. None of the libpq
    scenarios has shown the race.
  - **The TLS gauge run sets `PGSSLMODE` for libpq, so a test that names
    its own `sslmode` is not moved onto TLS by it**; and a test that fails
    in BOTH the plain and TLS runs is printed and does not fail the
    `psycopg-tls.yml` lane, which is the plain gauge's number and has no
    Linux lane that gates it.
  Backlog-ID: RP-013
  Verify: none -- what is left needs an outside service (peer, ident, gss, sspi, ldap, radius, pam, bsd), or is the regex engine, a relative-name distribution point, or probe tooling; the login comparison (tests/test_rust_pg_login_differential.py) pins the rest

## 5. Known bugs and edge cases to watch

- [ ] **RUST pgserver: the planner's choices that decide row order are only
  partly modelled.** **Decided (Joe, 2026-10-16): build a cost model** --
  enough of PostgreSQL's planner to make its choices, since the order rows
  come out in (and ties under ORDER BY) is the order of the plan it picks.
  Built so far (`secantus-pgplan/src/hashagg.rs`, 2026-10-16): a GROUP BY
  of bare columns over one table, with no WHERE and no grouping sets, comes
  out in PostgreSQL's HashAggregate order -- its estimate of the groups over
  a table with no statistics (`estimate_rel_size`'s 10-page minimum,
  `estimate_num_groups`' defaults), its tuple hash, and simplehash's
  placement and iteration -- and an ORDER BY over it sorts with
  PostgreSQL's quicksort. So does a `SELECT DISTINCT` of bare columns
  (a HashAggregate over the select list), and both under a WHERE, whose
  rows and groups are estimated with the planner's default selectivities
  (`filter_selectivity`: equality, `<>`, ranges, `IN`, AND, OR). And the
  choice between that HashAggregate and a Sort + Group (or a Unique) is
  PostgreSQL's (`secantus-pgplan/src/costsize.rs`, 2026-10-16): the
  cheapest scan priced as a sequential, bitmap heap or index scan (the
  heap's real pages, `btcostestimate`, Mackert-Lohman), `cost_sort`,
  `cost_group`, `cost_agg`, and `add_path`'s 1% fuzz, under which the
  sorted path's order wins a tie; a GROUP BY holding the whole primary key
  groups by it alone; an ORDER BY over the group keys moves them first;
  an aggregate with DISTINCT or its own ORDER BY is never hashed; and
  under a LIMIT with no ORDER BY a Group over an index scan in the keys'
  order is weighed too, by the cost of the first rows the LIMIT takes.
  186 of 186 shapes match 15.19 at 40, 300 and 3,000 rows. A table
  ANALYZE measured is modelled too (2026-10-16): ANALYZE records each
  column's `pg_statistic` row as `compute_scalar_stats` computes it from a
  sample of the whole table (`stanullfrac`, `stadistinct`, `stawidth`;
  `hashagg::analyze_column`) and `relpages` / `reltuples`, as a snapshot,
  and the group estimate reads them (`get_variable_numdistinct`); the
  hash table's growth is simplehash's (an integer threshold, checked on
  every row's lookup). 212 of 212 shapes over analysed int, text, bool,
  float8, char(n), varchar and NULL-bearing columns match 15.19 at 20 to
  10,000 rows, and again with half as many rows, and four times as many,
  inserted after the ANALYZE. A WHERE over an analysed table is
  estimated too (2026-10-16, `secantus-pgplan/src/colstats.rs`): ANALYZE
  keeps each column's most common values, histogram and correlation as
  `compute_scalar_stats` builds them, and a WHERE of comparisons with
  constants (`=`, `<>`, `<`, `<=`, `>`, `>=`, AND) gets `var_eq_const`'s
  and `scalarineqsel`'s selectivities (the MCV and histogram split, the
  histogram's ends read from an index as `get_actual_variable_range`
  does, `clauselist_selectivity`'s range pairs); an index scan's cost
  reads the column's correlation (`cost_index`), a Group over an index
  scan in the keys' order is weighed with or without a LIMIT, and the
  groups reach the hash table in the heap's order. 120 of 120 such shapes
  match 15.19 at 20 to 10,000 rows, 168 of 168 and 84 of 84 with rows
  inserted after the ANALYZE. So do `IN` lists (`scalararraysel`), `OR`s
  (`clauselist_selectivity_or`, with a BitmapOr path priced where each
  arm's index quals are on one column leading one index:
  `cost_bitmap_or_node`), `IS [NOT] NULL` (`nulltestsel`) and a measured
  WHERE under a LIMIT, and a non-unique integer index whose duplicates
  deduplication merges onto one leaf is sized so (`_bt_dedup_pass`):
  144 of 144 such shapes, 72 of 72 with rows inserted after the ANALYZE,
  and 28 of 28 deduplicated-index shapes at 300 to 3,000 rows. Batch 15
  (2026-10-16) added `NOT IN` (`<> ALL`), `IN` lists of nine or more (a
  hashed ScalarArrayOpExpr: a startup of a hash a value, a hash and a
  comparison a row) or with a repeated value, `OR`s as `canonicalize_qual`
  leaves them (`pull_ors`, `process_duplicate_ors`), null tests as index
  quals, one bitmap heap scan over `choose_bitmap_and`'s pick of every
  bitmap path (BitmapAnds inside a BitmapOr arm and beside one), a plain
  index scan's rows in its index's order, and VACUUM: `relpages`,
  `reltuples` and `relallvisible`, the visibility map's count stored by a
  later ANALYZE or CREATE INDEX (a row inserted since clears its page),
  and index-only scans priced with the all-visible fraction when the index
  holds every column read. 88 of 88 such shapes; 258 of 300 VACUUM shapes
  at 20 to 3,000 rows against a private 15.19 cluster (VACUUM cannot run
  in a transaction block), the other 42 each the deduplicated-index
  decline below (checked shape by shape), keeping the order they had
  before. Not yet:
  - An `OR` in the WHERE (a BitmapOr path), which keeps the HashAggregate
    unpriced (and is declined under a LIMIT); a DISTINCT with an ORDER BY,
    whose sort is not weighed; an ORDER BY with a LIMIT.
  - A WHERE with a pattern, `IS NULL` or an expression (declined).
  - GROUP BY an expression, over a join or a subquery, and past about
    50,000 estimated groups, where the HashAggregate's spilling is priced.
  - A table whose rows are not text, bytea or fixed width (numeric, arrays,
    json): its heap pages are taken at the estimated density.
  - DISTINCT ON, and a citext DISTINCT, which PostgreSQL sorts.
  - The size
    CREATE INDEX and an ALTER TABLE that builds an index record and what
    ANALYZE stores are modelled (`__sql_relstats__`), but not REINDEX's
    or CLUSTER's, nor across RENAME.
    Declined over an analysed table, keeping the old order: a range over
    a column that is not an integer (a text histogram is in the
    reference's locale: `select g from t3 where t > 'x2' group by g`), or
    equality on a text column whose repeated values overflow the MCV
    list; a hashed list inside an `OR`; an arm holding an `OR` of its own
    in a BitmapOr, or one whose index quals the WHERE's other clauses
    would join (`build_paths_for_OR`); a qual on a column that only a
    later column of an index holds; two bitmap paths tied in cost and
    selectivity; a statement whose columns read are not known (an
    expression, a HAVING, an aggregate's FILTER) over a vacuumed table; an
    index-only scan of an all-visible heap through a deduplicated index
    past one leaf, whose size is not modelled (`select distinct g from ti
    limit 3` at 1,500 rows); a
    join on its columns (`eqjoinsel` over its most common
    values); a key of a type whose statistics are not modelled (dates,
    numeric, arrays); extended statistics (`CREATE STATISTICS`); a table
    of more rows than ANALYZE samples (30,000), whose sample is random.
    A deduplicated non-unique index past one leaf keeps
    `btree_size`'s undeduplicated estimate, as without statistics, where
    the heap's pages dominate. VACUUM FULL is declined, and so is a
    vacuumed table that now holds fewer rows; an UPDATE or DELETE after a
    VACUUM clears visibility-map bits this model does not see. A NULL in
    an `IN` list is dropped by the lowering, where PostgreSQL still counts
    it in the list's length (its cost, and the nine for hashing). A
    ROLLBACK undoes ANALYZE's `relpages` / `reltuples` here, which
    PostgreSQL writes in place.
  - Joins: the join method (`secantus-pgplan/src/joincost.rs`: hash and
    merge joins priced as PostgreSQL does) decides whether a cursor over
    `a [LEFT] JOIN b ON a.x = b.y ORDER BY a.x` scrolls (at a cursor's
    tenth of the rows; 96 of 96 shapes match 15.19; this was RP-043), and
    the order a join of two tables on one equality emits its rows: a
    merge join's sides in key order, ties as each side's quicksort leaves
    them; a hash join's matches in its bucket's order (33 of 33). Under a
    WHERE each side is filtered by its own conjuncts before it is put in
    order, the plan is priced with them (either side outer, `add_path`'s
    startup costs, and under a LIMIT the fractional cost, where a nested
    loop over a materialised inner usually wins), and a WHERE no
    NULL-extended row passes makes a LEFT join inner (159 of 159 shapes
    at 20, 300 and 3,000 rows). A join on one expression equality
    (`on a.x = b.y + 3`, either side or both) is sorted or hashed by each
    side's value, priced with no statistics for the expression and its
    operators in the clause's cost; a LIMIT straight over a general join
    is priced for its rows (57 of 57). The ON's and WHERE's equalities
    form PostgreSQL's equivalence classes: one with a constant filters
    every member's side by it and leaves an inner join no join clause (a
    nested loop over every pair; a LEFT join's outer constant filters the
    inner side and its ON then selects every pair), one without filters
    each side by the equalities among its own members and joins on its
    first member each side (102 of 102). A chain of three stored tables
    on two column equalities, inner or LEFT, is planned by PostgreSQL's
    join order search (`secantus-pg/src/join_search.rs`): each pair a
    clause joins, then each with the third, in one `add_path` list, in the
    orders the outer joins allow; a LEFT join's hash right join returns
    the rows nothing matched in its hash table's order, two tables or
    three. An inner chain whose clauses share a key is one equivalence
    class over the three tables: every pair joins on its derived clause,
    and a merge join reads a pair already merged on the class unsorted,
    materialised (integer keys only). 83 of 86 shapes at 20, 300 and
    3,000 rows; the three left are a merge right join. A chain of four
    stored tables joined INNER, each on one column equality to a table
    before it with no column in two clauses, runs the same search over
    four relations (`secantus-pg/src/join_order.rs`), bushy pairs
    included, INNER or LEFT: a LEFT join's `SpecialJoinInfo` limits the
    orders as `join_is_legal` does, an inner join above makes the LEFT
    joins below it whose nullable table its clause reads inner, and a
    LEFT join's right join (hash or merge) runs with its preserved side
    hashed or merged in (71 of 71 four-table shapes at 20, 300 and 3,000
    rows). A merge right join returns the rows nothing matched in their
    key's place, three tables or four. An INNER chain of four whose
    clauses share a key is one equivalence class over three or four of
    them (2026-10-19): its members in `process_equivalence`'s order (each
    ON as written, `JoinNode::Join::equi_reversed`), any two relations
    holding members joined on its derived clause, and a merge join on it
    read unsorted by a merge join above on it, its paths kept apart from
    paths in another class's order (`JoinRel::add_paths_on`); 135 of 135
    shapes at 20, 300 and 3,000 rows, the 108 tree shapes unchanged. The
    same search runs chains of five to eight tables (2026-10-19; eight is
    `join_collapse_limit`, past which PostgreSQL does not flatten the
    JOINs and the search declines): 144 of 144 tree shapes of five, six
    and eight tables at 20 and 300 rows. A join's order counts only while
    something above can use it (`truncate_useless_pathkeys`, 2026-10-20):
    an edge's once its clause is inside the relation, the class's once
    every member is, so a path merged on an edge no longer stands beside
    an equally cheap one in the class's order and decides a tie; a class
    over a chain of five went from 12 to 21 of 27 shapes at 20, 300 and
    3,000 rows, every other shape unchanged. A hash join too big for
    `work_mem` x `hash_mem_multiplier` (4 MB x 2) is split into batches
    (`ExecChooseHashTableSize`, 2026-10-20): each join relation carries
    its row WIDTH, the columns still needed above it (the query's, a
    clause's key to a table outside it, the class's members while one is
    outside; `JoinRows::needed`), the batches' page I/O is priced
    (`initial_cost_hashjoin`, virtual buckets in `final_cost_hashjoin`),
    and the rows come out batch by batch, an outer row hashing to a later
    batch joined after every row of batch 0 (`ExecHashGetBucketAndBatch`);
    the class over five tables 27 of 27. A hash RIGHT join split into
    batches returns each batch's unmatched rows after its probes, which is
    not modelled: the search declines it. A RIGHT join is planned as the
    LEFT join of its sides swapped (`reduce_outer_joins` flips
    `JOIN_RIGHT`, 2026-10-20), and a WHERE that makes its LEFT side
    non-null makes it inner and is pushed to that side's scan; of 33
    RIGHT-join probes at 20 and 300 rows, 24 match (8 before). The search
    takes any tree of INNER and LEFT joins, not only a left-deep chain
    (2026-10-20): a LEFT join whose nullable side is an inner join of
    tables keeps that side whole (`make_outerjoininfo`'s
    `min_righthand`), and `reduce_outer_joins` runs top down over the tree
    (three tables when bushy, four to eight in any shape). Bushy trees of
    three to five tables: 21 of 24 probes at 20 and 300 rows (none
    before); RIGHT joins 27 of 33. A LEFT join whose nullable side holds
    another LEFT join is searched too (2026-10-25): `make_outerjoininfo`
    widens its `min_lefthand` / `min_righthand` by the lower outer join's
    when the clause reads its nullable side, and a LEFT join's ON makes
    the tables it reads non-null inside its nullable side
    (`reduce_outer_joins`): 18 of 20 nested shapes of three and four
    tables at 20 and 300 rows, the two left a class with a LEFT join
    (below). A FULL join of two stored tables on one column
    equality is planned too (2026-10-25): a Merge or Hash Full Join,
    either side outer (`add_paths_to_joinrel` with no nested loop), the
    hashed side's rows nothing matched last in its hash table's order, a
    merge's in their key's place (a NULL key's when its scan reaches it);
    a WHERE that cannot hold for a NULL-extended side makes it a LEFT,
    RIGHT or inner join (`reduce_outer_joins`), and an ALTER TABLE that
    builds an index records the table's size as CREATE INDEX does
    (`index_update_stats`). 56 of 56 shapes at 20 to 3,000 rows a side,
    with and without a primary key and NULL keys (none before). A join's
    ON reduces the outer joins below it as `reduce_outer_joins` does
    (2026-10-25): an inner join's strict ON passes the tables it reads to
    both its sides, a LEFT join's to its nullable side, so a FULL, LEFT or
    RIGHT join there whose nullable side holds one is made LEFT or inner
    before the search; 22 of 24 three- and four-table shapes (10 before),
    the two left a FULL join nothing reduces. A FULL join that stays FULL
    in a chain of three tables is searched too (2026-10-25): a FULL join's
    minimum sides are its whole ones, so its pair joins first, priced and
    run as two tables' FULL joins are; 48 of 48 shapes at 20 to 3,000
    rows, with and without NULL keys (6 before); so is one in four to
    eight tables, whose search carries a FULL join's `SpecialJoinInfo`
    and never commutes it (`join_is_legal`); 36 of 36 shapes of four and
    five tables (4 before). A WHERE that makes one side of a FULL join
    non-null is pushed to that side's scan, which `reduce_outer_joins`
    has made preserved; a FULL join whose ON holds more than its equality
    is a Hash Full Join only, as `select_mergejoin_clauses` allows no
    merge join a join filter (20 of 20 two-table variants: WHERE, LIMIT,
    ORDER BY, USING, ON filter). A FULL join over an analysed table is
    declined with the other joins over measured columns (below). A class
    beside outer joins is searched too (2026-10-25): only inner joins'
    equalities make the class (an outer join's clause is delayed,
    `distribute_qual_to_rels`), each inner ON the class stands in for is
    checked where its tables meet, below any outer join that could
    NULL-extend them, and an outer join's key that is a class member reads
    a side merged on the class unsorted, a LEFT join merged on its
    preserved member coming out in the class's order; 18 of 18 shapes of
    four and five tables (none before), nested LEFT joins 20 of 20 (18).
    A join of two tables on two to four column equalities is priced and
    run too (2026-10-25), INNER (two or more classes spanning both sides)
    or LEFT: each clause's selectivity multiplied, each `=` costed, the
    smallest hash bucket of the clauses, both sides sorted on every key,
    and a hashed side in its table's order by the keys' combined hash
    (`ExecHashGetHashValue`); 54 of 54 shapes at 20 to 3,000 rows, with a
    primary key and with NULL keys (14 before). Not yet: such a join in
    three or more tables, or FULL; an expression in a class; a LEFT join's class through the general path;
    a class of text or other non-integer keys (a WHERE equality over two tables is a join clause since 2026-10-26,
    the join graph a cycle where it closes one, two relations joined on
    every clause between them, a class's clause leading, and an inner
    join's further key pairs edges too: 14 of 14 cyclic shapes of three
    and four tables, none before; a WHERE conjunct that is no equality is
    searched since
    2026-10-25: the top join's join filter, or the filter of a pair joined
    first, priced with `clause_selectivity`'s defaults -- `DEFAULT_INEQ_SEL`,
    `neqjoinsel`, IS DISTINCT FROM's, `var_eq_const`'s, an OR's -- and an
    operator a row; 117 of 117 shapes, 9 before); four or more tables
    with a WHERE over two of them; more than eight tables; an expression key with a function
    call or a cast that costs; a join on two equivalence classes (`a JOIN
    b ON a.b = b.a JOIN c ON a.c = c.a JOIN d ON a.b = d.a JOIN e ON a.c =
    e.a`: 283 of 288 no-LIMIT shapes match, the 5 left all such); a LIMIT
    with an ORDER BY over two tables, which the narrow join path plans (84
    of 108); a join of more than 200,000 rows a side, which streams in
    scan order.
  - A LIMIT over three to eight tables (and two whose WHERE holds a join
    filter) is planned for its first rows (2026-10-26): every relation
    keeps its whole pathlist with startup costs counting, base tables'
    paths include whole-index scans in a key's order and index and bitmap
    scans parameterized by a clause's other table (Memoize, inner-unique
    nested loops), and the plan is the cheapest under a Limit, or with an
    ORDER BY of one integer column the cheapest of the presorted paths and
    a bounded Sort. Join relations parameterized by an outer table
    (`param_source_rels`, an outer join's) are built and run per outer
    row; index sizes come from the B-tree model; a hash table that
    outgrows its buckets is rebuilt in chunk order
    (`ExecHashIncreaseNumBuckets`); the search keeps whole pathlists with
    no LIMIT too, falling back to the old search where it declines. 843 of
    843 LIMIT shapes of three to five tables at 20 to 3,000 rows (514, then
    812 before). Declined: other ORDER BYs.
  - Sorts that spill to disk runs are not PostgreSQL's merge.
  Backlog-ID: RP-028
  Verify: tests/backlog/test_rust_postgres_gaps.py::test_a_join_on_two_equivalence_classes_is_searched

- [ ] **OPEN — RUST pgserver on LINUX: psycopg's timing tests fail in CI
      and pass on macOS (first measured 2026-10-12).** No Linux lane ran
      psycopg's suite against the Rust PostgreSQL server until
      `.github/workflows/psycopg-tls.yml`. Its first two runs, on
      `ubuntu-latest`, 5,788 tests each (80 skipped, 34 xfailed, 3
      xpassed, 1 deselected); the same tree on macOS has no failure:

      | run | in the clear | over TLS |
      | --- | --- | --- |
      | 1 | 6 fail, 1,689 s | cut off at 98% by the runner's 1,800 s backstop |
      | 2 | 31 fail, 2,492 s | 14 fail, 2,067 s |

      - **The NOTIFY timing tests fail in every run, in either
        transport**: `test_notify.py::test_notify` and
        `::test_notify_timeout`, their `test_notify_async.py` twins, and
        in run 2 the four `test_notify_query_notify` cases. Each sends a
        NOTIFY 0.25 s after it starts, from a second connection it opens
        then, and asserts when it arrives: 0.306 to 0.328 s measured,
        where 0.25 +/- 0.025 or 0.05 is allowed. So 55 to 80 ms late.
        Not diagnosed. The server wakes an idle listener by a
        `tokio::sync::Notify` and sets `TCP_NODELAY`, so nothing polls;
        what is left is the time a debug build takes to accept the second
        connection and run the NOTIFY on a shared runner. Measure that on
        a quiet Linux box, and with a release build, before calling it
        the runner.
      - `test_concurrency.py::test_notifies` is the same measurement and
        fails in some runs and not others.
      - **`test_cursor_async.py::test_leak` failed 22 of its 72 cases in
        run 2 in the clear, and none in run 1 or over TLS.** It counts
        Python objects in the client; it also failed twice on macOS on a
        loaded machine and passed three times alone.
      - `test_concurrency_async.py::test_type_error_shadow` hit the 20 s
        per-test timeout in all three completed runs.

      - Later runs each failed something else once: 3 and 5, then 10 and
        10 (PR #1905), where
        `test_concurrency_async.py::test_eintr[ITIMER_REAL-SIGALRM]` failed
        over TLS only. Its client script was killed by its own interval
        timer, which fires every 0.25 s and can land while the interpreter
        is shutting down; nothing the server answered.

      - **Measured 2026-10-19: both steady failures track the SIZE OF THE
        CATALOG, which the gauge grows as it runs.** The 2026-10-18
        scheduled run still failed the same eight (NOTIFY 0.36 to 0.38 s,
        so ~120 ms late, steady, not noise; `test_notify_query_notify`'s
        third notification missing, which is two connects in series
        running past the listener's window; `test_type_error_shadow` at
        20 s). On the development Mac, debug build: a new connection's
        first statement took 1.4 ms on an empty catalog and **620 ms with
        250 tables** (plus 63 each of types, functions and views), and an
        INSERT ran at 250 rows/s against 3,500 -- `test_type_error_shadow`'s
        script took 159 s against PostgreSQL's 8. Causes, each fixed:
        every reader of the table catalog decoding the whole of it on a
        new thread (`all_table_defs` now reads `committed_table_defs`'s
        shared copy outside a transaction block); `relation_oid` scanning
        the catalog per relation, a view's miss falling through to every
        view (quadratic); the function catalog re-normalised and copied
        per statement (`type_catalog_docs`, `may_call_user_function`);
        `views()` sorting a copy of every definition per statement for the
        privilege check; `any_temp_function`, `session_has_temp` and the
        user type / composite lookups scanning per call. After: first
        statement 45 ms at 250 objects and 90 ms at 500 (linear: the
        per-connection install of the planner's catalog tables, debug
        build), INSERT 3,131 rows/s at 500 objects against 3,331 empty,
        the shadow script 18 s against 16 s empty. **On Linux (run
        39321598460, PR #2030's tree) every NOTIFY test passed in the
        clear and over TLS**, where the 2026-10-18 run of main failed
        seven; only `test_type_error_shadow` still hit 20 s.
      - **Measured 2026-10-24: inserts slowed about 30% since
        2026-10-22**, and the test failed in both transports again on PR
        #2142's run. Three connections inserting 10,000 UUID-keyed rows
        each, debug builds on the development Mac: 1.70 s at `5a5634cd`
        (2026-10-22), 2.20 s at `d0b2135b` (batch 54), 2.27 s with the
        cumulative statistics (batch 55, about 3% of it). Not yet bisected
        within batches 26 to 54; a profile shows the server mostly waiting
        rather than computing.
      - **`test_type_error_shadow` is throughput**: 150,000 prepared
        UUID-keyed INSERTs from three connections (8 s on PostgreSQL here).
        Profiled 2026-10-19, debug build: every Describe and Execute
        parsed the statement again to ask whether it was a FETCH (a
        quarter of the time; now only once the catalog has moved), the
        plan cache refused a `uuid` parameter so every row was planned
        again (a third; `plan_cache` now templates one, since Bind has
        made it canonical), and the type lookups canonicalised the same
        names per value. The script went from 18.7 s to 11.6 s (PR
        #2032), and to about 9.5 s once each statement's subquery check
        and a uuid parameter's decode stopped repeating work (2026-10-20).
        **Measured on Linux after #2032 (run 39337247272): still over 20 s
        in both transports** -- that runner takes about 1.6 times as long
        as the development Mac over the whole gauge. **What bounds it now
        is storage's per-collection write lock** (`Storage::insert_chunk`'s
        `coll_lock`): every insert into one table holds it across its
        statement transaction, which re-reads the collection's index
        descriptors, options, timeseries flag and UUID from WiredTiger and
        writes the oplog entry, so the three connections insert one at a
        time (the lock's wait rose from 7% to 12% of a statement as the
        server's own work shrank), where PostgreSQL inserts them in
        parallel. **Decided (Joe, 2026-10-20): shorten that critical
        section by caching the collection's metadata in storage**, not by
        building the CI lane in release. Done the same day: a write reads
        its collection's registry row and index descriptors once per
        catalog generation (`index_descs_cached`, `coll_options_cached`,
        and `list_indexes`, which the PostgreSQL server asks on every row
        for expression indexes), under the rule the planner's cache already
        relies on -- a transaction uses an entry only while the generation
        its snapshot was taken under is still the settled one, and any
        catalog write in flight, its own included, settles nothing. The
        shadow script went from 9.8 s to 8.4 s (PostgreSQL 15.19: 8.0 s).
        Linux after that (PR #2040's psycopg-tls run): still over 20 s, in
        the clear and over TLS (the lane stays green: it fails only on a
        TLS-only failure). **Decided (Joe, 2026-10-20): keep optimising**,
        rather than build the lane in release or stop. Measured then: the
        collection lock was held about 88% of the run, so only work under
        it counts. Started, NOT landed: branch `rust-pg-batch24` (commit
        ac15e512, marked WIP) prepares each document's decode, minted
        `_id`, `_id` key and oplog `o2` before the lock and skips the
        per-entry `local` check an insert's namespace answers -- 8.2 s to
        7.6 s on the script; its storage gate was interrupted and nothing
        else has been run on it.

      - **Measured 2026-10-27, the top-ten batch: PostgreSQL itself does
        not fit `test_type_error_shadow`'s 20 s on that runner.** The
        script, timed on `ubuntu-latest` (4 vCPU, 16 GB; a throwaway
        workflow run, 40456257780) in the gauge's own environment (psycopg
        3.3.4, binary implementation): the runner's PostgreSQL 16.15
        (`synchronous_commit` on, `fdatasync`) 23.4, 24.6 and 24.0 s; this
        server's debug build 55.5, 51.5 and 54.7 s, about 2.8 cores of
        server CPU throughout. The batch cut the debug server's work on
        the script by 28% in instructions retired and 31% in CPU
        (`changelog.d/rust-pg-rp014-insert-cpu.md`: a shared cached
        `TableDef`, an insert's decode and `_id` work before the
        collection lock, an insert's heap reads only where PostgreSQL's
        B-tree insert reads, Bind's parameters reused at Execute, storage
        given the decoded rows, one B-tree descent fewer per unique
        insert), and the test still hit 20 s in both transports (run
        40446794397) -- as it must, when PostgreSQL takes 24 s there. So
        no server change can make this test pass on that runner; what is
        left is this server's 2.2x over PostgreSQL there (debug build
        against PostgreSQL's optimised one). **Decision for Joe**: deselect
        it on Linux by name with that reason, as Windows already does
        (PostgreSQL 30 s there), or keep it as a standing failure the
        lane does not gate on.

      What the lane gates, given that: a run that stopped short, and a
      test that fails over TLS, passes in the clear, **and fails over TLS
      again when run again by itself**. `::test_notify*`,
      `::test_notifies` and `::test_leak` are left out of that by name
      (`SHARED_RUNNER_NOISE` in `psycopg_validation/check_run.py`, applied
      only with `--shared-runner`), because they fail in most runs in
      either transport. **So a change that delays NOTIFY over TLS only
      would not fail that lane**, and neither would a TLS fault that shows
      once in two tries; `./inv validate-psycopg-rust --tls` on a
      developer's machine holds every test to one run. Nothing gates the
      plain number on Linux.
      Backlog-ID: RP-014
      Verify: none -- CI-runner timing flakes in psycopg's own suite on Linux

- [ ] **OPEN — RUST pgserver on WINDOWS: a COPY between two connections
      once failed 40001 (2026-10-25).** In the Windows psycopg gauge of
      PR #2175 (run 40217508289, attempt 1), all three variants of
      `tests/test_copy_async.py::test_copy_table_across` failed at once
      with `SerializationFailure: could not serialize access due to
      concurrent update`. The test creates a table with a serial primary
      key and 20 random columns in each of two open transactions, then
      copies one into the other. PostgreSQL never fails it, since the two
      transactions write nothing in common. Every WiredTiger write conflict
      reaches the client as this 40001 (`storage_err`), including a
      transaction WiredTiger rolls back to relieve its cache. Batch 79's
      run passed the test, and so did attempt 2 of the same commit (4846
      passed). It did not reproduce locally on macOS: the test alone 90
      times, under 60 seeds, with autovacuum waking every second, the whole
      file under eight random orders, and a 4-thread stress of the same
      pattern. One way this server raised that 40001 where PostgreSQL does
      not was found and fixed in batch 82. A CREATE TABLE of a name another
      open transaction was creating failed at once. PostgreSQL waits for the
      other, then raises 23505 on `pg_type_typname_nsp_index` or goes on
      (`tests/backlog/test_rp_batch82.py`). The test's two connections
      create different names, though, so that does not explain this run.
      The next step is the server's log from a failing Windows run: since
      batch 81 the gauge keeps it
      (`.validation/psycopg-raw-rust.server.log`, in the job's artifact).
      Backlog-ID: RP-268
      Verify: none -- not reproduced outside one Windows CI run; no test fails on demand

## Found by the 2026-10-20 audit

A review of the PG crates, each finding reproduced against a `secantusd-pg`
built from tree `83e845e5`. The gap tests are in
`tests/backlog/test_rp_found_2026_10_20.py`. Their expected answers are
literals measured on a PRIVATE PostgreSQL 15.19 cluster (`initdb --locale=C
-E UTF8`), because most scenarios create databases, roles or committed
tables, which the shared reference must not be given. Each literal was
re-proved on a fresh cluster before filing.

Not filed:
- `vacuumdb` exits 0 now, although the audit named it.
- The EXPLAIN ANALYZE option and replication-login findings are scope
  questions.
- The slow 1M-row block cursor needs a release-build measurement first.


## Found by batch 30 (2026-10-22)

Found while building `REASSIGN OWNED` and `DROP OWNED` (RP-072). The gap
tests are scenarios in `tests/backlog/test_rp_batch30.py`, measured on a
private 15.19 cluster. RP-133 .. RP-136 and RP-138 .. RP-142 were found and
fixed in the same batch; their tests there are regression tests.

## Found by the top-ten batch (2026-10-26)

Found while speeding up the `information_schema` views (RP-131), by
comparing `routines`, `parameters` and `element_types` with PostgreSQL 15.19
row for row. The tests are in `tests/backlog/test_rp_catalog_identity.py`,
measured with read-only queries on 15.19. Fixed in the same batch, their
tests regression tests:
- RP-270: 56 built-in `pg_type` rows differed from `initdb`'s: a
  fixed-length subscriptable type's `typelem` (`name`, `int2vector`,
  `oidvector`, `point`, `lseg`, `box`, `line`, `_record`), a range array's
  `typtype`, `_box`'s `typdelim`, ten types' `typcollation`, and NULL
  `typnotnull` / `typbasetype` / `typtypmod` on 26 pseudo-types. So
  `information_schema` called `int2vector` a plain type, not `ARRAY`.
- RP-271: `format_type(18, NULL)` was `character` (`"char"` is meant), and a
  modifier on a type with no `typmodout` was dropped (`format_type(25, 7)`
  is `text(7)`).
- RP-285 (found by RP-254's work): `pg_class.relam` was `btree` for every
  index, so a join to `pg_am` named a GIN, GiST or hash index's method
  wrong (`tests/backlog/test_rp_catalog_identity.py`).
- RP-286 (found by RP-202's): a `pg_sleep` in the select list of a streamed
  SELECT (`select a, pg_sleep(2) from t`) did not look for a cancel and
  slept to its end; a row streamed after the statement's hooks are gone
  is evaluated under a cancel hook now (`tests/backlog/test_rp202_cancel.py`).

- [ ] **The built-in relations' `pg_attribute` rows differ from
      PostgreSQL 15.19's.** Over `attrelid < 16384`: the 648 system-column
      rows (`attnum < 0`: `ctid`, `xmin` ... of every catalog with storage)
      are missing; 684 `information_schema` view columns name their domain's
      base type in `atttypid` where PostgreSQL names the domain
      (`sql_identifier`, `cardinal_number` ...); 282 `attcollation`s are
      the default collation or none where a catalog's collatable column is
      `C` (950); 59 array columns have `attndims` 0, not 1; and 40
      `attstorage`s differ (a TOAST table's `chunk_data` is `p`). So
      `element_types` lists the catalogs' text arrays without their
      collation. Changing `atttypid` is the delicate part: what a client is
      told a view column's type is follows it.
      Backlog-ID: RP-272
      Verify: tests/backlog/test_rp_catalog_identity.py::test_builtin_relations_attributes

- [ ] **In autocommit, a join whose select list calls `nextval` takes one
      value more than its rows**: `select a.a, nextval('s') from a, b where
      a.a = b.b` answers 2, 3, 4 and leaves `last_value` at 4 where
      PostgreSQL 15.19 answers 1, 2, 3; in a transaction block it does not.
      Found while working RP-019, which made a volatile WHERE condition run
      once a row of its table as PostgreSQL's scan does.
      Backlog-ID: RP-288
      Verify: tests/backlog/test_rp_top10_found.py::test_a_select_list_nextval_in_an_autocommit_join_runs_once_a_row
- [ ] **`DEFERRABLE` on a primary key is dropped**: `primary key (a, b)
      deferrable` records `condeferrable` false (15.19: true).
      Backlog-ID: RP-281
      Verify: tests/backlog/test_rp_top10_found.py::test_a_deferrable_primary_key_is_deferrable
- [ ] **A plain SELECT's WITH RECURSIVE is planned without PostgreSQL's
      checks**: a recursive term of another type, or one holding an
      aggregate, is answered (and without a LIMIT never ends) where 15.19
      refuses with 42804 / 42P19. Found while completing RP-203, whose
      CREATE VIEW now runs those checks first.
      Backlog-ID: RP-295
      Verify: tests/backlog/test_rp_top10_found.py::test_a_recursive_query_postgres_refuses
- [ ] **A hypothetical-set aggregate over two columns is refused**:
      `dense_rank(2, 'x') WITHIN GROUP (ORDER BY a, b)` is 0A000 here.
      Backlog-ID: RP-283
      Verify: tests/backlog/test_rp_top10_found.py::test_a_hypothetical_set_aggregate_over_two_columns
- [ ] **An UPDATE leaves the row's unique-key claim on its old value**
      (shared storage), so inserting that value again fails XX000 `E11000
      duplicate key error` where PostgreSQL 15.19 inserts it: the update
      path (`index_entry_diff`) never moves the row's claim in `UNIQ_TABLE`;
      only an insert writes claims and only a delete releases them. It also
      blocks a journal ROLLBACK TO that puts back two rows which swapped a
      unique value. The MongoDB server links the same code (not yet
      compared with mongod). Found while closing RP-018.
      Backlog-ID: RP-296
      Verify: tests/backlog/test_rp018_savepoint_undo.py::test_an_updated_unique_value_can_be_used_again
- [ ] **Pricing a key lookup reads the whole table** whenever the cached
      heap page count is invalid (after any CREATE or DROP, and inside
      transaction blocks): `priced_scan` -> `heap_pages` -> `heap_spaces` ->
      `table_docs` decodes every row, so a one-row SELECT or UPDATE took
      about 2.1 s on 200,000 rows (debug build) and about 1 s on 1,000,000
      rows (optimised). Found while closing RP-018.
      Backlog-ID: RP-297
      Verify: tests/backlog/test_rp018_savepoint_undo.py::test_a_key_lookup_after_ddl_does_not_read_the_table
- [ ] **An index created in the open transaction is no relation
      `regclass` finds**: `select pg_relation_size('t_b')` after `create
      index t_b` in the same block is 42P01; PostgreSQL 15.19 answers 8192.
      Found while working RP-028.
      Backlog-ID: RP-299
      Verify: tests/backlog/test_rust_postgres_gaps.py::test_an_index_made_in_the_open_transaction_is_a_relation

## Found by batch 78 (2026-10-25)

Found while working RP-255, which planning folded a function call around,
and fixed in the same batch; their tests in `tests/backlog/test_rp_batch78.py`
are regression tests:
- RP-266: an EXPLAIN without ANALYZE, and a PREPARE, ran a volatile
  function's side effects: they folded the call while planning, which only
  the plan that executes may do.
- RP-267: EXPLAIN's TIMING and SUMMARY options were ignored (and TIMING
  without ANALYZE was not refused), and its JSON lacked the actual times
  and the `Triggers` group.

## Found by batch 77 (2026-10-25)

Found while modelling hash indexes for SERIALIZABLE (RP-254): a hash index's
fill factor is a storage parameter, which this server dropped. The tests are
scenarios in `tests/backlog/test_rp_batch77.py`, measured on a private 15.19
cluster. Fixed in the same batch, their tests regression tests:
- RP-261: an index's `WITH (...)` was ignored: not checked against its
  access method, not in `pg_class.reloptions`, not printed by
  `pg_get_indexdef`. `ALTER INDEX ... SET` / `RESET` were refused.
- RP-262: `ROLLBACK TO SAVEPOINT` did not undo an index's CREATE, DROP,
  RENAME or ALTER. Undoing a DROP TABLE lost the table's indexes, and the
  COMMIT kept them lost.
- RP-263: a table's storage parameter given bare (`WITH (fillfactor)`) was
  "specified more than once", and one given twice was accepted. An
  out-of-bounds or enum value's error had no DETAIL.
- RP-264: a missing relation in `ALTER TABLE`, `INDEX`, `VIEW`, `SEQUENCE`
  or `MATERIALIZED VIEW` was reported with a position.
- RP-265 (batch 78): a constraint's index (`PRIMARY KEY` / `UNIQUE ...
  WITH (...)`, `ADD PRIMARY KEY USING INDEX`) lost its storage parameters,
  and `ALTER INDEX` on one was refused.

## Found by batch 75 (2026-10-25)

Found while building serializable snapshot isolation (RP-061), a port of
PostgreSQL 15.19's `predicate.c` checked against its own isolation specs
(`tests/backlog/test_rp_isolation.py`, traces recorded on a private 15.19
cluster). The open items' specs are gap tests there. Fixed in the same
batch, their tests regression tests:
- RP-257: `INSERT ... ON CONFLICT DO UPDATE ... WHERE` left the
  conflicting row unlocked when the WHERE rejected it. PostgreSQL locks it
  first, so a concurrent UPDATE waits.
- RP-258: a void SQL function answered the empty string, where PostgreSQL
  answers NULL. Only a void PL/pgSQL function answers the empty string.
- RP-259: `RAISE SQLSTATE 'P1234' USING MESSAGE = 'x'` was 42601 in a DO
  block. A second ERRCODE, MESSAGE after a format string, or a repeated
  HINT or DETAIL was accepted in a function. `USING ERRCODE` alone gave an
  empty message (`tests/backlog/test_rp_batch75.py`).
- RP-260: a REPEATABLE READ transaction took its snapshot at a SET, SHOW
  or LOCK. PostgreSQL takes it at the first statement that needs one.

- [ ] **What the GIN and GiST page models do not cover yet.** Since the
      top-ten batch (2026-10-26) GIN (`gin_index.rs`) and GiST
      (`gist_index.rs`, `point_ops` and `box_ops`) indexes are modelled page
      by page, so SERIALIZABLE takes PostgreSQL 15.19's SIREAD page locks
      on them (`predicate-gin` 31 of 31, `predicate-gist` 40 of 40; was 19
      and 22, RP-254). Not yet, each falling back to a relation lock as
      before:
      - `pg_relation_size` of a GIN or GiST index reads 0 though the model
        knows its pages;
      - GIN `=` on arrays, and opclasses other than `array_ops` (jsonb,
        tsvector, btree_gin); array elements other than ints, floats, bool
        and C-ordered text; an ANALYZEd array column (MCELEM statistics);
      - point/box `~=`, polygon and circle containment;
      - multi-column, partial and expression GIN / GiST indexes;
      - an UPDATE's or DELETE's scan (only a SELECT passes its residual);
      - VACUUM deleting emptied GIN posting-tree pages and GiST pages;
      - GiST's `gistchoose` breaks penalty ties with a per-backend random
        seed; the model uses a fixed one, so on overlapping `box_ops` data a
        tuple can land on another page than in a given backend (15.19 gave
        the same layout on three runs).
      Backlog-ID: RP-287
      Verify: tests/backlog/test_rp_top10_found.py::test_gin_and_gist_indexes_have_their_pages



## Found by batch 71 (2026-10-25)

Found while building REINDEX SCHEMA (RP-196), and fixed in the same batch;
its test in `tests/backlog/test_rp_batch71.py` is a regression test.
RP-249: a table outside `public` named its primary key after its
schema-qualified key (`rxs.t_pkey`, where PostgreSQL makes `t_pkey` in
schema `rxs`), so `pg_relation_size('rxs.t_pkey')` was 0. `REINDEX TABLE
rxs.t` was also 42P01, its relation never qualified.

## Found by batch 69 (2026-10-25)

Found while applying an UPDATE in its scan's order (RP-247), and fixed in
the same batch; its test in `tests/backlog/test_rp_batch69.py` is a
regression test. RP-248: an UPDATE's RETURNING listed each row's old
`ctid` (and `xmin`), where PostgreSQL 15.19 returns the new version's, and
listed the rows in storage order rather than the scan's.

## Found by batch 68 (2026-10-25)

Found while counting the heap buffers each write reads (RP-239). The gap
tests are scenarios in `tests/backlog/test_rp_batch68.py`, measured on a
private 15.19 cluster. RP-246 (`SELECT ... INTO t` answered its rows and
made no table, and an UNLOGGED CREATE TABLE AS made a logged one) was
found and fixed in the same batch; its test there is a regression test.


## Found by batch 66 (2026-10-24)

RP-245: a heap checkpoint ran at the end of a message cycle, before the
statement's answer was sent, so its durable writes held that answer back.
That included a cancelled statement's, which CI's cancel-latency
regression tests caught at 0.56 to 0.72 s against their 0.5 s budget. It
now runs on a thread of its own, as PostgreSQL's checkpointer is a process
of its own. It is found by those regression tests in
`tests/backlog/test_rp_triage.py` and `tests/backlog/test_rp_batch53.py`;
a latency budget has no gap test of its own.

## Found by batch 65 (2026-10-24)

Found while giving a COPY error its CONTEXT (RP-242), and fixed in the same
batch; its test in `tests/backlog/test_rp_batch64_copy.py` is a regression
test. RP-244: an integer in COPY's input past its type's range was 22P02
`invalid input syntax` where PostgreSQL answers 22003 `value "..." is out
of range for type integer`. A bigint literal past the range was 22P02 too,
and a float cast to bigint saturated instead of raising `bigint out of
range`.

## Found by batch 64 (2026-10-24)

Found while giving an MCV list over timestamps its datums (RP-099) and
while speeding up `pgbench -i`'s COPY. RP-240, RP-241 and RP-243 were fixed
in the same batch; their tests in `tests/backlog/test_rp_batch64.py` and
`tests/backlog/test_rp_batch64_copy.py` are regression tests.

- RP-240: under a session zone other than UTC, a `timestamptz` that an
  `INSERT ... SELECT` computed (`select timestamptz '...'`,
  `z + interval`), or copied as a `timestamptz[]`, was stored shifted by
  the zone's offset. So was each bound of `tstzrange(timestamptz, ...)`.
- RP-241: COPY FROM checked no NOT NULL, CHECK or foreign key constraint,
  and fired no trigger.
- RP-243: `new.d` in a statement-level trigger was 0A000, where PostgreSQL
  reads a NULL of the column's type.


## Found by batch 63 (2026-10-24)

Found while counting a sequential scan a LIMIT stops (RP-224). The gap
tests are scenarios in `tests/backlog/test_rp_batch63.py`, measured on a
private 15.19 cluster. RP-238 (a scan streamed in a transaction block,
`begin; select g from t`, was not counted in `pg_stat_user_tables` at all)
was found and fixed in the same batch; its test there is a regression
test.


## Found by batch 62 (2026-10-24)

Found while letting ON CONFLICT arbitrate on a unique index whose predicate
computes (RP-236), and fixed in the same batch; its test in
`tests/backlog/test_rp_batch62.py` is a regression test. RP-237: an
`ON CONFLICT DO UPDATE` whose proposed rows met one row twice (two rows of
one key, or a row and the one it had just inserted) updated it twice,
where PostgreSQL refuses with 21000.

## Found by batch 60 (2026-10-24)

Found while modelling the B-trees' pages (RP-196). RP-236 (a partial index
over a computed predicate) was closed by batches 61 and 62.

## Found by batch 59 (2026-10-24)

Found while drawing every oid from one counter (RP-137), and fixed in the
same batch; their tests in `tests/backlog/test_rp_batch59.py` are
regression tests. RP-234: a primary key could not be dropped (42704).
RP-235: a table made in an open block had no system columns (`ctid`,
`xmin` ... were 42703 until the block committed).

## Found by batch 58 (2026-10-24)

Found while building the heap's persistence (RP-193). RP-233 (two
databases' tables shared one heap model, their oids each database's own;
a dropped database's heaps outlived it) was found and fixed in the same
batch; its test in `tests/backlog/test_rp_batch58.py` is a regression test.

## Found by batch 57 (2026-10-24)

Found while building the extensions' members (RP-105), and fixed in the
same batch; their tests in `tests/backlog/test_rp_batch57.py` are
regression tests. RP-228: `pg_get_function_arguments`,
`pg_get_function_identity_arguments` and `pg_get_function_result` of a
built-in function were NULL. RP-229: `pg_get_functiondef` of a built-in
was NULL, and of an aggregate not PostgreSQL's 42809. RP-230: an operator
class or family hid one of its name in another access method, so
`pg_describe_object` qualified it. RP-231: a public function a built-in of
its name and arguments hides printed unqualified as a `regprocedure`.
RP-232: a reserved word as a `regprocedure`'s argument type (`count(any)`)
was 42883 where PostgreSQL's grammar answers 42601.

## Found by batch 55 (2026-10-24)

Found while building the cumulative statistics (RP-070). The gap tests are
in `tests/backlog/test_rp_batch55.py`, measured on a private 15.19 cluster.
RP-227 (`pg_stat_database` did not count a session whose client went away
without a Terminate, and counted a terminated one only once its client
closed the socket) was found and fixed in the same batch; its test there is
a regression test.

- [ ] **Some statements' catalog reads are not counted.** Since RP-225 a
      statement's catalog scans are walked as PostgreSQL 15.19's syscache,
      relcache and planner make them (`catalog_reads.rs`, over the
      per-session caches in `catcache.rs`), but not for: DDL's own reads
      (ALTER, DROP, CREATE INDEX; CREATE TABLE checks names only), the rule
      queries of system views (`pg_stat_*`), psql's `\d` pattern query
      (regex prefix as an index condition, `pg_table_is_visible` per row,
      `get_actual_variable_range`), subqueries, CTEs, windows, PL/pgSQL
      bodies, and texts over 64 KiB. Measured: `\d` 91 counters match, 29
      differ; a `pg_stat_*` view 53 match, 61 differ.
      Backlog-ID: RP-290
      Verify: tests/backlog/test_rp_catalog_reads.py::test_gap[an ALTER TABLE's own catalog reads], tests/backlog/test_rp_catalog_reads.py::test_gap[a statistics view], tests/backlog/test_rp_catalog_reads.py::test_gap[psql's describe query]
- [ ] **A catalog read counts no blocks** in `pg_statio_sys_*`: the scans
      are counted (RP-225), the buffers they hit or read are not.
      Backlog-ID: RP-291
      Verify: tests/backlog/test_rp_catalog_reads.py::test_catalog_blocks
- [ ] **The first backend after a start is counted as a warm one.**
      PostgreSQL serves it without the relcache init files (about 245
      `pg_attribute` index scans, `pg_class` heap scans, ...); the model
      treats every backend as starting with them. Separating the shared
      and local cold costs needs an instrumented PostgreSQL.
      Backlog-ID: RP-292
      Verify: tests/backlog/test_rp_catalog_reads.py::test_the_first_backend_after_a_start
- [ ] **`pg_class.relnatts` leaves out dropped columns** (1 where 15.19
      says 2 after a DROP COLUMN).
      Backlog-ID: RP-293
      Verify: tests/backlog/test_rp_catalog_reads.py::test_found[relnatts counts dropped columns]
- [ ] **`proargtypes::regtype[]` is refused** ("cannot cast int4 to
      regtype[]"); 15.19 returns `[0:0]={text}`.
      Backlog-ID: RP-294
      Verify: tests/backlog/test_rp_catalog_reads.py::test_found[an oidvector casts to regtype[]]

## Found by batch 54 (2026-10-24)

Found while building `pg_identify_object` and its siblings (RP-199), and
fixed in the same batch; their tests in `tests/backlog/test_rp_batch54.py`
are regression tests. RP-217: `CREATE POLICY`, `ALTER POLICY`, `CREATE
STATISTICS` and `CREATE` / `ALTER PUBLICATION` read a table in another
schema as `public`'s (42P01), and a statistics object was always in
`public`. RP-218: a `regprocedure` with a schema-qualified argument type
(`textlen(pg_catalog.text)`) was 42883. RP-219: `to_regoper` and
`to_regoperator` were refused. RP-220: `x.*` over a function in FROM was
refused. RP-221: a domain constraint's `connamespace` was `public`'s.
RP-222: an unnamed `CREATE STATISTICS` (PostgreSQL 16's) was accepted, and
`IS JSON`'s syntax error did not quote the token as written. RP-223:
`pg_options_to_table('{a=1,b=2}')`, an untyped array literal, listed no
options.

## Found by batch 52 (2026-10-24)

Found while building `pg_describe_object` (RP-199) and RP-200. The gap
tests are scenarios in `tests/backlog/test_rp_batch52.py` and
`tests/backlog/test_rp_batch51.py`, measured on a private 15.19 cluster.
RP-204 (an executor-answered function in a PL/pgSQL expression), RP-206 (a
view over another schema's table had no rule dependency), RP-207 (an index
in another schema was listed in `public`), RP-208 (a built-in row type
printed as its oid), RP-209 (`plpgsql`'s extension oid), RP-210 (the
visibility functions) and RP-211 (`pg_operator` and `pg_rewrite` listed
none of PostgreSQL's rows) were found and fixed in the same batch; their
tests are regression tests.


## Found by batch 51 (2026-10-23)

Found while building operator classes, subscriptions and transforms
(RP-169). The gap tests are scenarios in `tests/backlog/test_rp_batch51.py`,
measured on a private 15.19 cluster. RP-201 (`shobj_description` answered
NULL for a database's or role's comment) was found and fixed in the same
batch, and RP-200 (a block's own index definitions) in batch 52; their
tests there are regression tests.


## Found by batch 50 (2026-10-23)

Found while building extended statistics (RP-099). RP-197
(`pg_get_statisticsobjdef` and its siblings were refused) and RP-198 (a
`real` cast to text printed as the double it is kept in) were found and
fixed in the same batch; their tests in `tests/backlog/test_rp_batch50.py`
are regression tests.

## Found by batch 48 (2026-10-23)

Found while building VACUUM and the free space map (RP-193). The gap tests
are scenarios in `tests/backlog/test_rp_batch48.py`, measured on a private
15.19 cluster. RP-195 (`pg_relation_size`) was found and fixed in the same
batch; its test there is a regression test.


## Found by batch 46 (2026-10-23)

Found while building the system columns (RP-069). The gap tests are
scenarios in `tests/backlog/test_rp_batch46.py`, measured on a private 15.19
cluster. RP-194 was found and fixed in the same batch, RP-192 in batch 47
and most of RP-193 in batch 48; their tests there are regression tests.


## Found by batch 34 (2026-10-23)

Found while building `CREATE TEXT SEARCH CONFIGURATION` (RP-072). The gap
tests are scenarios in `tests/backlog/test_rp_batch34.py`, measured on a
private 15.19 cluster. RP-166 and RP-167 were found and fixed in the same
batch; their tests there are regression tests.

## Found by batch 26 (2026-10-22)

Found while making PostgreSQL 15's `pg_dump`, `pg_dumpall` and `psql` answer
as they do against PostgreSQL 15.19 (RP-070, RP-071). The gap tests are
scenarios in `tests/backlog/test_rp_batch26.py`, measured on a private 15.19
cluster.


## Found by batch 28 (2026-10-22)

Found while measuring `pg_depend` (RP-105) against PostgreSQL 15.19. The gap
tests are scenarios in `tests/backlog/test_rp_batch26.py`.

- [ ] **OPEN — the `information_schema` views over every built-in function
      take a second or more** (optimised build without LTO, a fresh
      database, `count(*)`): `routines` 0.9 s, `parameters` 1.0 s,
      `data_type_privileges` 2.1 s, `element_types` 14.8 s, where PostgreSQL
      15.19 takes 10-50 ms. They answer PostgreSQL's rows since RP-102 listed
      the 3,244 built-ins. The same build took 3.1, 2.2, 5.6 and 62 s before
      the top-ten batch (2026-10-26), which removed: a domain CHECK parsed
      again per value; a timestamp companion name built for every field of
      every row; a row expression copying every column of its source and
      the statement's parameters -- among them an uncorrelated subquery's
      rows, resolved while planning -- on every evaluation; the same
      parameters Debug-printed as a LATERAL item's memo key per left row;
      and a FROM subquery planned again only to learn its column names
      (`whole_row_rewrite`) at every level of nesting. What is left:
      - the evaluator analyses each expression again for every row:
        `const_value` resolves a cast's target (`type_name_of`,
        `declared_typmod`, `user_defined_base_source`), `static_type` and
        the user-cast and composite lookups per value, which PostgreSQL
        does once at parse analysis. Hoisting that to plan time is the
        structural fix;
      - `element_types`' `(a, b, c, d) IN (SELECT ... FROM
        data_type_privileges)` is an OR over the subquery's 11,734 rows of
        parameterised equalities (`row_subquery_comparison`), where
        PostgreSQL runs a hashed SubPlan; and the subquery runs while the
        statement is planned (2.3 s of its 15.8);
      - planning `element_types` still takes about 4 s besides that.
      Under `LIMIT 0` they ran whole too, until batch 83: on CI's
      debug build `test_rp_found_2026_10_20.py::test_catalog_relations_are_queryable`
      passed the 600 s per-test timeout, which ended the xdist worker.
      Backlog-ID: RP-131
      Verify: none -- a performance budget; every answer matches PostgreSQL's

## Found by the corpus test (2026-10-21)


## Code structure (2026-10-19)

- [ ] **Rust PG server: `PgHandler` holds every piece of session state in
      one flat struct, and one function runs most statement types.** Found
      by a graphify knowledge graph of the repository: `PgHandler` ranks
      second by edge count (660, all read from the code) and is the largest
      bridge in the graph, linking 24 communities. Measured on origin/main
      at 2026-10-19:
      - the struct (`crates/secantus-pg/src/lib.rs`) has 77 fields: login,
        TLS and hba state, transaction state (savepoints, deferred FKs and
        triggers, transaction-local GUCs), portals and cursors, plan and
        Describe caches, LISTEN/NOTIFY queues, temp tables and large-object
        descriptors;
      - `lib.rs` is 44,633 lines with about 650 functions, and 44
        `impl PgHandler` blocks across 29 files add roughly 300 more;
      - `execute_inner_keys` is a single 4,743-line `match` with 106
        `Statement` arms, and `virtual_rows` (the catalog views) is 2,848
        lines.
      The hub is not avoidable: pgwire wants one per-connection type
      implementing `StartupHandler`, `SimpleQueryHandler`,
      `ExtendedQueryHandler` and `CopyHandler`. Splitting the methods into
      separate files is already done. What is missing is ownership of the
      state, because any method can read or write any field. The likely
      shape: sub-structs that each own their own rules (transaction,
      portals and cursors, connection security, notifications), and
      `execute_inner_keys` split into one function per statement family.
      Three groups are done, all in `txn_state.rs`. The open transaction's
      uncommitted tables, types and sequences are `PendingCatalog`, with
      one `clear`. Its cache-bypass marks (catalog changed, role written)
      are `TxnMarks`, with one `end`. Its deferred constraint checks and
      `SET CONSTRAINTS` modes are `DeferredChecks`, with one `clear`.
      Every transaction-end path calls all three. Those paths had each
      kept their own list, and the lists had drifted apart three times
      (RP-050, RP-051, RP-052). `savepoints`, `txn_failed` and
      `in_transaction` are still reset path by path, and some of those
      differences are deliberate, so read each path's comments before
      merging them.
      Neither touches pgwire, the wire protocol or the on-disk format. No
      client sees a different answer, so this is not a divergence. It is
      here because a change to transaction or savepoint state currently has
      to be checked against the whole file.
      Backlog-ID: RP-049
      Verify: none -- internal code structure; no answer differs from PostgreSQL
