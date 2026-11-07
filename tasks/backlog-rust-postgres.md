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
- **Scope decision withdrawn — EXPLAIN costs** (Joe, 2026-11-02, deciding
  RP-314). EXPLAIN printed PostgreSQL's plan shape with zero costs, on the
  ground that this server had no cost model; it now has one (`costsize.rs`)
  and the catalogs' `pg_statistic` rows, so every node prints PostgreSQL's
  estimate. A single relation's Seq Scan does since RP-314; each other node
  kind is its own item (RP-317 to RP-333), and which side of a hash join is
  built goes with RP-330.
- **Scope decision — a PARTIAL expression index** is not used for a WHERE
  comparing the expression with a constant (its field is absent outside the
  predicate) -- a plan choice; the WHERE still runs, as a scan.
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
    call or a cast that costs; a join of more than 200,000 rows a side,
    which streams in scan order. Joins on several equivalence classes are
    searched since 2026-10-27 (`a JOIN b ON a.b = b.a JOIN c ON a.c = c.a
    JOIN d ON a.b = d.a JOIN e ON a.c = e.a`): every class of three or
    more members is kept in `eq_classes` order, two relations join on each
    class with members on both sides, and each class has its own merge
    order. A merge clause's key finds its class as
    `get_eclass_for_sort_expr` does (a big class, an inner equality's
    two-member class, or the one-member class the first outer join's
    clause on that column made), so an outer join's merge reads a side
    already in that order unsorted, and an open outer-join clause keeps
    that order useful (`pathkeys_useful_for_merging`). Two classes that
    both join one pair of relations are still declined.
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
    812 before). Since 2026-10-27 an ORDER BY under a LIMIT is planned
    with outer joins in the search (a nullable key's order is its own, as
    `build_join_pathkeys` gives a right or full join none), and an
    ascending NULLS LAST ORDER BY under a LIMIT over an inner or LEFT join
    of two stored user tables leaves the narrow path for this search,
    which weighs presorted paths against a bounded Sort
    (`create_ordered_paths`). On the rebuilt matrix
    (`tools/probes/pg_join_order_matrix.py`, 1,665 shapes, private 15.19):
    1,140 of 1,140 LIMIT shapes of three to five tables (1,031 before), 285
    of 285 with no LIMIT (247), and 228 of 240 two-table ORDER BY + LIMIT
    shapes (168); the 12 left were LEFT joins of 3,000-row tables with an
    index on the nullable key, which PostgreSQL reads by an Index Only Scan
    under a Merge Right Join (closed 2026-11-02, below). Since 2026-11-01 a DESC NULLS FIRST ORDER BY
    is searched too, every merge on its class sorting both sides
    descending (`select_outer_pathkeys_for_merge`): on the matrix with
    every ORDER BY made DESC, 1,097 of 1,140 LIMIT shapes and 212 of 240
    two-table ones match (1,017 and 177 before). Since 2026-11-01 a DESC
    key on a LEFT join's nullable side is searched too: the merge with that
    side outer sorts both sides descending, and a side presorted ascending
    is sorted for it (1,130 of 1,140 and 212 of 240). Since 2026-11-01 an
    index scan in a DESC ORDER BY's class gives two paths, as
    `build_index_paths` does: the forward scan, ascending and of use for
    merging, and a backward one in the query's order; a merge reading a
    side presorted takes that side's direction (1,140 of 1,140 and 226 of
    240). Since 2026-11-02 a DESC ORDER BY of a key an outer join
    NULL-extends, or one no clause reads, keeps only the backward index
    scan: the forward scan's ascending pathkey is the wrong direction for
    merging under that ORDER BY (`right_merge_direction`) and of no use
    for ordering, so `truncate_useless_pathkeys` leaves it none; that
    key's class (`ORDER_CLASS`) is the one its merge clause finds, so a
    merge reads the backward scan presorted. And a SELECT of an ORDER BY
    of one column under a LIMIT over two tables the search plans is no
    longer streamed (`try_stream_join_select`): the streamed merge join
    was another plan's and left ties in another order, which had hidden the
    search's answer for the shapes with an index on the nullable key.
    On the matrix (`tools/probes/pg_join_order_matrix.py`, private 15.19):
    1,140 of 1,140 LIMIT shapes, 285 of 285 with no LIMIT and 240 of 240
    two-table ones, ascending and with every ORDER BY made DESC (228 and
    226 before). Since 2026-11-04 an ascending ORDER BY with its NULLs
    first is searched too: an index scan's forward order (NULLS LAST) is
    of use for merging only, and a merge on the ORDER BY's class sorts
    NULLS FIRST. Since 2026-11-05 a DESC NULLS LAST is searched the same
    way, its merge sorting both sides descending with NULLs last: on the
    matrix with every ORDER BY made DESC NULLS LAST (`--desc-nulls-last`),
    1,140 of 1,140 LIMIT shapes, 285 of 285 with no LIMIT and 238 of 240
    two-table ones. Since 2026-11-06 a LEFT join with an index on the
    nullable key merges on that index's backward scan, whose DESC NULLS
    FIRST is of use for merging (`right_merge_direction` weighs the
    direction only) but not for the ORDER BY (`BACK_KEYS`): 240 of 240.
    Declined still: an ORDER BY of several columns (joined whole and sorted, so its
    ties come out in another order: `t0 JOIN t1 ON t0.b = t1.a ORDER BY
    t1.a, t0.c LIMIT 5` over two 3,000-row tables), and a two-table join
    with a subquery side.
  - Sorts that spill to disk runs are not PostgreSQL's merge.
  Backlog-ID: RP-028
  Verify: tests/backlog/test_rust_postgres_gaps.py::test_a_two_column_order_by_over_a_join_keeps_its_ties

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
        lane does not gate on. **Joe (2026-10-27): keep optimising.**
      - **Second pass, 2026-10-27 (`changelog.d/rust-pg-rp014-insert-cpu-2.md`):**
        a further 18.6% of instructions retired (225.4G to 183.4G) and
        17.3% of CPU (28.8 s to 23.8 s) on the development Mac's debug
        build; the script there went from 9.8 s to 8.1 s, against
        PostgreSQL 15.19's 7.6 s on a private cluster (16.7 s at the start
        of the batch). **On `ubuntu-latest` it barely moved**: 49.9, 49.7
        and 53.9 s against the runner's PostgreSQL's 23.4 to 24.1 s (run
        40476352850). There the server's three connection threads are
        flat out, 128 to 170 s of CPU a run (about 0.9 ms an insert,
        against about 0.16 ms on the Mac); with commits not synced
        (`SECANTUS_TEST_FAST_STORAGE=1`, a diagnostic only) the CPU is the
        same, so it is not `fdatasync`. A `perf` profile there (AMD EPYC
        7763, 2 cores / 4 threads, shared with the client) is flat: 96% in
        the server's own code, no symbol above 0.6%, the allocator about
        5%, `format!` about 0.7%; the VM exposes no instruction counter.
        So that runner pays several times this Mac's cost per instruction
        of debug code, and only less work an insert will close it. What
        is left is spread thin (the second pass's spans, per connection
        of 50,000 statements: storage's insert about 1.95 s, the row's
        page models about 1.9 s, Bind 0.7 s, privileges + locks + plan
        0.7 s, the statement wrapper 0.57 s); batching a pipeline's
        Executes would change per-statement semantics (`cmin`,
        `statement_timestamp`, where an error lands).

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

## Found by a CI run (2026-11-01)

- [ ] **OPEN — CI: three jobs ran out of disk; the one that left a log had
      been given a 72 GB runner, not the usual 145 GB one.** PR #2249,
      run 41312019513, job `pg-oracle (server)`: the slice suite (872
      passed) and the login comparisons passed; ~70 `backlog_fixed` tests
      passed, then one failed and every later test errored with `could not
      create numbered dir with prefix rust-pg` (ENOSPC), and the runner
      could not write its own log, so the test that wrote the data is not
      named. The same base commit (`b9a9a5b7`) had passed this job on `main`
      15 minutes earlier. A run of the job on the next commit showed 86 GB
      free on `/` at the start, so this was not a thin margin: tens of GB
      were written in seconds, which looks like a runaway write (a WT log or
      file growing without bound, or a test materialising something huge).
      `test.yml` now prints `df` around each pg-oracle suite and, on
      failure, the largest directories under the temp and work trees.
      **If it recurs, read those lines first**: they name the directory,
      and so the test, before any theory.
      Checked 2026-11-02 (fix-one): no ENOSPC in any pg-oracle run since,
      across 4 red runs (each red for another reason) and ~50 green ones.
      Run 41397005963 shows the step's normal footprint: 798 `backlog_fixed`
      tests left 21 GB under `/tmp/pytest-of-runner`, spread evenly over the
      four workers (~26 MB a test) -- accumulation, not the runaway seen
      here. The `du -d 3` stopped at `popen-gw<k>`, which names a worker,
      not a test; the step now also lists the per-test directories.
      Seen again on 2026-11-03 in a DIFFERENT job: `storage-engine
      (ubuntu-latest)` on PR #2301 (run 41710470392, attempt 1, job
      126176901872). The disk filled so completely that the Actions runner
      itself died ("No space left on device" writing its `_diag` worker
      log), so the job left no log at all; the rerun passed. That job has
      no on-failure disk report, and the PR changed only login probes, so
      the writer is not specific to the pg-oracle shard.
      Since 2026-11-03 both jobs run `scripts/ci_disk_watch.sh` all job
      long (proved on each runner first by `ci_disk_watch_selftest.sh`):
      under 10 GB free it reports the largest files, the processes holding
      them and each one's pytest test, kills those processes so the runner
      survives, and its last step fails the job and prints the report.
      **The next occurrence names the writer: read the "Disk watchdog
      report (RP-312)" step first.**
      **Diagnosis (2026-11-03): probably no runaway write at all.** A scan
      of every failed Tests job since 2026-10-20 found a third occurrence
      with its log intact: `rust-storage`, run 41630467524, where `df`
      shows `/dev/mapper/cow-root 72G`, 13 GB free before `cargo test` and
      157 MB after, rustc and ld dying of ENOSPC with an 11 GB target dir.
      Every green job measured (6 runs x 3 jobs) ran on `/dev/root 145G`.
      So `ubuntu-latest` sometimes hands out a 72 GB disk on which the same
      image leaves ~13 GB, and the heavy jobs use 21 GB (`rust-storage`),
      31 GB (`pg-oracle`) and 33 GB (`storage-engine`). The 2026-11-01 and
      2026-11-03 storage-engine occurrences left no `df` line, so that they
      were the same small runner is inferred, not measured. Since
      2026-11-03 every heavy Linux job runs `.github/actions/free-disk`
      (removes unused toolchains, prints the disk device and size, warns
      under 40 GB free). Close this when a job on a `cow-root` runner is
      seen passing, or the watchdog names a real writer.
      **Measured 2026-11-07: no writer, the retained runs.** The watchdog
      fired in `storage-engine (ubuntu-latest)` on PR #2408 (run
      42333063884, job 128283737530), on a `cow-root 72G` runner, during
      that job's FOURTH pytest invocation (`backlog.py verify --file
      backlog-rust-mongodb.md --regressions`): 9.9 GB free, no file over
      1 GB, and 38 GB under `/tmp/pytest-of-runner` -- `pytest-1`, the
      first step's 1,262-test run, alone 27 GB (~260 MB a server-starting
      test: its 128 MB WiredTiger log file). pytest keeps three earlier
      runs' directories, and the session-start reap in `tests/conftest.py`
      cannot run in that job's slim env (`python_tasks` imports `invoke`),
      so every step's stores stayed. Since then pyproject sets
      `tmp_path_retention_count = 1`: each run's exit deletes the runs
      before it. Close this when a `cow-root` `storage-engine` job passes
      with that in place.
      Backlog-ID: RP-312
      Verify: none -- seen once in CI, not reproduced; a gap test needs the writer named first (the job now reports it)

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

## Found by the isolation flake (2026-10-27)

Found while chasing `test_rp_isolation.py`'s CI flake ("another command is
already in progress", and a worker killed at the 600 s per-test deadline),
and fixed in the same batch; their tests in
`tests/backlog/test_rp_lock_release.py` are regression tests:
- RP-270: a lock waiter was still reported blocked
  (`pg_isolation_test_session_is_blocked`, `pg_blocking_pids`) after its
  holder's COMMIT had answered, 48 times of 50; PostgreSQL 15.19 never.
- RP-271: an autocommit statement took no table lock, so a DROP TABLE ran
  beside an autocommit UPDATE that had waited on a row, and the server
  wedged for good: every later BEGIN waited too.

## Found by the top-ten batch (2026-10-26)

Found while speeding up the `information_schema` views (RP-131), by
comparing `routines`, `parameters` and `element_types` with PostgreSQL 15.19
row for row. The tests are in `tests/backlog/test_rp_catalog_identity.py`,
measured with read-only queries on 15.19. Fixed in the same batch, their
tests regression tests:
- RP-300: 56 built-in `pg_type` rows differed from `initdb`'s: a
  fixed-length subscriptable type's `typelem` (`name`, `int2vector`,
  `oidvector`, `point`, `lseg`, `box`, `line`, `_record`), a range array's
  `typtype`, `_box`'s `typdelim`, ten types' `typcollation`, and NULL
  `typnotnull` / `typbasetype` / `typtypmod` on 26 pseudo-types. So
  `information_schema` called `int2vector` a plain type, not `ARRAY`.
- RP-301: `format_type(18, NULL)` was `character` (`"char"` is meant), and a
  modifier on a type with no `typmodout` was dropped (`format_type(25, 7)`
  is `text(7)`).
- RP-285 (found by RP-254's work): `pg_class.relam` was `btree` for every
  index, so a join to `pg_am` named a GIN, GiST or hash index's method
  wrong (`tests/backlog/test_rp_catalog_identity.py`).
- RP-286 (found by RP-202's): a `pg_sleep` in the select list of a streamed
  SELECT (`select a, pg_sleep(2) from t`) did not look for a cancel and
  slept to its end; a row streamed after the statement's hooks are gone
  is evaluated under a cancel hook now (`tests/backlog/test_rp202_cancel.py`).

      Verify: none -- reproducing it exhausts memory; a bounded gap test is still to be written

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
      - GIN `=` on arrays, and opclasses other than `array_ops`,
        `jsonb_ops`, `jsonb_path_ops` and `tsvector_ops` (btree_gin), whose
        indexes `pg_relation_size` reads as 0; array elements other than ints,
        floats, bool and C-ordered text; an ANALYZEd array column (MCELEM
        statistics);
      - `jsonb_ops` scans (`@>`, `?`, `?|`, `?&`): its pages and keys are
        modelled, its `extractQuery` and consistent functions are not;
      - `jsonb_path_ops` scans (`@>`, `@?`, `@@`): its pages and keys
        (`gin_extract_jsonb_path`) are modelled, its `extractQuery` and
        consistent functions are not;
      - `tsvector_ops` scans (`@@`): its pages and keys
        (`gin_extract_tsvector`) are modelled, its `extractQuery` and
        consistent functions are not;
      - point/box `~=`, polygon and circle containment;
      - a multi-column GIN index's scan (since 2026-11-06 its pages are
        modelled, each key tuple carrying its column's attnum);
        multi-column GiST indexes, expression GIN / GiST indexes, and a
        partial GiST index; a partial GIN index's scan (`predicate_implied_by` is
        not modelled, so it never serves one), though since 2026-11-04 its
        pages are modelled, built and inserted into only for the rows its
        predicate holds for;
      - an UPDATE's or DELETE's scan (only a SELECT passes its residual);
      - VACUUM deleting emptied GIN posting-tree pages and GiST pages;
      - GiST's `gistchoose` breaks penalty ties with a per-backend random
        seed; the model uses a fixed one, so on overlapping `box_ops` data a
        tuple can land on another page than in a given backend (15.19 gave
        the same layout on three runs).
      The size functions read the modelled indexes' pages (since RP-287's
      first slice); an unmodelled one (an expression GIN index, or a
      btree_gin one) still reads 0 from
      `pg_relation_size`.
      Backlog-ID: RP-287
      Verify: tests/backlog/test_rp_top10_found.py::test_an_expression_gin_index_has_its_pages



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
      (DROP, CREATE INDEX; CREATE TABLE checks names only; ALTER TABLE
      only for a superuser's ADD COLUMN without a default, constraint,
      collation, serial or domain type, and a DROP COLUMN no index,
      default, constraint or non-pinned type is tied to -- every other
      subcommand, a new TOAST table and the role caches a non-superuser's
      permission checks read stop its walk, and a view that depends on a
      dropped column is not seen: its pg_depend rows count as none), the rule
      queries of system views (`pg_stat_*`), psql's `\d` pattern query
      (regex prefix as an index condition, `pg_table_is_visible` per row,
      `get_actual_variable_range`), subqueries, CTEs, windows, PL/pgSQL
      bodies, and texts over 64 KiB. Measured: `\d` 91 counters match, 29
      differ; a `pg_stat_*` view 53 match, 61 differ.
      Backlog-ID: RP-290
      Verify: tests/backlog/test_rp_catalog_reads.py::test_gap[a statistics view], tests/backlog/test_rp_catalog_reads.py::test_gap[psql's describe query]
- [ ] **A catalog row added after `initdb` counts no heap block** in
      `pg_statio_sys_tables`. A catalog scan counts its index pages
      (`catalog_btree.rs`, from `pg15_catalog_indexes.txt`) and, since
      2026-11-01, the heap blocks behind `initdb`'s own rows: an index
      scan's fetch of each tuple it returns, a sequential scan's every
      block, over `pg15_catalog_heaps.txt` (`catalog_heap.rs`; regression
      test `test_catalog_heap_blocks`). A row any database adds later has
      no place yet, so a scan that returns one (a new table's `pg_class`
      and `pg_attribute` rows) counts neither its heap block nor, for a
      sequential scan, the blocks it grew the heap by.
      **Decided** (Joe, 2026-11-01): option A, the full model, landed as a
      sequence of PRs, this item open until the last. Since 2026-11-02
      each built-in catalog's `initdb` pages (free space, line pointers,
      redirects, heap-only tuples) and free space map fork are recorded
      too, and `catalog_heap::initdb_heap_model` builds the heap a new
      row goes in, placed FSM-first as PostgreSQL places it (regression
      test `a_new_tables_catalog_rows_go_where_postgresql_puts_them`).
      Remaining: place every later catalog row in those models as DDL
      writes it (each catalog's `heap_form_tuple` length, and the HOT or
      non-HOT updates DDL makes of its own new rows), and insert its
      index entries into the index trees, which hold only `initdb`'s
      tuples and are shared by every database -- with a non-HOT update's
      new version and entries, and VACUUM's removal of dead ones (reading
      this server's catalog rows to find them is not the way: on a
      statement's path it cost a new session hundreds of milliseconds,
      inside a transaction that changed the catalogs it blinded the
      session to its own new objects, and off a statement's path it lost
      other sessions' new tables). Also open: the shared buffer pool keys
      a page by relation oid and block, not database, so two databases'
      copies of one catalog share hits. An index-only scan's `Heap
      Fetches` (RP-360) reads `initdb`'s visibility map: until later rows
      are placed, a row DDL adds counts as fetched but the `initdb` rows
      on the page it cleared do not, and a row DDL deleted or updated
      still reads as all-visible.
      Backlog-ID: RP-291
      Verify: tests/backlog/test_rp_catalog_reads.py::test_catalog_blocks
- [ ] **The first backend after a start is counted as a warm one.**
      PostgreSQL serves it without the relcache init files (about 245
      `pg_attribute` index scans, `pg_class` heap scans, ...); the model
      treats every backend as starting with them. Separating the shared
      and local cold costs needs an instrumented PostgreSQL.
      **Decided** (Joe, 2026-11-02): build it -- a private PostgreSQL
      15.19 from source with the catalog-read counters, kept in the
      worktree's `.scratch/`, never the shared reference servers.
      Backlog-ID: RP-292
      Verify: tests/backlog/test_rp_catalog_reads.py::test_the_first_backend_after_a_start
- [ ] **A database-wide ANALYZE measures only some catalog columns.** Since
      RP-311 each database holds `initdb`'s `pg_statistic` rows for the
      built-in catalogs. A plain `ANALYZE` (or `VACUUM ANALYZE`)
      re-measures the catalog columns of a type whose statistics are
      modelled (`oid`, the integers, `bool`, `float4` / `float8`, `text`,
      `name`, `"char"`, and since 2026-11-04 the one-dimensional arrays of
      those, with `compute_array_stats`' element slots), over the
      catalog's rows as they stand, and their rows replace `initdb`'s;
      since 2026-11-05 so do `aclitem[]` (`=` but no `<`:
      `compute_distinct_stats`) and the `reg*` types (under `oid`'s
      opclass), and since 2026-11-06 `int2vector` (an array to
      `type_is_array`: `array_ops`) and `oidvector` (its own
      `oidvector_ops`). The other columns keep `initdb`'s rows, where
      PostgreSQL 15.19 measures them too: `pg_node_tree`. Not modelled
      either: the measured
      catalogs' `relpages` / `reltuples` in `pg_class`, and the row order
      of the replaced `pg_statistic` rows (an update moves PostgreSQL's)
      (found while working RP-311).
      Backlog-ID: RP-313
      Verify: none -- the `pg_node_tree` columns need `nodeToString`'s
      text of each stored tree; no gap test until a probe pins one
- [ ] **A database-wide ANALYZE's correlation of a catalog column is not
      PostgreSQL's.** `compute_scalar_stats` correlates a column's sort order
      with the rows' heap order; here the catalog's rows are walked in this
      server's own order, not PostgreSQL's heap's (`initdb`'s pages, then
      where a new row goes, RP-291). In a fresh database after `create table
      zzzz_rp313 (a int)` and `analyze`, PostgreSQL 15.19's `pg_class`
      correlations are `oid` 0.6547713, `relkind` 0.7882681, `relname`
      -0.12838678, `relnatts` 0.67018104; here 0.9854369, 0.8315357,
      0.1999988 and 0.50667804. The most common values and their
      frequencies match (found while working RP-313).
      Verify: tests/backlog/test_rp_catalog_reads.py::test_a_catalog_columns_correlation_follows_the_heap
      Backlog-ID: RP-358
- [ ] **EXPLAIN prints no estimate for most Nested Loops.** Since RP-329
      a Nested Loop of two tables' sequential scans joined only by `<`,
      `<=`, `>`, `>=` or `<>` conjuncts (or by none) prints
      `cost_nestloop`'s estimate, its outer side and Materialize as
      PostgreSQL 15.19 picks them: a comma join's WHERE, and since
      2026-11-05 an inner or LEFT `JOIN ... ON` (a LEFT join keeps its
      nullable side inner and never estimates fewer rows than its outer),
      over a table with an index too when no clause could be its Index
      Cond (a `<>`), and since 2026-11-06 the inner Index Scan
      parameterized by an inequality between two keys, the join's other
      clauses its Filter (`e.a < k.a`: `Index Scan using k_pkey on k` with
      `Index Cond: (a > e.a)`), or Index Only Scan where the index holds
      every column the side reads. Every other nested loop still prints
      zeros: two bounds on the index column (`e.a < k.a and e.a >
      k.a`, one `Index Cond` of both), a clause over an expression of the
      inner side (`e.a < k.a + 1`, the scan's Filter), a key on a later
      index column; an ON conjunct over one side only (`e left join e e2
      on e.a <> e2.a and
      e.a > 990` is `rows=9990`, its `e.a > 990` a LEFT join's join filter
      estimated from `e.a`'s statistics); a LEFT join with a WHERE; and a
      join of three or more relations (found while working RP-329).
      Backlog-ID: RP-354
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a one-sided ON conjunct]

- [ ] **EXPLAIN ANALYZE's scan counters PostgreSQL prints that are still
      left out.** Since RP-383 a user table's heap pages carry the
      visibility map's bit (set by VACUUM, cleared by a write to the page),
      and an index-only scan prints its `Heap Fetches`, a Bitmap Heap Scan
      its `Heap Blocks: exact=` and its Bitmap Index Scan its own `actual
      rows`, as PostgreSQL 15.19 does, and since 2026-11-06 an index
      scan its `Rows Removed by Filter`, and since 2026-11-06 a BitmapOr
      its and its arms' actual rows. Still missing: any of those
      counters while the heap holds a dead
      version, a dead line pointer or a HOT chain not yet vacuumed, whose
      index entries the scan visits too (`update k set b = 'z' where a =
      5`, then the index-only scan of `a = 5`: `Heap Fetches: 2`); and a
      bitmap past `work_mem`, whose
      pages go lossy (`Heap Blocks: exact=... lossy=...`). Measured on a
      private 15.19 cluster (found while working RP-383).
      Backlog-ID: RP-384
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an index-only scan over a dead version]
- [ ] **EXPLAIN ANALYZE prints no counts below most other joins.** Since
      RP-386 a join's children print their actual rows and loops, a
      parameterized inner index-only scan its `Heap Fetches`, a Hash its
      `Buckets / Batches / Memory Usage` line and a Nested Loop its `Rows
      Removed by Join Filter`, since RP-389 a LEFT Nested Loop's too
      (since 2026-11-06 a filter of any shape, evaluated per pair:
      `s left join e on s.a + 1 < e.a` is `Rows Removed by Join Filter:
      495`), and since 2026-11-06 a plain Nested Loop's Seq Scans their
      `Rows Removed by Filter` (`select * from s join e on s.a < e.a where
      e.b = 'v1'`), as PostgreSQL 15.19 counts them, and since RP-403 a
      Hash Join's and a parameterized Nested Loop's outer Seq Scans theirs
      too. Still unannotated: a LEFT join filter over a char(n) column
      (`cc left join s on cc.b < s.c`, `b char(4)`: `Seq Scan on cc
      (actual rows=30 loops=1)`), a LEFT join with a WHERE (unpriced too,
      RP-379), a parameterized inner index scan with its own Filter, a
      Merge Join (its Sorts' `Sort Method`), a hash table past `hash_mem`
      (several batches) and a join with an inner-unique inner side.
      Measured on a private 15.19 cluster (found while working RP-386).
      Backlog-ID: RP-390
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a left nested loop over a char(n) join filter]
- [ ] **An inner join nested in an outer join keeps its one-table ON
      conjuncts in its Join Filter.** Since RP-391 / RP-392 an inner
      join's ON conjunct over one table is that table's scan Filter, but
      only for joins reached from the FROM through inner joins. PostgreSQL
      15.19's `distribute_qual_to_rels` places an inner join's qual the
      same below an outer join: `select * from s left join (e join k on
      e.a > 998 and k.a = e.a) on true` runs `Filter: (a > 998)` on `Seq
      Scan on e`; here the Hash Join keeps `Join Filter: (e.a > 998)`. Its
      rows are right. Measured on a private 15.19 cluster (found while
      working RP-392).
      Backlog-ID: RP-393
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an inner join under an outer join with a one-table ON conjunct]
- [ ] **A LEFT join's ON conjunct over a table inside an inner join on its
      nullable side stays in its Join Filter.** Since RP-395 an outer
      join's ON conjunct over its nullable side runs in that side's scan
      when the side is one table. PostgreSQL 15.19's
      `distribute_qual_to_rels` also pushes it through inner joins on that
      side: `select * from s left join (e join k on e.a < k.a) on s.a = e.a
      and k.b = 'v3'` is a `Hash Right Join` whose `Seq Scan on k` has
      `Filter: (b = 'v3'::text)`; here the Hash Left Join keeps `Join
      Filter: (k.b = 'v3'::text)` and no estimate. Its rows are right. Not
      through a lower outer join's nullable side, where PostgreSQL keeps it
      at the join too. Measured on a private 15.19 cluster (found while
      working RP-395).
      Backlog-ID: RP-397
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a left join's ON conjunct over a table of an inner join it null-extends]
- [ ] **A joined scan's Filter keeps its conjuncts in written order.** A
      lone scan prints them in PostgreSQL's order (`qual_order`, RP-348:
      an equality to a constant after the rest, then by cost), but a
      join's scan does not: `select * from s join e on s.a < e.a where e.b
      = 'v3' and e.a > 5` prints `Filter: ((b = 'v3'::text) AND (a > 5))`,
      where PostgreSQL 15.19 prints `((a > 5) AND (b = 'v3'::text))`. The
      same for a LEFT join that the WHERE reduces to an inner one, whose
      ON conjunct over the nullable side leads its scan's Filter (RP-395).
      Measured on a private 15.19 cluster (found while working RP-395).
      Backlog-ID: RP-398
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a joined scan's filter puts its equality to a constant last]
- [ ] **A join whose WHERE equates a column to two constants still scans.**
      PostgreSQL 15.19 finds the equivalence class contradictory and plans
      no scan: `select * from s join e on s.a < e.a where e.b = 'v1' and
      e.b = 'v2'` is `Result` with `One-Time Filter: false`, and a LEFT
      join's ON doing the same (`on s.a < e.a and e.b = 'v1' and e.b =
      'v2'`) gates the nullable side's scan under a `Result`. Here a lone
      scan does it (RP-350), but a join's scan prints `Filter: ((b =
      'v1'::text) AND (b = 'v2'::text))`. Its rows are right. Measured on a
      private 15.19 cluster (found while working RP-395).
      Backlog-ID: RP-400
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a join whose where equates a column to two constants]
- [ ] **A LEFT join on an indexed key prints a Hash join where PostgreSQL
      merges through the index.** `select * from s left join k on s.a =
      k.a` (`k.a` the primary key) is, in PostgreSQL 15.19, a `Merge Right
      Join` on `(k.a = s.a)` over `Index Scan using k_pkey on k` and a
      Sort of `s` (cost 2.31..4.13); here a `Hash Right Join` over a `Seq
      Scan on k` (cost 1.68..20.73), the same with a filter on `k`, and an
      inner join of the two is a `Hash Join` where PostgreSQL's is a
      `Merge Join`. The join search does not price a merge join whose
      sorted input is an index scan. Its rows are right; the order they
      come out in may not be. Measured on a private 15.19 cluster (found
      while working RP-395).
      Backlog-ID: RP-401
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a left join on a primary key merges through its index]
- [ ] **A join's equivalence classes keep a repeated conjunct.** A
      one-table SELECT matches PostgreSQL 15.19 (`where a = 5 and a = 5` is
      `Filter: (a = 5)`), a join does not. `select * from e, k where e.b =
      k.b and e.b = k.b` prints `Hash Cond: ((e.b = k.b) AND (e.b = k.b))`
      and prices both (`rows=10000`), where PostgreSQL's
      `process_equivalence` finds the second redundant: `Hash Cond: (e.b =
      k.b)`, `rows=100000`. A repeated `e.a = 5` is `Filter: ((a = 5) AND
      (a = 5))` on e's scan, priced twice (`cost=0.00..20.00`; PostgreSQL
      `Filter: (a = 5)` at `17.50`). The rows are right. Measured on a
      private 15.19 cluster (found while working RP-388).
      Backlog-ID: RP-402
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[comma join with a repeated join equality], tests/backlog/test_rp_explain_costs.py::test_plan[comma join with a repeated constant equality]
- [ ] **EXPLAIN prints no estimate for a scan filtered by a function
      call, a pattern over an expression, a LIKE over a table never
      analysed, or a regex.** Since RP-404 a comparison of integer or text
      arithmetic with a constant, and a LIKE / ILIKE of an analysed string
      column, are estimated as PostgreSQL 15.19 estimates them. Still
      zeros: `select * from e where abs(a) > 5` (PostgreSQL `rows=333`,
      `cost=0.00..20.00`: `abs` is one function of `procost` 1, which is
      not modelled for any function), `lower(b) like 'v%'` (`rows=5`,
      `patternsel` with no statistics), `select * from zu where c like
      'x%'` over a table never analysed (`rows=6`, `cost=0.00..25.88`), and
      the regex operators: `c ~ '^xx'` over thirty analysed strings is
      `rows=30`, whose fixed prefix `regex_fixed_prefix` reads from the
      compiled regex, and `c ~ 'w01'` over a thousand is `rows=101`. Only
      a single relation's scan is estimated so: a join over such a scan
      prints zeros throughout (`e join s on s.a = e.a where s.c like
      'xxx%'` is PostgreSQL's `Hash Join (cost=1.74..20.78 rows=29
      width=27)`; `where e.a + 1 > 996` instead, `cost=1.68..23.02
      rows=10`). Measured on a private 15.19 cluster (found while working
      RP-404).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a function call compared with a constant], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a join over a scan filtered by a LIKE], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a LIKE over an expression], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a LIKE over a table never analysed], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of an anchored regex match], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of an unanchored regex match]
      Backlog-ID: RP-423
- [ ] **A lone scan's Filter does not put an expression's conjunct
      after a cheaper one.** PostgreSQL 15.19 orders a qual list cheapest
      first (`order_qual_clauses`, by `cost_qual_eval`): `select * from e
      where a + 1 > 996 and b = 'v1'` is `Filter: ((b = 'v1'::text) AND
      ((a + 1) > 996))`, the expression's two operators after the
      column's one. Here the conjuncts are printed as written (a join's
      scan keeps the written order in other cases too, RP-398). Measured
      on a private 15.19 cluster (found while working RP-403). The same
      for an OR, which costs its operators (`a not between 15 and 55 and b
      <> 'v3'` is `Filter: ((b <> 'v3'::text) AND ((a < 15) OR (a >
      55)))`), and for a coerced column (`a between 1 and 5.5 and b <>
      'v3'` puts `((a)::numeric <= 5.5)` after `(b <> 'v3'::text)`, the
      cast a function call); here both keep the written order (found while
      working RP-399).
      Backlog-ID: RP-406
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a scan filter's conjuncts in cost order], tests/backlog/test_rp_explain_costs.py::test_plan[a scan filter's or after a cheaper conjunct]
- [ ] **A constant coerced to its operator's type prints unfolded.**
      PostgreSQL 15.19 folds the coercion of an integer literal compared
      with a numeric before it prints the qual
      (`eval_const_expressions`): `select * from e where a::numeric > 4`
      is `Filter: ((a)::numeric > '4'::numeric)`; here `((a)::numeric >
      (4)::numeric)`, alone or as a join's scan. A cast written on the
      constant is folded already (RP-370). Measured on a private 15.19
      cluster (found while working RP-405).
      Backlog-ID: RP-407
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a scan filter's coerced constant is folded]
- [ ] **A table whose only index is a composite one prices no scan of
      both its columns.** `create table t2 (a int, b int, c text)`, 5,000
      rows, `create index t2_ab on t2 (a, b)`, analysed: PostgreSQL 15.19
      plans `select * from t2 where a = 15 and b = 3` as `Index Scan using
      t2_ab on t2  (cost=0.28..8.30 rows=1 width=11)` with `Index Cond:
      ((a = 15) AND (b = 3))`; here a `Seq Scan on t2  (cost=0.00..0.00
      rows=0 width=0)`, no estimate at all. The same table with a second,
      one-column index (`n` in the scenarios, `n_b` beside `n_ab`) is
      planned as PostgreSQL plans it, and so is `where a = 15` alone. Its
      rows are right; the order they come out in may not be. Measured on
      a private 15.19 cluster (found while working RP-399).
      Backlog-ID: RP-410
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a table's only index composite]
- [ ] **EXPLAIN of an IN or EXISTS subquery prints `Filter: (...)`, not
      PostgreSQL's semi join.** PostgreSQL 15.19 pulls `select * from e
      where a in (select a from s)` and `... where exists (select 1 from s
      where s.a = e.a)` up into a join (`convert_ANY_sublink_to_join`,
      `convert_EXISTS_sublink_to_join`) and prints a `Hash Semi Join` on
      `(e.a = s.a)` over `Seq Scan on e` and a Hash of `Seq Scan on s`;
      here `Seq Scan on e` with `Filter: (...)`, and the subquery's own
      scan, Filter included, is not printed. Related to the semi join
      RP-131 names for `element_types`. Measured on a private 15.19
      cluster (found while working RP-399).
      Backlog-ID: RP-411
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery is a hash semi join], tests/backlog/test_rp_explain_costs.py::test_plan[an exists subquery is a hash semi join]
- [ ] **A NOT over an AND, an OR or a BETWEEN prints as written, not
      pushed onto its arms.** PostgreSQL 15.19's `negate_clause` (during
      `eval_const_expressions`) applies De Morgan and replaces each
      operator by its negator: `select * from e where not (a > 15 or b =
      'v3')` is `Filter: ((a <= 15) AND (b <> 'v3'::text))` and `not (a
      between 15 and 55)` is `Filter: ((a < 15) OR (a > 55))`; here
      `(NOT ((a > 15) OR (b = 'v3'::text)))` and `(NOT ((a >= 15) AND (a
      <= 55)))`. An AND that comes out is then flattened into the scan's
      quals, which can change the index conditions and the estimate. Its
      rows are right. Measured on a private 15.19 cluster (found while
      working RP-399). With a NULL arm the pushed-down AND is constant
      false: `not (a > 1 or null)` is `Result` with `One-Time Filter:
      false` there, a `Seq Scan` with `Filter: (NOT ((a > 1) OR
      NULL::boolean))` here (found while working RP-409).
      Backlog-ID: RP-412
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a not over an or is the and of its negated arms], tests/backlog/test_rp_explain_costs.py::test_plan[a not over a between is the or of its negators], tests/backlog/test_rp_explain_costs.py::test_plan[a not over an or with a null arm is a false one-time filter]
- [ ] **A function in FROM leaves a missing column with no HINT.** Since
      RP-408 a 42703 carries PostgreSQL's `errorMissingColumn` hint, but
      only when every range table entry in reach has columns this server
      can tell: a function in FROM has the columns of its result type,
      which the hint's search does not know, so it gives none.
      PostgreSQL 15.19: `select * from generate_series(1,2) g where gg =
      1` hints `Perhaps you meant to reference the column "g.g".`, and
      `select * from e, generate_series(1, 2) g where bb = 1` hints
      `"e.b"`; here neither has a HINT. Measured on a private 15.19
      cluster (found while working RP-408).
      Backlog-ID: RP-413
      Verify: tests/backlog/test_rp_column_errors.py::test_function_in_from_hint
- [ ] **A missing column's 42703 names the wrong reference, or drops its
      qualifier.** PostgreSQL 15.19 reports the first missing column in
      the order it analyses the statement, by the name as written and at
      its first token. Here: `select * from e, k where k.bcc = 1 and e.bb =
      2` reports `column e.bb does not exist` at 40 (PostgreSQL `column
      k.bcc does not exist` at 26); a third table's `join k on k.bcc = 1`
      reports `column "bcc"` at the name's own token; an UPDATE's or
      DELETE's alias is dropped (`update e x set a = 1 where x.bb = 1` is
      `column "bb"` at 30, PostgreSQL `column x.bb` at 28);
      `public.e.bb` is positioned at 15, not 8; and `on conflict (a) do
      update set b = excluded.bb` reports this server's internal name,
      `column "__excluded__.bb" does not exist`, with no position
      (PostgreSQL `column excluded.bb does not exist` at 65). Each loses
      its HINT too, as the hint is found from the reference at the
      error's position. Measured on a private 15.19 cluster with `e(a
      int, b text)`, `s(a int, c text)`, `k(a int, b text, bc int)`
      (found while working RP-408).
      Backlog-ID: RP-414
      Verify: tests/backlog/test_rp_column_errors.py::test_missing_column_reported_as_written
- [ ] **A missing output column is reported after the WHERE's errors.**
      PostgreSQL 15.19's `transformSelectStmt` transforms the target list
      before the WHERE, so `select nosuch from e where 1`, `... where a =
      true` and `... where 'x'` are each 42703 `column "nosuch" does not
      exist` at 8; here the WHERE's 42804, 42883 or 22P02 comes first. The
      FROM list, a JOIN's ON included, is analysed before both, which this
      server already matches. Measured on a private 15.19 cluster (found
      while working RP-424).
      Verify: tests/backlog/test_rp_where_types.py::test_target_list_is_analysed_before_where
      Backlog-ID: RP-429
- [ ] **EXPLAIN of an UPDATE ... FROM or a DELETE ... USING prints a Seq
      Scan of the target alone.** The statement is rewritten into
      correlated subqueries over its FROM items (`rewrite_dml_from`), and
      EXPLAIN prints that rewrite's scan: `explain (costs off) delete from k
      using (select 50 as a) t where k.a = t.a` (and the UPDATE ... FROM
      form) is `Seq Scan on k`, where PostgreSQL 15.19 pulls the subquery
      up into `Index Scan using k_pkey on k` with `Index Cond: (a = 50)`;
      `delete from k using e where k.a = e.a` prints no join, where
      PostgreSQL prints a `Hash Join` with `Hash Cond: (e.a = k.a)` over
      both tables. Measured on a private 15.19 cluster with `e(a int, b
      text)` and `k(a int primary key, b text)` (found while working
      RP-432).
      Verify: tests/backlog/test_rp_dml_from.py::test_explain_prints_the_join_and_the_index_scan
      Backlog-ID: RP-435
- [ ] **An UPDATE ... FROM or DELETE ... USING reports an ambiguous name
      ahead of a missing column written before it.** `rewrite_dml_from`'s
      42702 check reads every expression of the statement before anything
      else is resolved; PostgreSQL 15.19's `transformUpdateStmt` analyses
      the WHERE before the SET list, each left to right, and stops at the
      first bad column. So `update k set a = a from (select 1 as a) t where
      t.nosuch = 1`, `update k set b = b from e where e.nosuch = 1` and
      `update k set b = 'x' from e where nosuch = 1 and b = 'y'` are 42702
      `column reference "a"` / `"b" is ambiguous` here and 42703 `column
      t.nosuch` / `e.nosuch` / `"nosuch" does not exist` there. A name
      written before the missing one keeps its 42702 on both. Measured on a
      private 15.19 cluster (found while working RP-432).
      Verify: tests/backlog/test_rp_dml_from.py::test_missing_column_reported_before_ambiguous_one
      Backlog-ID: RP-436
- [ ] **A subquery in a CREATE TABLE AS or CREATE MATERIALIZED VIEW is
      refused.** `create table ct as select (select 1) x from e` is 0A000
      `SubLink is not supported yet`, uncorrelated or correlated (`(select
      e.a + 1)`, `exists(select ... where f.a = e.a)`), and so is `create
      materialized view mv as select (select a + 1) from e`; PostgreSQL
      15.19 makes each table with its rows. `select ... into` of the same
      list is made. Measured on a private 15.19 cluster with `e(a int, b
      text)` (found while working RP-449).
      Verify: tests/backlog/test_rp_ctas_subquery.py::test_subquery_in_create_table_as
      Backlog-ID: RP-466
- [ ] **A missing relation named by a string literal for a `regclass`
      parameter is looked up only if the call runs.** PostgreSQL 15.19 coerces an
      untyped literal argument to its parameter's type during analysis
      (`coerce_type` runs `regclassin`), so a relation it names that does
      not exist is 42P01 `relation "nosuchseq" does not exist` at the
      literal wherever the call sits: `select 1 where false and
      nextval('nosuchseq') > 0` (zero rows here), `select (select
      nextval('nosuchseq') where false)` (a NULL here), `select (select
      nextval('nosuchseq'), 2)` (here the subquery's 42601 width error,
      since RP-452) and `select nextval('nosuchseq') + (1 + true)` (here
      the operator's 42883). An explicit `'nosuchseq'::regclass` is already
      42P01 as it is planned. A statement PostgreSQL refuses runs.
      Measured on PostgreSQL 15.19 (found while working RP-452).
      Verify: tests/backlog/test_rp_regclass_literal.py::test_regclass_literal_is_resolved_at_analysis
      Backlog-ID: RP-458
- [ ] **`SELECT *` with no FROM is refused as a gap.** `select *` answers
      `0A000 this target is not supported yet`; PostgreSQL 15.19's
      `transformTargetList` answers 42601 `SELECT * with no tables
      specified is not valid` at the `*`. Measured on PostgreSQL 15.19
      (found while working RP-441).
      Verify: tests/backlog/test_rp_fromless_analysis.py::test_star_with_no_from
      Backlog-ID: RP-446
- [ ] **An operator's type error is reported ahead of a missing column to
      its left.** `select nosuch + (1 + true)` is 42883 `operator does not
      exist: integer + boolean` here, from `optype` checking every operator
      before the planner meets the column; PostgreSQL 15.19 analyses the
      left operand first and answers 42703 `column "nosuch" does not exist`
      at 8. The operator to the reference's LEFT already comes first on
      both (`select (1 + true) + nosuch`). The same pass as RP-442. Measured
      on PostgreSQL 15.19 (found while working RP-441).
      Verify: tests/backlog/test_rp_fromless_analysis.py::test_missing_column_before_operator_to_its_right
      Backlog-ID: RP-447
- [ ] **An operator over `current_user` or `current_schema` names its
      operand `text`, not `name`.** `select 1 + current_user` is 42883
      `operator does not exist: integer + text` here and `integer + name`
      on PostgreSQL 15.19 (`select current_schema + 1` likewise);
      `pg_typeof` of each is `name` on both, so only the operator check's
      typing of the keyword functions differs. Measured on PostgreSQL 15.19
      (found while working RP-441).
      Verify: tests/backlog/test_rp_fromless_analysis.py::test_keyword_function_operand_is_name
      Backlog-ID: RP-448
- [ ] **A cast literal's input error is reported after the operator
      around it.** PostgreSQL 15.19's `transformTypeCast` runs a constant's
      input function as the cast is transformed, before `make_op` looks the
      operator up, so `select 'x'::int = true` is 22P02 `invalid input
      syntax for type integer: "x"` at 8, `select 1 = 'x'::uuid` 22P02 at
      12 and `select '2020-13-01'::date = 1` 22008 at 8; here each is the
      operator's 42883 (`integer = boolean`, `integer = uuid`, `date =
      integer`), from `optype` checking operand types before any cast's
      value is read. The same in a simple CASE (`case 'x'::uuid when 1`).
      A lone cast is 22P02 on both. Measured on PostgreSQL 15.19 (found
      while working RP-439).
      Verify: tests/backlog/test_rp_fromless_analysis.py::test_cast_literal_input_error_before_operator
      Backlog-ID: RP-442
- [ ] **EXPLAIN reports its option's error ahead of a missing relation, a
      missing function or a bad literal.** PostgreSQL 15.19 analyses the
      statement before `ExplainQuery` reads an option, so `explain (bogus)
      select * from nosuch` is 42P01, `explain (bogus) select nosuchfn(a)
      from e` 42883 and `explain (bogus) select * from e where a = 'x'`
      22P02; here each is 42601 `unrecognized EXPLAIN option "bogus"`.
      Since RP-419 the operator-type, untyped-value and ORDER BY checks run
      first; these are resolved while planning, after the options. A
      planner-time error (`explain (bogus) select 1/0`) correctly stays
      behind the option's. Measured on a private 15.19 cluster (found
      while working RP-419).
      Backlog-ID: RP-428
      Verify: tests/backlog/test_rp_analysis_order.py::test_explain_resolves_names_before_options
- [ ] **A node above a dummy relation prints no estimate, and a dummy
      join no width.** Since RP-409 a WHERE that folds to constant false
      plans a `Result` with `One-Time Filter: false`, costing nothing, as
      PostgreSQL 15.19 does. Above it PostgreSQL still prices each node:
      `explain select count(*) from e where a = null` is `Aggregate
      (cost=0.00..0.01 rows=1 width=8)`, here `cost=0.00..0.00 rows=0
      width=0`; a Sort over it `cost=0.01..0.02 rows=0 width=36`. A dummy
      join's Result keeps the join's width (`select * from e, k where e.a
      = null` is `rows=0 width=14`) and so does an UPDATE's (`width=38`);
      here `width=0`. Measured on a private 15.19 cluster (found while
      working RP-409).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate over a dummy relation is priced], tests/backlog/test_rp_explain_costs.py::test_plan[a dummy join keeps its width]
      Backlog-ID: RP-420
- [ ] **A COALESCE's constant arguments print unfolded.** PostgreSQL
      15.19's `eval_const_expressions` drops a COALESCE's NULL arguments
      and stops at its first non-NULL constant: `select * from e where
      coalesce(null, a) = 1` is `Filter: (COALESCE(a) = 1)`, and
      `coalesce(1, a) = 1` folds to true, a `Seq Scan on e` with no Filter.
      Here `(COALESCE(NULL::integer, a) = 1)` and `(COALESCE(1, a) = 1)`.
      Its rows are right. Measured on a private 15.19 cluster (found while
      working RP-409).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a coalesce drops its null arguments], tests/backlog/test_rp_explain_costs.py::test_plan[a coalesce of a constant first folds]
      Backlog-ID: RP-421
- [ ] **EXPLAIN of a plain FROM-subquery prints a Subquery Scan
      PostgreSQL pulls up.** `pull_up_subqueries` merges a simple subquery
      into its parent: `select * from (select * from e where a > 5) s` is
      `Seq Scan on e` with `Filter: (a > 5)` on PostgreSQL 15.19; here
      `Subquery Scan on s` over a `Seq Scan on e` that prints no Filter.
      Its rows are right. Measured on a private 15.19 cluster (found while
      working RP-409).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a plain from-subquery is pulled up]
      Backlog-ID: RP-422
- [ ] **EXPLAIN prints an uncorrelated subquery inline, with no InitPlan
      and no estimate.** PostgreSQL 15.19 plans `(select 'v3'::text)` as
      an `InitPlan 1 (returns $0)` over a `Result`, run once, and the qual
      reads `$0`: `explain select * from e where b = (select 'v3'::text)` is
      `Seq Scan on e  (cost=0.01..17.51 rows=100 width=7)`, `Filter: (b =
      $0)`, then the InitPlan. Here `Filter: (b = ( SELECT 'v3'::text AS
      text))`, every cost zero, no InitPlan, in a WHERE as in a join. In a
      join's ON (runnable since RP-438) the conjunct also stays in the
      join's `Join Filter`, where PostgreSQL pushes it to `Seq Scan on k`'s
      `Filter: (b = $0)`, inner and LEFT join alike. Its rows are right.
      Measured on a private 15.19 cluster (found while working RP-438).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[an uncorrelated subquery is an initplan], tests/backlog/test_rp_explain_costs.py::test_plan[a join's on subquery is an initplan on its scan]
      Backlog-ID: RP-440

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


## Found while working RP-443 (2026-11-07)

Measured on a private PostgreSQL 15.19 cluster with `fe(a int, b text)`
and `fs(a int, c text)`, one row each.

- [ ] **A join's alias is refused or not found.** `select u.a from fs join
      fe using (a) as u` (the `USING ... AS` alias PostgreSQL 14 added) and
      `select j.a from (fs join fe using (a)) as j` answer 1 on PostgreSQL
      15.19. Here the first is 42P01 `missing FROM-clause entry for table
      "u"`, the second 0A000 `an aliased JOIN is not supported yet`
      (`joins.rs`), and inside a subquery the USING alias reaches the
      correlated rewrite: `select (select u.a from fs join fe using (a) as
      u limit 1)` is 42883 `function \x1fcorrelated(...) does not exist`,
      naming the rewrite's internal function.
      Backlog-ID: RP-454
      Verify: tests/backlog/test_rp_subquery_scope.py::test_join_alias

## Found while working RP-444 (2026-11-07)

- [ ] **A subquery inside an EXISTS subquery's select list is not
      analysed.** `select exists (select (select nosuch))` and `select
      exists (select 1 + (select nosuch))` answer `true` here; PostgreSQL
      15.19's `transformSubLink` analyses the inner subquery with the rest
      of the list, so each is 42703 `column "nosuch" does not exist` at 31
      and 35. A statement PostgreSQL refuses runs. Since RP-444 a column
      reference in the list, ORDER BY or DISTINCT ON is kept for the
      planner when the subquery's own tables do not answer it; a nested
      subquery cannot simply be kept, as it would then RUN (`exists (select
      (select 1/0))` is `true` on PostgreSQL), so it needs an analysis that
      plans without executing. Measured on PostgreSQL 15.19.
      Verify: tests/backlog/test_rp_fromless_analysis.py::test_exists_target_subquery_is_analysed
      Backlog-ID: RP-459

## Found while working RP-453 (2026-11-07)

- [ ] **An UPDATE ... FROM's or a DELETE ... USING's RETURNING subquery
      cannot read the FROM.** `update fe set b = 'w' from fs returning
      (select fs.a from fe limit 1)` answers 1 on PostgreSQL 15.19, `delete
      from fe using fs returning (select fs.c from fe limit 1)` `y`, and the
      same through an alias (`from fs x ... x.c`); the FROM and USING items
      are relations of the statement's own level. Here each is 42P01
      `missing FROM-clause entry for table "fs"` (or `"x"`). A subquery over
      the write's target alone already reads it. Measured on PostgreSQL
      15.19.
      Backlog-ID: RP-457
      Verify: tests/backlog/test_rp_subquery_scope.py::test_returning_subquery_reads_the_from

## Found while working RP-455 (2026-11-07)

- [ ] **A subquery inside a FROM-subquery is offered a sibling's alias as
      its hint.** PostgreSQL 15.19 analyses a FROM-subquery that is not
      LATERAL with the FROM items before it in its level's range table but
      out of its namespace (`p_lateral_only`), so `select * from fe x,
      (select (select fe.a)) s` and `select * from fe x, (select 1 where
      exists (select fe.a)) s` are 42P01 `invalid reference to FROM-clause
      entry for table "fe"` with the hint `There is an entry for table "x",
      but it cannot be referenced from this part of the query.`; here the
      hint is `Perhaps you meant to reference the table alias "x".`, an
      alias the client cannot use there. `resolve_sublinks_in_select`
      pushes the whole FROM, every item visible, onto `OUTER_RELATIONS` and
      `OUTER_RANGE_LEVELS` before it resolves the FROM-subqueries, so
      `missing_rte_hint` finds the alias visible; a LATERAL one may see only
      the items to its left. (The EXISTS forms naming the sibling itself,
      `exists (select x.a)`, answered rows when this was filed; RP-444 made
      them 42P01.) Measured on PostgreSQL 15.19.
      Verify: tests/backlog/test_rp_subquery_scope.py::test_from_subquery_sibling_is_not_visible
      Backlog-ID: RP-460

## Found while working RP-451 (2026-11-07)

Measured on a private PostgreSQL 15.19 cluster with `fe(a int, b text)` and
`fs(a int, c text)`, one row each (`(1, 'x')`, `(1, 'y')`).

- [ ] **A SQL function's bare parameter name is replaced wherever it is a
      word.** PostgreSQL resolves an unqualified name as a column first and
      as a parameter only when no column of that name is in scope
      (`sql_fn_post_column_ref`), and a name that is not a reference at all
      -- an output alias -- is never a parameter: `gb(a int)` with the body
      `select a from fe` answers fe's 1 for `gb(7)`, and `gal(p int)` with
      `select 1 as p` answers 1. Here every identifier token naming a
      parameter becomes `$n` (`bind_parameter_names`), so the first answers
      7 and the second is 42601 `syntax error at or near "$1"`. The
      faithful fix resolves a parameter where the planner fails to resolve
      a column (the post-columnref hook), not by token; RP-451 resolved the
      qualified form `fname.p` by token with a catalog check, which that
      hook would replace.
      Verify: tests/backlog/test_rp_function_bodies.py::test_bare_parameter_name
      Backlog-ID: RP-461
- [ ] **A composite parameter's field is not read.** `gr1(r fe)` with
      `select r.a` and `gr2(r fe)` with `select gr2.r.a` answer 2 for
      `row(2, 'z')::fe` on PostgreSQL; here both are 42P01 `missing
      FROM-clause entry for table "r"`.
      Verify: tests/backlog/test_rp_function_bodies.py::test_composite_parameter_field
      Backlog-ID: RP-462
- [ ] **A PL/pgSQL block label does not qualify its variables.** `<<blk>>
      declare x int := 3; begin return blk.x + p; end` answers 10 for
      `gl(7)` on PostgreSQL; here 42P01 `missing FROM-clause entry for table
      "blk"`. The function's own name, the label of the block its
      parameters are declared in, qualifies them since RP-451; a label the
      body writes does not.
      Verify: tests/backlog/test_rp_function_bodies.py::test_block_label_qualifies_a_variable
      Backlog-ID: RP-463
- [ ] **CREATE FUNCTION ... LANGUAGE sql does not check its body.** With
      `check_function_bodies` on, its default, PostgreSQL parses and
      analyses each statement at CREATE: a body `select gx.q` is 42P01
      `missing FROM-clause entry for table "gx"` at 65, `select nosuch`
      42703 `column "nosuch" does not exist` at 65, `select 1 from nosuch`
      42P01 `relation "nosuch" does not exist` at 72. Here each CREATE
      succeeds and the error waits for the first call.
      Verify: tests/backlog/test_rp_function_bodies.py::test_create_checks_the_body
      Backlog-ID: RP-465

## Found while working RP-464 (2026-11-07)

- [ ] **A function called in FROM runs its body twice.** `select * from
      fz()`, where `fz()` returns `setof int` with the body `insert into t
      values (100); select 1`, inserts one row on PostgreSQL 15.19 and two
      here; a scalar `fq()` called as `select * from fq()` does the same,
      and `select * from fs(10)` over `update t set a = a + p returning a`
      adds 20 to every row and answers 21, 22, 23 where PostgreSQL adds 10
      and answers 11, 12, 13. The same call in a select list (`select
      fq()`) runs once. A silent double write; the extended protocol does
      it too. Measured on PostgreSQL 15.19.
      Backlog-ID: RP-472
      Verify: tests/backlog/test_rp_from_function_runs_once.py::test_from_function_runs_once

## Found while working RP-316 (2026-11-02)

- [ ] **Every CREATE VIEW takes over a second.** Each one rebuilds
      `rule_trees`' catalog snapshot by running internal queries over the
      whole of `pg_namespace`, `pg_class`, `pg_attribute`, `pg_type`,
      `pg_proc` and the rest (`rule_trees.rs`, `text_rows`), to render the
      view's `pg_rewrite` tree. On an M-series Mac, `CREATE VIEW v AS SELECT a
      FROM t` takes 1.3 s (1.6 s before `system_relation_columns` was
      built once). PostgreSQL 15.19 takes about a millisecond. On a loaded CI
      runner, `test_rp_found_pgtest.py::test_view_columns_name_their_base_columns`
      went past its client's 10 s socket timeout twice on PR #2265, and
      again on PR #2332's `pg-oracle (server)` job (2026-11-04; that PR
      changes EXPLAIN only, and locally the test took 3.5 s).
      Since 2026-11-05 PostgreSQL's own rows (oid below 16384) are read
      once per process and only the user's on each call: 0.5 s in a debug
      build (1.3 s before). What is left is the user rows' reads, each
      through a virtual catalog that builds its system rows too before the
      `>= 16384` filter, cold after every DDL (`pg_attribute` 0.39 s of it),
      and a CREATE VIEW reads them twice (`statement_rule_refusal`, then
      `statement_rule_trees`). The snapshot should hold only what the view's
      tree references, as `SearchSysCache` looks rows up one by one, or be
      kept up to date as DDL changes the catalogs.
      Backlog-ID: RP-336
      Verify: none -- a wall-clock cost; a timing assertion would flake, so the measurement above is the evidence
- [ ] **OPEN -- `pgbench -i -s 1` took over 120 s once in CI.**
      `tests/backlog/test_rp_found_2026_10_20.py::test_client_tools`
      (RP-071's regression test) failed on PR #2356's `pg-oracle (server)`
      job (run 42077464172, job 127421380691; EXPLAIN costing only):
      `pgbench -i -s 1` against the server hit the test's 120 s
      `TimeoutExpired`. The runner was slow throughout (installing
      PostgreSQL 15 took about 19 minutes; the backlog step ran 996 s
      against 580-654 s on `main`'s runs of the same day), and the same
      test passed on `main`'s next run. Locally (macOS, release build,
      idle) the whole test takes 27 s. Not diagnosed: which phase of
      `pgbench -i` (the 100,000-row COPY, VACUUM, the primary keys) used
      the time is not in the log, so the next step is the server's log of
      a slow run with each statement's duration. Seen a second time on
      PR #2397's `pg-oracle (backlog)` job (run on commit 05d7a19c, RP-404
      and RP-408; 2026-11-06): pgbench had printed `creating tables...` when
      the 120 s ran out, and that job's backlog step took 1102 s against
      476 s on PR #2395's. Locally, a debug build of the same commit runs
      `pgbench -i -s 1` in 22 s, as does the commit before it.
      Backlog-ID: RP-380
      Verify: none -- seen once in CI; a timing that does not fail on demand

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

- [ ] **OPEN — `information_schema.data_type_privileges` and
      `element_types` take a quarter to half a second** (optimised build
      without LTO, a fresh database, `count(*)`, interleaved runs):
      `routines` 0.046 s, `parameters` 0.11 s, `data_type_privileges`
      0.25 s, `element_types` 0.43 s, where PostgreSQL 15.19 takes 10-50
      ms. The same build took 0.09, 0.21, 0.49 and 0.85 s before the
      follow-up (2026-10-27: a LATERAL built-in function scan planned once,
      `ExecReScanFunctionScan`; `nameconcatoid` as `name.c` has it; call
      resolution, operator operand checks, value-less node types and
      declared parameter types kept per planned expression; unread
      FROM-subquery outputs left out of materialised rows, and join leaves
      holding only carried columns; system views not trial-planned before
      pruning), 0.85, 1.0, 2.0 and 13.7 s before the top-ten batch's second
      pass, and 3.1, 2.2, 5.6 and 62 s before its first. Every row of the
      four views is the same as before and as 15.19's. What is left:
      - `element_types`' row IN is still a hashed SubPlan run while the
        statement is planned. PostgreSQL pulls it up
        (`convert_ANY_sublink_to_join`) and hash-joins the unique-ified
        inner (Unique over Sort); here that needs a semi-join in `joins` /
        `join_search`. Since RP-306 the pulled-up form, a join to
        `(SELECT DISTINCT ...)`, keys on the domain columns and answers
        (about 6 s in a debug build), so it is no longer refused by
        memory. Either way `element_types` costs at
        least `data_type_privileges`;
      - each view layer still materialises documents
        (`materialise_sub_cells`, `lateral_rows`): building BSON documents
        is the largest share left, about 25% of `routines`;
      - the evaluator still asks operand types with the value in hand on
        operator paths (`static_json_type`, `typed_string_operand`, the
        bpchar check), and a domain cast clones the domain for every row
        (`domains::user_domain`).
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
