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
- **Scope decision — REPEATABLE READ answers 40001 after a wait when ANY
  transaction committed since its snapshot**, where PostgreSQL goes on unless
  the ROW changed. WiredTiger cannot continue a transaction after a conflict,
  so going on means a new snapshot, which under REPEATABLE READ is invisible
  only when nothing it could read later committed -- unknowable for the rest
  of the transaction. Narrowing to the conflicting table would let a later
  read see rows committed after the snapshot: a wrong answer, worse than a
  40001 every client must retry anyway.

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
  - A bitmap heap scan's order after a HOT UPDATE: PostgreSQL reaches the
    new version through its chain's root and reads it at the root's place
    (`select id from t where id > 0` over a primary key reads rows 3, 1, 2
    where this server, reading in ctid order, returns 3, 2, 1). The size
    CREATE INDEX records and what ANALYZE stores are modelled (`__sql_relstats__`), but not REINDEX's,
    CLUSTER's or an ALTER TABLE that builds an index, nor across RENAME.
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
    before); RIGHT joins 27 of 33. What is left is a LEFT join whose
    nullable side holds another LEFT join -- `make_outerjoininfo` widens
    its sides by the lower join's, which is not modelled, so the search
    declines it -- and FULL joins. Not yet: two classes spanning both sides (two join keys); an
    expression in a class; a LEFT join's class through the general path;
    a class of text or other non-integer keys, or with a LEFT join; three
    tables with a WHERE over two of them or a LIMIT; four or more tables
    with a WHERE over two of them or a LIMIT; more than eight tables; an expression key with a function
    call or a cast that costs; parameterized nested loops (an index scan inner); a LIMIT with
    an ORDER BY, or with a WHERE over the general path; a join of more
    than 200,000 rows a side, which streams in scan order.
  - Sorts that spill to disk runs are not PostgreSQL's merge.
  Backlog-ID: RP-028
  Verify: tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_whose_nullable_side_holds_a_left_join_is_searched, tests/backlog/test_rp_batch47.py::test_scenario[a bitmap heap scan reads a HOT-updated row at its root]

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

## Open from the 2026-09-30 batch

- [ ] **Rust PG server: a SAVEPOINT captures whole tables, so its cost is
      the size of what the block writes.** WiredTiger has no savepoint, so
      one here is a set of pre-images: the first write to a table after a
      savepoint copies that table's documents into it. Correct for any size,
      but a savepoint taken before a write to a large table copies the whole
      table into memory. Lazy capture is what keeps the common case free (a
      savepoint nobody writes through costs nothing), and a per-row undo log
      would be the fix if the cost ever bites.
      Backlog-ID: RP-018
      Verify: none -- savepoint memory cost design; no answer differs
- [ ] **Rust PG server: what the general JOIN planner does not push into
      its table leaves: a subquery.** A WHERE conjunct that reads ONE
      alias's columns (qualified, or unqualified and in only one table),
      built of operators, casts, constants, parameters and calls of
      IMMUTABLE built-ins, is applied inside that leaf (only on the
      PRESERVED side of every outer join above it); corpus `join_pushdown`,
      unit tests `joins::tests` (immutable calls since 2026-10-19). Not
      pushed: a conjunct holding a subquery, which PostgreSQL runs once as
      an InitPlan beside the scan. The pushed copy also stays in the outer
      WHERE, so pushing one here would run the subquery twice, and a
      volatile one would answer differently; it needs the conjunct moved
      rather than copied. A volatile or stable call, or a user function,
      stays out for the same reason. (A conjunct over two aliases is a join
      clause in PostgreSQL too, not a leaf filter.)
      Backlog-ID: RP-019
      Verify: none -- predicate pushdown/index use only; answers identical, EXPLAIN costs are a scope decision

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

- [ ] **SERIALIZABLE is snapshot isolation: write skew commits.** Two
      SERIALIZABLE transactions each read both on-call rows and set their
      own to false; both commit here, leaving nobody on call. PostgreSQL
      15.19 fails one with 40001. `secantus-pg` treats SERIALIZABLE as
      REPEATABLE READ. PostgreSQL's mechanism is SSI: predicate (SIREAD)
      locks and rw-conflict tracking. The REPEATABLE READ scope decision
      above is a different question.
      Backlog-ID: RP-061
      Verify: tests/backlog/test_rp_found_2026_10_20.py::test_serializable_write_skew
- [ ] **A database created with a non-UTF8 `ENCODING` still behaves as
      UTF8.** `CREATE DATABASE ... ENCODING 'LATIN1'` is recorded (and
      `pg_database` shows it), and TEMPLATE, OWNER, CONNECTION LIMIT,
      IS_TEMPLATE, ALLOW_CONNECTIONS and ALTER DATABASE are PostgreSQL's
      (2026-10-22). Inside such a database PostgreSQL 15.19 reports
      `server_encoding` as LATIN1, defaults `client_encoding` to it, refuses a
      character LATIN1 lacks on the way from the client (22P05), and its byte
      functions count LATIN1 bytes: `octet_length('é')` is 1, `convert_to`
      and `::bytea` give `\xe9`, `chr(8364)` is 54000. Here every database is
      UTF8 inside. **Decided (Joe, 2026-10-22): emulate the encoding** --
      PostgreSQL's model, the server encoding per database with the
      client-to-server conversion and the byte functions honouring it -- as
      its own slice.
      Backlog-ID: RP-064
      Verify: tests/backlog/test_rp064_encoding.py::test_latin1_database_behaves_as_latin1
- [ ] **The cumulative statistics views cannot be queried** (42P01): the 21
      `pg_stat_*_tables` / `_indexes` / `_functions`, `pg_stat_xact_*`,
      `pg_stat_database` and `pg_statio_*` views. Every other relation in
      PostgreSQL 15.19's `pg_class` (`tests/backlog/rp070_catalog_relations.txt`)
      now answers, as PostgreSQL's own view definition over this server's
      catalogs, from PostgreSQL's built-in rows, or from this server's state.
      **Decided (Joe, 2026-10-22): model the counters, refuse the
      internals** -- PostgreSQL's cumulative-statistics model (per-table,
      per-index and per-function counters, the database's counters,
      persisted across restarts) behind those views; the server's internals
      (`pg_backend_memory_contexts`, `pg_shmem_allocations`,
      `pg_stat_bgwriter`, `pg_stat_wal`, `pg_stat_slru`, `pg_stat_archiver`)
      refused by name, 0A000, which the gap test expects. Relations refused
      for reasons other items record (RP-098 .. RP-101) are left to theirs.
      Backlog-ID: RP-070
      Verify: tests/backlog/test_rp_found_2026_10_20.py::test_catalog_relations_are_queryable

## Found by batch 30 (2026-10-22)

Found while building `REASSIGN OWNED` and `DROP OWNED` (RP-072). The gap
tests are scenarios in `tests/backlog/test_rp_batch30.py`, measured on a
private 15.19 cluster. RP-133 .. RP-136 and RP-138 .. RP-142 were found and
fixed in the same batch; their tests there are regression tests.

- [ ] **A relation's oid is not its place in creation order.** PostgreSQL
      draws every oid from one counter, so oids rise as objects are made,
      and DROP ROLE's DETAIL lists the role's objects in oid order
      (`shared_dependency_comparator`): a sequence made before a table is
      listed first. Here a sequence's, a view's and an index's oid is a
      hash of its name in a band of its own (`sequence_oid`, `view_oid`,
      `index_oid`), so the DETAIL -- which this server sorts by oid as
      PostgreSQL does -- and any `ORDER BY oid` come out in another order.
      DROP SCHEMA ... CASCADE's notice lists a schema's members in the same
      creation order (`table s.t`, `function s.f()`, `view s.v`); here they
      come grouped by kind -- types, routines, views, tables, sequences --
      since nothing records which was made first.
      Backlog-ID: RP-137
      Verify: tests/backlog/test_rp_batch30.py::test_scenario[DROP ROLE lists dependents in creation order], tests/backlog/test_rp_batch31.py::test_scenario[DROP SCHEMA lists its members in creation order]

## Found by batch 51 (2026-10-23)

Found while building operator classes, subscriptions and transforms
(RP-169). The gap tests are scenarios in `tests/backlog/test_rp_batch51.py`,
measured on a private 15.19 cluster. RP-201 (`shobj_description` answered
NULL for a database's or role's comment) was found and fixed in the same
batch; its test there is a regression test.

- [ ] **`pg_describe_object`, `pg_identify_object`,
      `pg_identify_object_as_address` and `pg_get_object_address` are
      refused** (`function pg_describe_object(regclass, regclass, integer)
      is not supported yet`). PostgreSQL 15.19 describes any object by
      `(classid, objid, objsubid)`: `table pg_class`, `column relname of
      table pg_class`, `operator class c for access method btree`. The
      descriptions exist here for the error and notice texts that name
      objects (a 2BP01's DETAIL, a CASCADE notice), one kind at a time; the
      functions need `getObjectDescription`'s switch over every catalog.
      Backlog-ID: RP-199
      Verify: tests/backlog/test_rp_batch51.py::test_scenario[pg_describe_object and its siblings]
- [ ] **Inside a block, `pg_get_indexdef` does not see an index the block
      created.** It answers NULL until COMMIT, where PostgreSQL 15.19
      answers the definition; `pg_indexes.indexdef` shows it at once. The
      planner's published definition tables (index definitions, shared
      comments for `shobj_description`) are read from the committed
      catalog at the start of each statement, before the statement takes
      the transaction's own session. `pg_get_constraintdef` is published
      the same way and is right only because a constraint's definition
      lives in the table's own catalog row.
      Backlog-ID: RP-200
      Verify: tests/backlog/test_rp_batch51.py::test_scenario[pg_get_indexdef of an index the block created]

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

- [ ] **`pg_class` shows no page or tuple counts for an index.** A table's
      `relpages`, `reltuples` and `relallvisible` are what VACUUM, ANALYZE
      and CREATE INDEX recorded (batch 49). An index's row still reads 0 and
      -1, where PostgreSQL 15.19 shows its own pages and entries: an index
      on 200 integers is `2 | 200` after CREATE INDEX. The pages need a
      model of the B-tree as inserts split it; `costsize::btree_size`
      estimates a freshly built one only.
      Backlog-ID: RP-196
      Verify: tests/backlog/test_rp_batch49.py::test_scenario[pg_class shows an index's size]

## Found by batch 46 (2026-10-23)

Found while building the system columns (RP-069). The gap tests are
scenarios in `tests/backlog/test_rp_batch46.py`, measured on a private 15.19
cluster. RP-194 was found and fixed in the same batch, RP-192 in batch 47
and most of RP-193 in batch 48; their tests there are regression tests.

- [ ] **A heap's dead tuples, free space map and target blocks are lost
      on a restart.** The heap is modelled as PostgreSQL 15.19 keeps it
      (`crates/secantus-pg/src/heap_page.rs`, `fsm.rs`; batch 48): HOT
      chains, on-access pruning, VACUUM's passes, the free space map and
      each backend's target block. But it is kept in the server process,
      and after a restart it is derived from the live rows' ctids. Every
      place no row holds is then a dead line pointer with no storage: a HOT
      chain's dead root, which PostgreSQL's VACUUM leaves a redirect, is
      made unused and reused; a dead tuple's space is free before any
      VACUUM; and the free space map is empty. PostgreSQL keeps all three
      on disk. The fix is persisting the heap (and FSM) state beside the
      rows.
      Backlog-ID: RP-193
      Verify: tests/backlog/test_rp_batch48.py::test_a_dead_tuple_outlives_a_restart

## Found by batch 34 (2026-10-23)

Found while building `CREATE TEXT SEARCH CONFIGURATION` (RP-072). The gap
tests are scenarios in `tests/backlog/test_rp_batch34.py`, measured on a
private 15.19 cluster. RP-166 and RP-167 were found and fixed in the same
batch; their tests there are regression tests.

- [ ] **What is left of the statements once refused with 0A000.** Batch
      51 built `CREATE / ALTER / DROP OPERATOR FAMILY / CLASS` for the
      btree and hash methods, text-search dictionaries over the `simple`,
      `snowball` and `synonym` templates, subscriptions created with
      `connect = false`, and transforms. Three parts are still refused or
      differ from PostgreSQL 15.19:
      - **Operator classes of the GiST, GIN, SP-GiST and BRIN methods**
        are refused by name. Their `amadjustmembers` rules and support
        function numbers differ from btree's, and an index over such a
        class would need its support functions, which are C internals.
      - **The `ispell` and `thesaurus` dictionary templates** are refused
        by name: their dictionary and affix file formats are not read here.
      - **`pg_subscription` lists only the current database's
        subscriptions.** It is a shared catalog: PostgreSQL lists every
        database's, each with its `subdbid`. Here they are kept per
        database.
      Backlog-ID: RP-169
      Verify: tests/backlog/test_rp_batch51.py::test_scenario[a GiST operator class], tests/backlog/test_rp_batch51.py::test_scenario[ispell and thesaurus dictionaries], tests/backlog/test_rp_batch51.py::test_scenario[pg_subscription lists every database's subscriptions]

## Found by batch 26 (2026-10-22)

Found while making PostgreSQL 15's `pg_dump`, `pg_dumpall` and `psql` answer
as they do against PostgreSQL 15.19 (RP-070, RP-071). The gap tests are
scenarios in `tests/backlog/test_rp_batch26.py`, measured on a private 15.19
cluster.

- [ ] **Extended statistics: timestamps, and tables past ANALYZE's
      sample.** ANALYZE builds every kind of statistics object as PostgreSQL
      15.19 does (batch 50: ndistinct, dependencies, MCV lists, expression
      statistics, inherited ones), and `pg_statistic_ext_data`,
      `pg_stats_ext` and `pg_stats_ext_exprs` show them. What is left:
      - An MCV list over a `timestamp` or `timestamptz` column has no bytes
        (`stxdmcv` is NULL, where PostgreSQL writes its datums). A
        timestamp's microseconds are kept in a hidden companion field, which
        the statistics' values do not carry, so two timestamps a fraction of
        a millisecond apart also count as one value.
      - A table of more rows than ANALYZE samples (300 times the statistics
        target) has no extended statistics here: PostgreSQL builds them from
        its random sample (as for `pg_statistic`, which is not built either).
      Backlog-ID: RP-099
      Verify: tests/backlog/test_rp_batch50.py::test_scenario[an MCV list's bytes over timestamps], tests/backlog/test_rp_batch50.py::test_scenario[a table past ANALYZE's sample has extended statistics]
- [ ] **The progress views are refused by name** (0A000):
      `pg_stat_progress_vacuum`, `_analyze`, `_cluster`, `_copy`,
      `_create_index`, `_basebackup`. PostgreSQL reports a row per backend
      running that command, which a COPY or CREATE INDEX here never
      publishes.
      **Decided (Joe, 2026-10-22): stream COPY, slots for every command.**
      COPY FROM parses and inserts rows as CopyData arrives, as PostgreSQL
      does (it buffers to CopyDone today), so `pg_stat_progress_copy`'s
      counters move as PostgreSQL's do; every command keeps a per-backend
      progress slot read through `pg_stat_get_progress_info` and
      PostgreSQL's own six view definitions; CREATE INDEX reports the
      phases this server's build has.
      Backlog-ID: RP-100
      Verify: tests/backlog/test_rp_batch26.py::test_scenario[progress views]
- [ ] **An extension's members are not in `pg_depend`.** Every other
      object's dependencies are recorded as PostgreSQL 15.19 records them
      (`crates/secantus-pg/src/depend.rs`), an extension's schema and types
      among them. But an extension's functions, operators, casts and
      operator classes and families -- citext's 47, 26, 8, 3 and 3 -- are
      this server's built-ins, with no `pg_proc` / `pg_operator` /
      `pg_cast` / `pg_opclass` rows of their own, so there is nothing to
      record their membership (`e`) against. The fix is those rows: each
      extension's objects as its script creates them, in its schema.
      Backlog-ID: RP-105
      Verify: tests/backlog/test_rp_batch45.py::test_scenario[an extension's members]

## Found by batch 28 (2026-10-22)

Found while measuring `pg_depend` (RP-105) against PostgreSQL 15.19. The gap
tests are scenarios in `tests/backlog/test_rp_batch26.py`.

- [ ] **The `information_schema` views over every built-in function take
      seconds** (release build, a fresh database): `routines` 3.6 s,
      `parameters` 2.5 s, `data_type_privileges` 6.4 s, `element_types` 52 s,
      where PostgreSQL 15.19 takes 2-47 ms. They answer PostgreSQL's rows
      since RP-102 listed the 3,244 built-ins. The cost is mostly a LATERAL
      FROM function (`_pg_expandarray` becomes `unnest(...) WITH
      ORDINALITY`) re-planned and run as SQL once per left row
      (`PgHandler::lateral_rows`), and per-row expression evaluation over
      the joins.
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
