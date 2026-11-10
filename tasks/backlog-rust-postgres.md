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

- [ ] **NOT REPRODUCED -- left open as a watch item. A `CREATE VIEW`
  went unanswered for 10 s once (2026-11-08).** `tests/backlog/test_rp_found_pgtest.py::test_view_columns_name_their_base_columns`
  (a regression test) failed in CI's `pg-oracle (backlog)` lane on
  snapshot PR #2497 (`b8976e15`): `TimeoutError: timed out` in
  `pgwire_raw.py`'s `_until_ready`, waiting for ReadyForQuery after the
  test's `CREATE VIEW rp_v ... FROM rp_tab1 JOIN rp_tab2` (traceback line
  328). The raw client's socket timeout is 10 s, so the server sent
  nothing for 10 s after a DDL statement. The `rust_pg` server is shared
  by the module's tests under `-n 4`, so one explanation is DDL waiting on
  a lock another test held, but that was not measured. The snapshot's only
  change was RM-342, in `secantus-commands`, which the PG server does not
  link. It did not fail in the 24 previous runs of that lane; a rerun was
  started. Found by the MongoDB batch; not worked from that side. On a
  recurrence, capture `pg_locks` / the server's lock waits at the moment
  of the stall and which other test was running.
  Backlog-ID: RP-606
  Verify: none -- intermittent, not reproduced; the regression test is the check
- [ ] **NOT REPRODUCED -- left open as a watch item. A cancel during the
  parse was honoured late on Linux CI twice, in both of RP-002's
  regression tests (2026-11-08, 2026-11-10).** `tests/backlog/test_rp_triage.py::test_cancel_during_parse_is_honoured_with_user_code_present`
  (RP-002's regression test) failed in CI's `pg-oracle (backlog)` lane on
  snapshot PR #2480 (`e8272d38`): `cancel honoured 0.65 s after it was
  sent`, budget 0.25 s. RP-002's own record puts the unfixed bug at about
  0.6 s on CI, where the lock pass swallowed the cut-short parse and later
  passes parsed the statement again. So this looks like an intermittent
  return of that path, not slowness. A rerun of the same commit passed,
  and the test passed in each of the 30 previous runs of that lane (main,
  pg-batch-ci, rm-batch-ci). The snapshot carried RM-320, which changed
  the shared storage crate (a transaction older than an index refuses to
  write past it). Found by the MongoDB batch; not worked from that side.
  On a recurrence, capture which passes parsed the statement and when each
  checked the cancel flag.
  **Second recurrence (2026-11-10), in the other test:**
  `test_cancel_during_parse_is_honoured_at_once` failed in the same lane on
  snapshot PR #2591 (`2d70153d`, job 129547189731): `cancel honoured 0.53 s
  after it was sent`, budget 0.5 s, the 57014 at 1.03 s where PostgreSQL
  15.19 answers at 0.515 s. The snapshot's one change was RP-741 (NULLIF's
  operand typing), which no parse or cancel path reaches. It was not
  reproduced on macOS (130 runs, up to 4 at once at load ~14, all within
  0.09 s). The same day RP-748 (RP-202's test, late under load) was traced
  to a loop in `optype::check_aexpr` with no cancellation point, over every
  item of the `IN` list (RP-530's row check), about 70 ms in a debug build
  and half a second under load. This statement's `1 in (...)` goes through
  that loop too, so it is the likely cause here as well, but neither
  RP-002 failure was traced, so this stays a watch item. Both RP-002 tests
  now print the server's cancel trace (`SECANTUS_PG_CANCEL_TRACE`) when
  they miss their budget: on a recurrence, read which stretch the trace
  shows without a check. A third the same morning: the user-code test
  at 0.33 s against 0.25 s on RM snapshot PR #2601 (job 129641495115),
  whose change (RM-385, MongoDB statement statistics) reaches no PG path.
  Backlog-ID: RP-571
  Verify: none -- intermittent, Linux CI only, not reproduced on macOS; RP-002's two regression tests are the check, and print the cancel trace on a miss
- [ ] **OPEN -- `gate_pg.sh`'s login step differs from the local reference
  in 6 of 547 TLS scenarios, because that reference now links OpenSSL 4.**
  Homebrew's `postgresql@15` (15.19_1, 2026-11-07) links
  `openssl@4/lib/libssl.4.dylib`. Since then
  `tests/test_rust_pg_login_differential.py` (`pg_login_tls_files.py`)
  reports `541 of 547 the same` on every local run, and again on
  2026-11-09 in the RM-379 lane. The six are the CRL distribution-point
  scenarios: no distribution point (good, revoked), distribution point a
  (good, revoked), distribution point b, and revoked-first-answer. In each
  of them the reference answers `tlsv1 alert unknown ca`, even for a good
  certificate. The Rust PG server accepts the good certificate, or answers
  `certificate revoked`. Neither server's code changed. RP-013's record
  says the OpenSSL version decides how a CRL on the root is checked. CI's
  Ubuntu reference links OpenSSL 3, which is why CI still passes. Until
  this is settled, the local login step cannot tell a regression from this
  difference. The fix is either a local reference PostgreSQL linked against
  OpenSSL 3, or CRLs that the probe issues so that OpenSSL 3 and 4 agree,
  with the decision recorded in RP-013. Found by the MongoDB batch, and not
  worked from that side.
  Verify: none -- the local reference's OpenSSL decides it; `tests/test_rust_pg_login_differential.py` against a PostgreSQL 15 on OpenSSL 4 is the check
  Backlog-ID: RP-747
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

## Found by a CI run (2026-11-09)

- [ ] **OPEN — any DDL statement makes the next catalog query re-decode
      every catalog collection.** `bump_catalog_version` (`lib.rs`, the
      `CatalogCache` comment) moves one version after every statement that
      may change the catalog, and the next read of any catalog then decodes
      all of them again. PostgreSQL invalidates only the cache entries for
      the rows that changed (`inval.c`, `CatCacheInvalidate`, a relation's
      relcache entry), so its first query after DDL costs about what the
      next one does. Measured 2026-11-09 with psql's `\d` collation
      subquery (`tests/backlog/test_rp_catalog_latency.py`) after a
      `CREATE TABLE`:
      - PostgreSQL 15.19 (private cluster): 0.9 ms first, 0.2 ms after.
      - Debug `secantusd-pg`: 0.58 s first, 0.016 s after; and the first
        query grows with the schema: 0.68 s with 50 more tables, 0.97 s
        with 150, 1.79 s with 400 (0.044 s after).
      A slow CI runner took the first query to 2.2 s and failed RP-077's
      latency test on PR #2552 (job 129166397146), which until then timed
      that cold read instead of the subquery it was written for.
      The faithful fix keys the decoded rows per catalog row, or per
      relation, and invalidates what a statement changed, as `inval.c` does;
      `catcache.rs` already models which lookups miss.
      Backlog-ID: RP-715
      Verify: tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_what_the_next_does

## Found by a CI run (2026-11-07)

- [ ] **OPEN — CI: `obj_description` of a commented index answered NULL
      twice; not reproduced.** Commit `f96fc90d`, job 128491514608
      (`pg-oracle (differential)`),
      `tests/test_rust_pg_corpora.py::test_corpus_answers_as_postgresql_does[sqlalchemy_catalog]`.
      The setup ran `COMMENT ON INDEX sa35_s.sa35_d_ix IS 'ix comment'`, and
      `SELECT obj_description('sa35_s.sa35_d_ix'::regclass, 'pg_class')`
      then answered NULL where PostgreSQL answers `ix comment`. About 2,000
      local runs, including runs under load and runs with forced
      interleavings, never reproduced it. Two things that could each
      produce exactly this symptom were found and fixed.
      First, every storage read on the comment path answered a failure
      with "no comment", and the publication was then recorded as current
      for that catalog version. Those reads were the index comments, the
      catalog, each table's index list, and `COMMENT ON INDEX`'s own lookup.
      Second, the corpus test ignored a failed setup statement.
      Since 2026-11-07 those reads raise `XX000` naming the storage error.
      The publication is not recorded, and a failed corpus setup statement
      fails its test with its error (regression test:
      `crates/secantus-pg/tests/embedded.rs::a_failed_comment_read_is_an_error_not_a_null`).
      **If it recurs, the job log names the cause.** Read the failing
      statement's error, or the setup statement the test now reports. A
      NULL with no error would mean the comment was never written, or was
      published to a planner thread that missed it, and the transient
      storage read is ruled out.
      Some reads under `object_comments` still answer a failure as "none":
      the constraint, text-search, conversion, opclass, subscription and
      transform comment helpers, and `shobj_description`'s
      `shared_comments`. They were not on this corpus's path, and their
      first failure should be made to surface the same way.
      **It recurred on 2026-11-09, as the NULL with no error.** RM snapshot
      #2584, run 42717349016, job 129502313793, 23:17 UTC: the same check
      answered NULL, 1 of 52 differ, 1 failed and 1,183 passed. Every setup
      statement succeeded, because the test now fails a setup error first,
      and no statement raised. The rerun of the same job on the same commit
      passed (job 129513427390). The snapshot changed no code on this path.
      That rules out the transient storage read. What remains: the comment
      was written under a key the later read does not find, or the planner
      thread that ran the check held a publication of `OBJECT_COMMENTS`
      from before the write. Locally, 600 more runs did not reproduce it
      (macOS arm64, a binary of tree `72006e6c`): 200 serial, and 400 as
      four concurrent loops at load 13-25. Both occurrences are Linux CI.
      A lead not yet checked: the autovacuum launcher builds a full
      `PgHandler` (`autovacuum::setting`, `worker`) every second for the
      server's whole life, a second session reading the catalog beside
      every connection, so a fix should prove the catalog caches cannot
      take that session's view as the connection's.
      Backlog-ID: RP-499
      Verify: none -- seen twice in Linux CI (2026-11-07, 2026-11-09), 600 + ~2,000 local runs never reproduced it

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

## Found while working RP-465 (2026-11-07)

Measured on a private PostgreSQL 15.19 cluster.

## Found while working RP-501 (2026-11-07)

Measured on PostgreSQL 15.19 (the data-modifying WITH on a private cluster).

- [ ] **What `transformWithClause` checks of a WITH is not all checked.**
      Since RP-501 an unread item is planned without being run, so its
      names and types are checked, but not: an unread item's literal input
      (`with w as (select 'x'::int) select 1` is 22P02 at 19 there, `1`
      here); an item of a WITH inside a subquery expression (`select (with
      w as (select nosuch) select 1)`, 42703 at 27 there, `1` here); the
      forward reference's HINT `Use WITH RECURSIVE, or re-order the WITH
      items to remove forward references.`; an alias list longer than the
      item's columns (`with w(x, y) as (select 1)`, 42P10 `WITH query "w"
      has 1 columns available but 2 columns specified` at 6 there; here
      42601 `table "w" ...` with no position when read, and nothing when
      not); and a name given twice (42712 `WITH query name "w" specified
      more than once` at the second, 25; here the first wins).
      Verify: tests/backlog/test_rp_fromless_analysis.py::test_with_clause_checks
      Backlog-ID: RP-504

## Found while working RP-486 (2026-11-07)

- [ ] **A syntax error in a DO block the interpreter compiles carries no
      position.** A body with a DECLARE section or a label is compiled by
      the function interpreter, and its 42601 reaches the client with no
      statement position: `do $$ declare x int; begin raise notice 'z' end
      $$` is `syntax error at or near "end"` at 45 on PostgreSQL 15.19,
      `do $$ <<blk>> begin raise notice 'z' end $$` the same at 38, and
      `do $$ <<blk>> begin raise notice 'z'; end nope $$` `end label "nope"
      differs from block's label "blk"` at 43. Here each message matches
      and no position is sent. (The fast path for a plain `begin ... end`
      body already points at the token.)
      Verify: tests/backlog/test_rp_do_compile_errors.py::test_compile_error_has_a_position
      Backlog-ID: RP-492

## Found while working RP-463 (2026-11-07)

- [ ] **An expression's error in a PL/pgSQL statement carries no QUERY and
      no internal position.** `do $$ declare x int; begin raise notice '%',
      nosuch.x; end $$` is 42P01 on both, but PostgreSQL 15.19 sends
      `internal_query` `nosuch.x` and `internal_position` 1 (psql prints
      `LINE 1: nosuch.x` and `QUERY:`); here neither field is sent. Called
      as a function (`return nosuch.x` in `lbe()`), the error here instead
      carries a statement position, 8, into `select lbe()`, where
      PostgreSQL sends none. Measured on a private 15.19 cluster.
      Verify: tests/backlog/test_rp_plpgsql_labels.py::test_expression_error_carries_its_query
      Backlog-ID: RP-488

## Found while working RP-483 (2026-11-07)

- [ ] **A call in a schema that does not exist is reported as a missing
      function.** On PostgreSQL 15.19 `select nosuch_schema.nosuch()` is
      3F000 `schema "nosuch_schema" does not exist` at 8
      (`LookupExplicitNamespace`, before any function is looked up), and
      `select pg_temp.nosuch()` the same for `pg_temp` in a session that has
      made no temporary object yet. Here each is 42883 `function nosuch()
      does not exist` (the schema dropped from the name, at the parenthesis,
      22) or `function pg_temp.nosuch() does not exist`, with the
      no-matching-function HINT. Once the session has a temp table both
      servers answer 42883 for `pg_temp`. Measured on a private 15.19
      cluster.
      Verify: tests/backlog/test_rp_function_bodies.py::test_call_in_a_missing_schema
      Backlog-ID: RP-485

## Found while working RP-472 (2026-11-07)

- [ ] **EXPLAIN of a user function in FROM prints no Function Scan.**
      PostgreSQL 15.19 prints `Function Scan on fq` for `explain (costs
      off) select * from fq()`, a scalar or a SETOF SQL function alike.
      Here each prints `Subquery Scan on fq` over `Values Scan on
      "*VALUES*"` (a scalar one printed `Result` until RP-474 made it a
      row source): the call is answered at plan time and its rows planned
      as a VALUES list. A built-in
      (`generate_series`) already prints its Function Scan.
      Backlog-ID: RP-475
      Verify: tests/backlog/test_rp_from_function_scan.py::test_function_scan_plan

## Found while working RP-454 (2026-11-07)

- [ ] **A FROM item named as a table an aliased JOIN hides is refused.**
      An aliased JOIN hides its sides' names, so PostgreSQL 15.19 lets
      another FROM item reuse one: `select * from (fs join fe using (a)) as
      j, fs` and `select * from fs, (fs join fe using (a)) as j` each answer
      their row. Here each is 0A000 `a FROM item named "fs" beside an
      aliased JOIN that hides that name is not supported yet`: a joined row
      is keyed by its table's alias (`alias<U+001F>column`), so the two `fs`
      leaves would share their keys. Fixing it needs leaf keys that are not
      the alias the query wrote.
      Backlog-ID: RP-469
      Verify: tests/backlog/test_rp_subquery_scope.py::test_hidden_join_name_reused
- [ ] **EXPLAIN of an aliased JOIN prints no Hash Cond and keeps its WHERE
      above the join.** PostgreSQL flattens a join alias's columns to the
      sides' own (`flatten_join_alias_vars`), so `explain (costs off)
      select j.c from (fs join fe on fs.a = fe.a) as j where j.b = 'x'`
      prints the unaliased query's plan: `Hash Cond: (fs.a = fe.a)` and
      `Filter: (b = 'x'::text)` on `fe`'s scan. Here neither line is
      printed: the WHERE pushdown and the equivalence classes
      (`pushdown_filters`) stop at an aliased JOIN, as a conjunct naming
      `j` has no leaf to go to. The rows are right; the plan text is not.
      Backlog-ID: RP-471
      Verify: tests/backlog/test_rp_subquery_scope.py::test_aliased_join_plan

## Found while working RP-470 (2026-11-07)

- [ ] **A missing USING column's 42703 carries a position.** PostgreSQL
      15.19 reports `column "a" specified in USING clause does not exist in
      left table` with no position; here it points into the statement (41,
      33 in the gap test's two statements).
      Verify: tests/backlog/test_rp_subquery_scope.py::test_using_column_missing_has_no_position
      Backlog-ID: RP-481

## Found while working RP-479 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `fs(a int, c text)`
holding `(1, 'y')`.


## Found while working RP-508 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster. The gap tests are in
`tests/backlog/test_rp_found_rp508.py`.

- [ ] **`pg_get_function_arguments` and `pg_get_function_identity_arguments`
      of a procedure print no argument modes.** PostgreSQL 15.19's
      `print_function_arguments` prints every mode of a procedure's
      arguments, IN included: `create procedure gk(a int, inout b text)`
      prints `IN a integer, INOUT b text` in both; here `a integer, b text`.
      (A function's IN is left out on both servers.)
      Verify: tests/backlog/test_rp_found_rp508.py::test_procedure_arguments_print_their_modes
      Backlog-ID: RP-515

## Found while working RP-467 (2026-11-08)

- [ ] **Two schemas' tables of one name are not `checkNameSpaceConflicts`'
      exception.** PostgreSQL lets two unaliased FROM items share a name
      when they are different relations (`select 1 from s1.t join s2.t on
      true` runs, and only a `t.a` naming both is ambiguous), and refuses an
      alias equal to another item's name (42712 `table name "t" specified
      more than once`). Here the join of two unaliased `t` is that 42712,
      since a joined row is keyed by the bare name, and `create view v as
      select t.a from s1.t t join s2.t on s2.t.a = t.a` is accepted, its
      definition then reading `t.a = t.a`. Measured on a private 15.19
      cluster.
      Backlog-ID: RP-526
      Verify: tests/backlog/test_rp_found_rp467.py::test_same_name_relations_in_from

## Found while working RP-528 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster. The gap test is in
`tests/backlog/test_rp_found_rp528.py`.

- [ ] **A DROP naming a schema that does not exist reports the relation
      missing.** PostgreSQL 15.19's `DropErrorMsgNonExistent` asks for the
      schema first: `drop table nosch.t` (and `view`, `materialized view`,
      `index`, `sequence`) is 3F000 `schema "nosch" does not exist`, and
      under IF EXISTS the NOTICE is `schema "nosch" does not exist,
      skipping`. Here each is the relation's own 42P01 / 42704 (`table
      "nosch.t" does not exist`) or its skipping NOTICE. The planner keys a
      qualified name in an unknown schema as `nosch.t`, which `split_key`
      cannot tell from a quoted `"nosch.t"` in `public` (PostgreSQL reports
      that one as a missing table): the fix needs the written schema from
      the parse tree.
      Verify: tests/backlog/test_rp_found_rp528.py::test_drop_in_a_missing_schema
      Backlog-ID: RP-533

## Found while working RP-514 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster. The gap tests are in
`tests/backlog/test_rp_found_rp514.py`.

- [ ] **A built-in routine is not looked up among its overloads or its
      kind.** PostgreSQL 15.19 looks a built-in up in `pg_proc` like any
      routine: `grant execute on function lower to public` and `comment on
      function lower is 'x'` are 42725 `function name "lower" is not unique`
      (`comment on routine abs` says `routine name`), and `grant execute on
      procedure abs(int)` and `comment on procedure abs(int)` are 42809
      `abs(integer) is not a procedure`. Here the bare-name GRANT succeeds,
      the PROCEDURE forms are 42883 `procedure abs(integer) does not exist`,
      and every COMMENT ON a built-in routine is refused as 0A000 (PostgreSQL
      records `comment on function abs(int)`). `function_acl_key`
      (object_acl.rs) knows a built-in only by `is_known_function(name)`:
      the fix needs the built-in's overloads, and a place to keep a
      built-in's comment.
      Verify: tests/backlog/test_rp_found_rp514.py::test_builtin_routine_resolved_by_its_overloads
      Backlog-ID: RP-519
- [ ] **GRANT's 42883 for a signature no routine has carries a position.**
      PostgreSQL 15.19's `LookupFuncWithArgs` raises `function gc(boolean)
      does not exist` (and `procedure ...` for PROCEDURE) with no position;
      here it carries one, at the routine's name (27, 28). COMMENT's same
      error has none.
      Verify: tests/backlog/test_rp_found_rp514.py::test_grant_missing_signature_has_no_position
      Backlog-ID: RP-520

## Found while working RP-516 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `fe(a int, b text)`
and `fs(a int, c text)`.

- [ ] **A write's name error is reported away from the reference.**
      PostgreSQL 15.19 positions each at the name as written. Here `update
      fs x set c = fs::text` and `... where fs is null` are 42703 at 8 (the
      target), not 21 / 31; `update fs set c = 'q' from fe e where e.a =
      fs.a returning fe` is 42703 at 28 (the FROM item), not 60; and an
      ambiguous bare column in an UPDATE ... FROM's or DELETE ... USING's
      RETURNING (`returning a`) is 42702 with no position, not 59 / 53.
      RP-414 has the reference's name and qualifier; this is where it is.
      Backlog-ID: RP-524
      Verify: tests/backlog/test_rp_subquery_scope.py::test_write_name_error_position

## Found while working RP-546 (2026-11-08)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster. The gap
tests are in `tests/backlog/test_rp_found_rp546.py`.

- [ ] **DROP TABLE names a table's dependants by kind, not in creation
      order.** PostgreSQL 15.19's `findDependentObjects` reads `pg_depend`
      in creation order, so a view made before the table whose foreign key
      references it is named first (`view o560v depends on table o560a`,
      then the constraint), and a cast made between two functions over the
      row type falls between them. Here the foreign keys always come before
      the views (`drop table` and its CASCADE notice), and since RP-546 the
      casts follow every routine: a cast's oid is derived from its types, not
      drawn from the oid counter as `CastCreate` draws it, so its creation
      order is not kept. `DROP SCHEMA ... CASCADE` names a routine over a
      member's row type in the same order as here, not PostgreSQL's.
      Backlog-ID: RP-560
      Verify: tests/backlog/test_rp_found_rp546.py::test_drop_table_dependants_in_creation_order
- [ ] **An operator over a table's row type does not depend on it.**
      `create operator ==== (leftarg = p561, rightarg = p561, ...)` records
      a dependency on its argument types, so `drop table p561` names
      `operator ====(p561,p561) depends on type p561` and CASCADE drops it.
      Here the DETAIL names only the function, and the operator outlives the
      table in `pg_operator`.
      Backlog-ID: RP-561
      Verify: tests/backlog/test_rp_found_rp546.py::test_operator_over_row_type_depends_on_its_table
- [ ] **DROP TYPE of a table's row type with a routine over it names the
      routines.** PostgreSQL 15.19 answers `drop type r562` with 2BP01
      `cannot drop type r562 because table r562 requires it` whatever else
      names the type; here a routine over it makes the answer that routine's
      `other objects depend on it`. With no routine it already matches.
      Backlog-ID: RP-562
      Verify: tests/backlog/test_rp_found_rp546.py::test_drop_type_of_a_row_type_with_a_routine
- [ ] **DROP TYPE names a routine with a blank after each comma.**
      `getObjectDescription` writes `format_procedure`'s
      `function k563(integer,ty563)`; DROP TYPE's DETAIL and CASCADE notice
      here say `k563(integer, ty563)` (`function_signature`, which DROP
      FUNCTION's dependants share). DROP TABLE names them as PostgreSQL does
      since RP-546 (`row_type_depends::routine_description`).
      Backlog-ID: RP-563
      Verify: tests/backlog/test_rp_found_rp546.py::test_drop_type_names_a_routine_as_format_procedure
- [ ] **A DROP TABLE of several tables is refused under the first one's
      name.** PostgreSQL 15.19's `reportDependentObjects` says `cannot drop
      desired object(s) because other objects depend on them` when the DROP
      names more than one object; here `drop table m564a, m564b` is `cannot
      drop table m564a because ...`, each table being dropped on its own.
      Backlog-ID: RP-564
      Verify: tests/backlog/test_rp_found_rp546.py::test_multi_table_drop_refusal_headline
- [ ] **A routine over a temporary table's row type names no type.** On
      PostgreSQL 15.19 `create function tf565(tt565)` over a temp table has
      `proargtypes` `{tt565}`, and `drop table tt565` is 2BP01 naming it.
      Here the argument is stored by its bare name, which resolves to no
      type (`-`), the DROP succeeds, and the routine is left behind.
      Backlog-ID: RP-565
      Verify: tests/backlog/test_rp_found_rp546.py::test_routine_over_a_temp_table_row_type
- [ ] **A `regprocedure` prints its name unquoted and a schema's type
      quoted whole.** `regprocedureout` is `format_procedure`: `"Q
      f567"(s567.t)` and `s567.h(s567.t)` on PostgreSQL 15.19. Here they are
      `Q f567("s567.t")` and `s567.h("s567.t")` (`regobj::text`), and
      `pg_describe_object` of a routine shares that text.
      Backlog-ID: RP-567
      Verify: tests/backlog/test_rp_found_rp546.py::test_regprocedure_quotes_as_format_procedure

## Found while working RP-527 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster. The gap tests are in
`tests/backlog/test_rp_found_rp527.py`.

- [ ] **A FROM relation that does not exist is reported after the
      statement's qualified column references.** PostgreSQL 15.19's
      `transformFromClause` runs before the select list and WHERE are read,
      so `select fs.c from nosuch` is 42P01 `relation "nosuch" does not
      exist` at 18, `select nosuch.c from nosuch` the same at 22 and
      `select a from nosuch where zz.a = 1` at 15. Here the qualifier check
      runs first: the first and third are `missing FROM-clause entry for
      table "fs"` / `"zz"` (at 8, 28), and the second has PostgreSQL's
      message at the reference (8).
      Backlog-ID: RP-545
      Verify: tests/backlog/test_rp_found_rp527.py::test_missing_from_relation_reported_first

## Found while working RP-530 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `fs(a int, c text)`
holding `(1, 'y')`, `create type ct as (x int)` and `create type ct2 as (x
int)`. The gap tests are in `tests/backlog/test_rp_found_rp530.py`.

- [ ] **An untyped literal beside a row is not read as `record`.** `select
      fs = 'x' from fs`, `'x' = fs` and `(1,2) = 'x'` are 0A000 `input of
      anonymous composite types is not implemented` at the literal (13, 8,
      16) on PostgreSQL 15.19: the operator resolves to `record = record`
      and `record_in` refuses the literal. Here each is 0A000 `comparing
      document with string using = is not supported yet`, with no position.
      Verify: tests/backlog/test_rp_found_rp530.py::test_untyped_literal_beside_a_row
      Backlog-ID: RP-591
- [ ] **A row beside an `ARRAY[...]` names the row `text`.** `select
      array[1] = (1,2)`, `fs = array[1]` and `array[1] = fs` are 42883
      `operator does not exist: integer[] = record` (`fs = integer[]`,
      `integer[] = fs`) on PostgreSQL 15.19; here they name the array
      `int4[]` and the row `text`. The SQLSTATE and position match.
      Verify: tests/backlog/test_rp_found_rp530.py::test_row_beside_an_array_constructor
      Backlog-ID: RP-592
- [ ] **An error a PREPARE's query raises carries the query's own offset.**
      PREPARE plans the deparsed inner query, so a recorded location is an
      offset into `SELECT ...`, honoured whenever a token of the PREPARE
      text happens to start there: `prepare p1(int) as select 12 = true`
      is 42883 at 30 on PostgreSQL 15.19 and at 11 here (the `(` of
      `p1(int)`). An offset that lands on no token falls back to the text
      search, which is why most look right.
      Verify: tests/backlog/test_rp_found_rp530.py::test_prepare_error_position
      Backlog-ID: RP-593

## Found while working RP-532 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `fs(a int, c text)`
holding `(1, 'y')` twice, `(2, 'z')` and `(null, null)`, and `fk(k fs)`
holding `row(null, null)`, `row(1, null)`, `row(1, 'a')` and `null`. The
gap tests are in `tests/backlog/test_rp_found_rp532.py`.

- [ ] **An ungrouped whole row's 42803 names its first column, with no
      position.** PostgreSQL 15.19's `check_ungrouped_columns` names a
      whole-row Var `fs.*` (`get_rte_attribute_name` of attribute 0), at
      the reference: `select fs, count(*) from fs group by a` is `column
      "fs.*" must appear in the GROUP BY clause ...` at 8, and `select
      count(*) from fs having fs is not null` the same at 32. Here they
      name `fs.c` and `fs.a`, with no position.
      Backlog-ID: RP-536
      Verify: tests/backlog/test_rp_found_rp532.py::test_ungrouped_whole_row
- [ ] **A relation name its alias hides is reported at the FROM item.**
      Under `from fs x` the name `fs` names nothing, so PostgreSQL 15.19
      reports `select count(*) from fs x group by fs` and `... order by fs`
      as 42703 `column "fs" does not exist` at 36, the reference. Here the
      first is positioned at 22 (the FROM item), and the second is 42803
      `column "x.fs" must appear in the GROUP BY clause ...` at 22.
      Backlog-ID: RP-537
      Verify: tests/backlog/test_rp_found_rp532.py::test_alias_hides_relation_name

## Found while working RP-557 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `create domain d557 as
char(3)` and `t557(c char(3), a d557)`. Each was wrong before RP-557's
change too. The gap tests are in `tests/backlog/test_rp_found_rp557.py`.

- [ ] **`min` / `max` over a `char(n)` column describe its width.** An
      Aggref carries no modifier (`exprTypmod` has no case for it), so
      `select min(c) from t557` and `select max(a) from t557` describe
      `bpchar` -1 on PostgreSQL 15.19; here both are 7
      (`aggregate_result_typmod` copies the column's). Since RP-671
      `aggregate_output_def` carries the width too, as a FROM-subquery's
      column, because it is what pads the trimmed value again: a fix must
      keep the padding width and describe -1.
      Verify: tests/backlog/test_rp_found_rp557.py::test_min_max_over_char_n_has_no_modifier
      Backlog-ID: RP-659
- [ ] **A set operation of a domain column and its base type keeps the
      width.** `select a from t557 union select c from t557` (either order)
      describes -1 on PostgreSQL 15.19: `select_common_type` smashes the
      domain to `bpchar` and `select_common_typmod` finds the sides'
      modifiers differ, the domain column's own being -1. Here it is 7.
      Two sides of one type keep 7 in both.
      Verify: tests/backlog/test_rp_found_rp557.py::test_set_op_of_a_domain_and_its_base_type
      Backlog-ID: RP-660

## Found while working RP-590 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp590.py`.

- [ ] **The `record_image` operators are refused.** `*=`, `*<>`, `*<`,
      `*<=`, `*>` and `*>=` over two record values are `record_image_eq`
      .. `record_image_ge` on PostgreSQL 15.19, which compare the fields'
      binary images: `(select row(1.0)) *= (select row(1.00))` is `f` where
      `=` is `t`, and `row(1, 'a')::ca *= row(1, 'a')::cd` is `t`. Between
      two `ROW(...)` constructors the operator is applied field by field
      (`make_row_comparison_op`), so `row(1) *= row(1)` is 42883 `operator
      does not exist: integer *= integer` at 15. Here each is 0A000
      `operator *= on these operands is not supported yet`.
      Verify: tests/backlog/test_rp_found_rp590.py::test_record_image_operators
      Backlog-ID: RP-650

## Found while working RP-569 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp569.py`: `d569nv` over `int` with a CHECK
`value > 0` added `NOT VALID` after `t569nv(a d569nv)` took `-1` and `5`;
`t569c(c char(3))` holding `'xy'`; `d569c` over `char(3)` and `t569d(a
d569c, i int)` holding `('a b', 1)` and `('xy', 2)`.


## Found while working RP-657 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `create domain d657c as
char(3)`, `t657d(a d657c, i int)` holding `('a b', 1)` and `('xy', 2)`, and
`t657e(a d657c, i int)` holding `('e', 1)`. The gap tests are in
`tests/backlog/test_rp_found_rp657.py`. Since RP-657 a domain column keeps
its domain through a join, a WITH, a FROM subquery and a set operation all
of whose branches are of the domain (`domain_scope.rs`).

- [ ] **`ARRAY(select * ...)` over a one-column domain subquery is typed by
      the domain's base.** `select array(select * from (select a from
      t657d) s order by 1)` is `d657c[]` on PostgreSQL 15.19, as the same
      query with `select a` is; here it is `character[]`. A `*` target is
      not a column reference the cast can wrap
      (`domain_scope::cast_first_columns`).
      Verify: tests/backlog/test_rp_found_rp657.py::test_array_of_star_over_a_domain_subquery_is_the_domains
      Backlog-ID: RP-673
- [ ] **A LATERAL subquery's column that is an outer domain column is typed
      by the domain's base.** `select pg_typeof(b), array[b] from t657d x
      cross join lateral (select x.a b) l` is `d657c` and `d657c[]`, with
      the padding, on PostgreSQL 15.19; here `character` and `character[]`,
      without it. The subquery's scope does not see the outer FROM item it
      references, so its column's type is not known.
      Verify: tests/backlog/test_rp_found_rp657.py::test_lateral_column_of_an_outer_domain_column_is_the_domains
      Backlog-ID: RP-674

## Found while working RP-672 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp672.py`: `t672c(a char(3))` holding `'xy'`.

- [ ] **`CREATE TABLE AS` of an unsized `bpchar` drops its blanks.** `create
      table c as select 'xy '::bpchar a` makes a `bpchar` column with no
      modifier on PostgreSQL 15.19, and `octet_length(a)` is 3; so is a
      value over a `char(3)` column UNIONed with `text` inserted into it.
      Here the column holds `xy`, 2 bytes: a `bpchar` column is stored
      trimmed and padded on the way out to its width, and one with no
      width pads nothing. The same holds for any unsized `bpchar` column
      (`create table u (a bpchar)`: `'ab  '` reads back `ab`), and for a
      `max`, a GROUP BY or a DISTINCT over one. Sized while working RP-681
      (2026-11-09): comparisons, `IN`, ranges and joins already agree with
      PostgreSQL, but keeping the blanks breaks the keys -- a UNIQUE
      constraint and a primary key are storage indexes over the stored
      value (the PK is `_id`), so `'k'` and `'k '` would stop colliding
      (both are 23505 on 15.19 today, rightly) -- and GROUP BY, DISTINCT
      and an indexed `=` would separate them too. The faithful fix keeps
      the value as written and keys it by `bpchar`'s comparison (trailing
      blanks ignored), which reaches the storage keys: more than one PR.
      Verify: tests/backlog/test_rp_found_rp672.py::test_create_table_as_of_an_unsized_bpchar_keeps_its_blanks
      Backlog-ID: RP-693
- [ ] **INTERSECT keeps the left side's value, not the smaller input's.**
      PostgreSQL 15.19 plans an INTERSECT with the input it estimates
      smaller first (`generate_nonunion_paths`), and a group's value is the
      one that input gave: `select a from t672c intersect select
      'xy'::text` is `xy`, unpadded, as the one-row constant is estimated
      smaller than the unanalysed table (1850 rows). Here the left side's
      row is always the one returned, `xy ` with its blank. Over equal
      estimates (two VALUES lists) the left side comes first, as here.
      Verify: tests/backlog/test_rp_found_rp672.py::test_intersect_keeps_the_smaller_inputs_value
      Backlog-ID: RP-692

## Found while working RP-665 (2026-11-10)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp665.py`: `t665(a d665, c dc665, b int)`
over `create domain d665 as int` and `create domain dc665 as char(4)`.

- [ ] **A CASE, COALESCE, GREATEST or LEAST over domain columns alone is
      typed by the domain's base.** `select pg_typeof(coalesce(a, a)) from
      t665` is `d665` on PostgreSQL 15.19, and so are `case when ... then a
      else a end`, `greatest(a, a)` and `least(a, a)`; `coalesce(c, c)` is
      `dc665` (`select_common_type` keeps a type every input shares
      exactly, a domain included). Here each is `integer` (`character`).
      Since RP-665 the common type keeps a domain every input is OF -- a
      scalar subquery over one is -- but a bare domain column reaches a
      `pg_typeof` as its domain only directly (`rewrite_domain_typeof`),
      so inside these it is still of its stored base type.
      Verify: tests/backlog/test_rp_found_rp665.py::test_common_type_of_domain_columns_alone_is_the_domain
      Backlog-ID: RP-764

## Found while working RP-656 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp656.py`: `t656(c char(3), v varchar(4), n
numeric(5,2), ca char(3)[])` holding `('xy', 'ab', 2.5, '{a,bc}')`.

- [ ] **An `ARRAY(select n ...)` over a `numeric(p,s)` column describes no
      modifier.** `select array(select n from t656)` describes `numeric[]`
      327686 on PostgreSQL 15.19 (`exprTypmod` of an ARRAY sublink is its
      column's); here it is -1. Since RP-656 a `char(n)` or `varchar(n)`
      column's array is cast at its modifier and describes it; a numeric
      one is declared as `numeric[]`, which carries none.
      Verify: tests/backlog/test_rp_found_rp656.py::test_array_sublink_of_numeric_describes_its_modifier
      Backlog-ID: RP-667

## Found while working RP-671 (2026-11-08)

Measured on PostgreSQL 15.19 (the shared reference on port 5415): the
VALUES-only statements as written, the rest with the `_SETUP` tables of
`tests/backlog/test_rp_found_rp671.py` made temporary. This server keeps a
`char(n)` value trimmed and pads it where it enters an expression; a value
that reaches a comparison carrying its blanks is compared with them.

- [ ] **OVER on a plain function is 0A000.** `select octet_length('x')
      over ()` is 42809 `OVER specified, but octet_length is not a window
      function nor an aggregate function` on PostgreSQL 15.19 (`ParseFuncOrColumn`);
      here it is 0A000 `window function octet_length() is not supported
      yet`, for `upper` and `length` alike: a function PostgreSQL itself
      refuses is reported as a gap in this server.
      Verify: tests/backlog/test_rp_found_rp671.py::test_over_on_a_plain_function_is_42809
      Backlog-ID: RP-682

## Found while working RP-670 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp670.py`: `t687(a d687, v varchar(3), w
dv687, c char(3))`, `d687` an `int` with `check (value > 0)` and `dv687` a
`varchar(3)`.

- [ ] **OPEN -- `stop()` with an idle connection open took 3.8 s once
      under load.** `crates/secantus-pg/tests/embedded.rs::stop_does_not_wait_on_an_idle_connection`
      failed in a local `scripts/gate_pg.sh --fast` run on 2026-11-09
      (`stop() waited 3.826650542s on an idle connection`, against its 3 s
      budget) while the machine's load average was 21 on 12 cores, two
      gates running, and the `embedded` tests running four at a time. The
      same commit passed the step before the rebase that preceded it, and
      the test alone passed five times running at load 17-20 (2.0-2.6 s
      for the whole test, reopen included). Not diagnosed: whether the time
      is the shutdown watch telling the connection to finish, the drain, or
      the closing checkpoint under disk contention; the next step is the
      three phases' durations from a slow run. Seen a second time on
      2026-11-10 (RP-831's `--fast` gate, again the run after a rebase):
      `4.21637575s`, with the load average at 15-17 and a second lane's
      gate running; the test alone then passed six times running at load
      8-10.
      Backlog-ID: RP-690
      Verify: none -- seen once, under load; a timing that does not fail on demand

## Found while working RP-687 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp687.py`: `dcc687` a `varchar(3)` with
`check (value <> 'no')`, `dvv687` a domain over it.

- [ ] **A cast to a domain over a domain names the inner domain.** `select
      'no'::dvv687` is 23514 `value for domain dvv687 violates check
      constraint "dcc687_check"` on PostgreSQL 15.19 (one `CoerceToDomain`
      names the domain it coerces to); here the message names `dcc687`,
      for `'no'::text::dvv687` and a PL/pgSQL `x dvv687 := 'no'` alike.
      Since RP-670 a trigger record's field of such a domain names it.
      Verify: tests/backlog/test_rp_found_rp687.py::test_cast_to_a_domain_over_a_domain_names_the_outer_domain
      Backlog-ID: RP-696

## Found while working RP-694 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp694.py`: `dnn694` an `int` domain `not null`.

- [ ] **A `FOR` over a query or an `EXECUTE ... INTO` refuses scalar
      variable targets.** `for x in select '7'::text loop` over an `int`
      `x`, `for x, y in select 1, 'a'` and `execute 'select 7' into x` each
      assign on PostgreSQL 15.19 (the grammar makes the list a row target
      that `exec_move_row` fills); here each is 0A000 `this FOR target` /
      `this EXECUTE INTO target ... is not supported yet`. Both read only
      the target's `dno`, which a scalar list has none of (a plain `INTO`
      goes through `target_datum`).
      Verify: tests/backlog/test_rp_found_rp694.py::test_for_and_execute_into_scalar_variables
      Backlog-ID: RP-713

## Found while working RP-712 (2026-11-10)

Measured on PostgreSQL 15.19 (port 5415) with statements that create
nothing. The gap tests are in `tests/backlog/test_rp_found_rp712.py`.

- [ ] **A `numeric` out of `int4`'s range quotes its value.** `select
      1e20::numeric::int` is 22003 `integer out of range` on PostgreSQL
      15.19 (`numeric_int4_opt_error`); here the message is `integer out of
      range: "100000000000000000000"`, in plain SQL and in a PL/pgSQL
      assignment or `INTO` alike.
      Verify: tests/backlog/test_rp_found_rp712.py::test_numeric_out_of_int4_range_quotes_no_value
      Backlog-ID: RP-749
- [ ] **A `jsonb` input error carries no `JSON data` CONTEXT.** `select
      't'::jsonb` is 22P02 `invalid input syntax for type json` with
      CONTEXT `JSON data, line 1: t` on PostgreSQL 15.19 (`json_errcontext`);
      here the CONTEXT is absent, and in a PL/pgSQL assignment (`x jsonb :=
      true`) the line is missing from the stack.
      Verify: tests/backlog/test_rp_found_rp712.py::test_jsonb_input_error_carries_its_json_data_context
      Backlog-ID: RP-750
- [ ] **An assignment's overlong `varchar(n)` value is 22001 with no
      assignment frame.** `declare x varchar(3); begin x := 'abcdef'::text;`
      carries CONTEXT `PL/pgSQL assignment "x := 'abcdef'::text"` above the
      `at assignment` line on PostgreSQL 15.19: the length coercion is part
      of the assignment's query and folds as it is planned. Here only the
      `at assignment` line is sent: `fit_assigned` raises outside the
      framed query. The same holds for an element of a `varchar(3)[]`
      (`x[1] := 'abcdef'`, framed `PL/pgSQL assignment "x[1] :=
      'abcdef'"` on 15.19) and for a slice (`x[1:1] := array['abcd']`).
      Verify: tests/backlog/test_rp_found_rp712.py::test_overlong_varchar_assignment_is_framed_by_the_assignment
      Backlog-ID: RP-752
- [ ] **An assignment's literal input error carries no internal query.**
      `declare x int; begin x := 'abc';` is 22P02 with `internal_query` `x
      := 'abc'` on PostgreSQL 15.19 (the literal is coerced as the
      assignment is parsed); here no internal query is sent. The same class
      as RP-488, in a shape its test does not reach.
      Verify: tests/backlog/test_rp_found_rp712.py::test_literal_input_error_in_an_assignment_carries_its_query
      Backlog-ID: RP-753

## Found while working RP-668 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SCOPE_SETUP` in
`tests/backlog/test_rp_found_rp664.py`.

- [ ] **RETURNING a FROM item's column is refused when the join condition
      reads a column the UPDATE assigns.** `update t668 set b = u668.v
      from u668 where t668.b < 0 and u668.k = 5 returning t668.a, u668.v`
      returns the joined row's `v` for each updated row on PostgreSQL
      15.19 (the join is done once, before the rows are written); here it
      is 0A000 `RETURNING a FROM item's column when the join condition
      reads an updated column is not supported yet`, whether the FROM item
      is a table or a subquery over the target. `rewrite_dml_from` finds
      the joining row again for RETURNING with a correlated subquery over
      the NEW row, so it refuses the case where the new row no longer
      joins.
      Verify: tests/backlog/test_rp_found_rp664.py::test_returning_a_from_item_when_the_join_reads_an_assigned_column
      Backlog-ID: RP-686

## Found while working RP-570 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `create domain d570c as
char(3)` and `t570(k int, t text, ta text[])` holding `(1, 'ab', '{ab}')`
and `(2, 'abcd', '{abcd}')`. The gap tests are in
`tests/backlog/test_rp_found_rp570.py`.

- [ ] **A domain array literal's input error has no position.** `select
      '{abcd}'::d570c[]` is 22001 at 8 on PostgreSQL 15.19 (`array_in` runs
      `domain_in` while it reads the literal, as `'x'::int` is positioned
      there); here the 22001 has no position. `positioned` places an error
      by its SQLSTATE and message, and an assignment's 22001 has none.
      Verify: tests/backlog/test_rp_found_rp570.py::test_domain_array_literal_error_position
      Backlog-ID: RP-581

## Found while working RP-607 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `create domain d607q
as int`. The gap tests are in `tests/backlog/test_rp_found_rp607.py`.

- [ ] **A domain over another domain's array type is refused.** `create
      domain d607s as d607q[]` is made on PostgreSQL 15.19, with
      `typbasetype` `d607q[]`, and `'{1,2}'::d607s::text` is `{1,2}`; here
      the CREATE is 42704 `type "d607q[]" does not exist`: `create_domain`
      knows a base by a built-in's name or a user type's, never a user
      type's array. The DROP side follows such a domain already (RP-607).
      Verify: tests/backlog/test_rp_found_rp607.py::test_domain_over_a_domain_array
      Backlog-ID: RP-654

## Found while working RP-658 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster. The gap tests are in
`tests/backlog/test_rp_found_rp658.py`.

- [ ] **A composite type's attribute has `attndims` 0.** `create type c as
      (p int[][], q int, r text[])` gives `p`, `q` and `r` 2, 0 and 1 in
      PostgreSQL 15.19's `pg_attribute` (`DefineCompositeType` builds the
      descriptor with `BuildDescForRelation`, which counts the written
      bounds); here each is 0: the composite's stored fields keep only a
      type name. A table column records them since RP-658.
      Verify: tests/backlog/test_rp_found_rp658.py::test_composite_attribute_attndims
      Backlog-ID: RP-663

## Found while working RP-575 (2026-11-08)

Measured on PostgreSQL 15.19; none of it needs DDL. The gap tests are in
`tests/backlog/test_rp_found_rp575.py`.

- [ ] **The array built-ins refuse an `int2vector` or `oidvector`.** Both
      are true array types to PostgreSQL 15.19's polymorphic resolution
      (`typsubscript` is `array_subscript_handler`, `typelem` int2 / oid),
      subscripted from 0: `cardinality('1 2'::int2vector)` is 2,
      `array_dims('1 2'::int2vector)` `[0:1]`, `array_to_string('1
      2'::oidvector, ',')` `1,2`, `array_dims(indkey)` / `array_upper(indkey,
      1)` of a one-column index `[0:0]` / 0, and `cardinality(array_agg(v))`
      over vectors 2. Here each is 42883 `function ... does not exist`:
      `funcsig::accepts` takes only a `[]` type for `anyarray`. Accepting
      the vectors there is not enough: a vector reaches the built-ins with
      no lower bound of its own (`[1:2]`, and `array_position` off by one),
      and over a column through an evaluator that does not see its type.
      `'1 2'::int2vector || '3'::int2vector` is `[0:2]={1,2,3}` on
      PostgreSQL and `{1,2,3}` here, for the same reason.
      Verify: tests/backlog/test_rp_found_rp575.py::test_array_builtins_over_a_vector
      Backlog-ID: RP-601
- [ ] **An `int2vector` of two dimensions prints as text.** `array_agg`
      over vectors is `array_agg(anyarray)`, typed `int2vector` and holding
      two dimensions, which `int2vectorout` refuses (`check_valid_int2vector`):
      on PostgreSQL 15.19 `array_agg(v)::text` and `concat(array_agg(v))`
      are 42804 `array is not a valid int2vector`. Here they are `{1,2}`.
      The value sent as a column already is 42804 in text (RP-575); a cast
      or `concat` prints it through `typeout`, which cannot fail.
      Verify: tests/backlog/test_rp_found_rp575.py::test_two_dimensional_vector_text
      Backlog-ID: RP-602
- [ ] **A parameter declared `int2vector[]` or `oidvector[]` has no type.**
      A Bind of `{"1 2",3}` with type oid 1006 is `int2vector[]` to
      `pg_typeof($1)` on PostgreSQL 15.19, `($1)[2]::text` is `3` and
      `cardinality($1)` 2, in text or binary format. Here the first is 42P18
      `could not determine data type of parameter $1` and the others 42804 /
      42883: oids 1006 and 1013 are in neither the parameter-type nor the
      array-element table (`element_of_array_oid`), so the value stays its
      raw text.
      Verify: tests/backlog/test_rp_found_rp575.py::test_vector_array_parameter
      Backlog-ID: RP-604
- [ ] **An `int2vector` input's out-of-range error quotes only its number.**
      `int2vectorin` reports the rest of the input from the number on:
      `'70000 1'::int2vector` and `'1 70000 1'::int2vector` are 22003
      `value "70000 1" is out of range for type smallint` on PostgreSQL
      15.19. Here the message quotes `"70000"`.
      Verify: tests/backlog/test_rp_found_rp575.py::test_int2vector_range_error_quotes_the_rest
      Backlog-ID: RP-605

## Found while working RP-603 (2026-11-08)

Measured on PostgreSQL 15.19; none of it needs DDL. The gap tests are in
`tests/backlog/test_rp_found_rp603.py`.

- [ ] **`oidvector`'s comparison functions are refused.**
      `btoidvectorcmp('1 2', '3')` is 1 on PostgreSQL 15.19, and
      `oidvectorlt` .. `oidvectorne` answer as their operators; here each is
      `function ... is not supported yet`. `cmpfn` has the B-tree support
      functions of the byte-ordered types only.
      Verify: tests/backlog/test_rp_found_rp603.py::test_oidvector_comparison_functions
      Backlog-ID: RP-612

## Found while working RP-529 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `create domain dc3 as
char(3)`, `create domain dc3n as dc3` and the rest of the `_SETUP` in
`tests/backlog/test_rp_found_rp529.py`.

- [ ] **A domain's CHECK failing inside a record literal carries no
      position.** `select '(0,x)'::ctp`, `ctp` a composite with a `pint`
      field, is 23514 at 8 on PostgreSQL 15.19, at the literal as its other
      input errors are (`'(abcd,1)'::fc` is 22001 at 8 here too); here the
      23514 has no position.
      Backlog-ID: RP-552
      Verify: tests/backlog/test_rp_found_rp529.py::test_record_literal_domain_error_position
- [ ] **A schema's domain reports its CHECK under the qualified name.**
      `select 200::s.sd` is 23514 `value for domain s.sd violates check
      constraint "sd_check"` on PostgreSQL 15.19; here the constraint is
      `"s.sd_check"`, though `pg_constraint.conname` is `sd_check` in both.
      Backlog-ID: RP-553
      Verify: tests/backlog/test_rp_found_rp529.py::test_schema_domain_check_name
- [ ] **`ALTER COLUMN TYPE` to a domain over the column's type is
      refused.** `alter table falt alter column a type pint`, `a int` and
      `pint` a domain over `int`, retypes the column on PostgreSQL 15.19
      (an implicit cast to the domain, no USING needed); here it is 42804
      `column "a" cannot be cast automatically to type pint`.
      Backlog-ID: RP-556
      Verify: tests/backlog/test_rp_found_rp529.py::test_alter_column_type_to_domain

## Found while working RP-534 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `fs(a int, c text)`,
`create domain dfs as fs` and `fd(k dfs)` holding `row(null, null)`,
`row(1, null)`, `row(1, 'a')` and `null`.

- [ ] **A domain over a table's row type is refused.** `create domain dfs
      as fs` over a table `fs` is 42704 `type "fs" does not exist` here;
      PostgreSQL 15.19 creates it (a domain may be over a composite since
      PostgreSQL 11), and a column of it holds rows: `select count(*) from
      fd where k is not null` is 1 and `... where k is null` 2, the null
      test reading the fields because `type_is_rowtype` is true of the
      domain. RP-534's per-row null test asks `type_is_rowtype` of the
      column's declared type, so it needs to see through the domain then.
      Backlog-ID: RP-538
      Verify: tests/backlog/test_rp_found_rp534.py::test_domain_over_a_table_row_type

## Found while working RP-500 (2026-11-07)

Measured on a private PostgreSQL 15.19 cluster with `fe(a int, b text)`.

- [ ] **A string constant an operator over a column or a parameter, an
      INSERT or UPDATE target, or COALESCE gives its type is not read at
      CREATE.** PostgreSQL 15.19 coerces each while it analyses a SQL
      function's body (`transformExpr`, `coerce_type`), so `create function
      gx(p int) returns int language sql as $$ select p + 'a' $$` is 22P02
      `invalid input syntax for type integer: "a"` at 69, and so are `select
      a + 'q' from fe` (69), `insert into fe values ('q')` (82), `update fe
      set a = 'q'` (77) and `select coalesce('q', 1)` (74). Here each CREATE
      succeeds. Since RP-500 a cast's constant is read at CREATE, and an
      operator's operand only where planning folds the comparison (`'a' =
      1`, `a = 'q'` in a WHERE): the rest needs each constant's resolved
      type from the analysis, which the planner does not keep. PREPARE has
      the same shape (RP-511).
      Backlog-ID: RP-507
      Verify: tests/backlog/test_rp_found_rp500.py::test_operand_constant_read_at_create
- [ ] **A malformed range literal's 22P02 has no DETAIL.** PostgreSQL
      15.19's `range_parse` explains each one: `select '[1,2'::int4range`
      has DETAIL `Unexpected end of input.`, `'[1,2]x'` `Junk after right
      parenthesis or bracket.`, `'1,2'` `Missing left parenthesis or
      bracket.`. Here the message and position match and the DETAIL is
      empty.
      Backlog-ID: RP-510
      Verify: tests/backlog/test_rp_found_rp500.py::test_range_literal_detail
- [ ] **PREPARE evaluates a constant expression, and does not read a cast
      constant it does not evaluate.** PostgreSQL 15.19's PREPARE
      parse-analyses its statement and plans nothing: `prepare s1 as select
      1/0` is prepared, while `prepare s2 as select 'x'::int from fe`,
      `prepare s3 as select case when false then 'x'::int end` and `prepare
      s4(int) as select $1 + 'a'` are 22P02 at the constant (22, 43, 32).
      Here the first is 22012 `division by zero` and the other three are
      prepared. The planner folds a FROM-less constant expression while it
      plans and reads a constant only where it folds. Since RP-500 CREATE
      FUNCTION reads every cast's constant (`sql_fn_validator::literal_casts`),
      which PREPARE could share, but it also needs planning that does not
      evaluate.
      Backlog-ID: RP-511
      Verify: tests/backlog/test_rp_found_rp500.py::test_prepare_analyses_without_evaluating

## Found while working RP-493 (2026-11-07)

Measured on a private PostgreSQL 15.19 cluster.

- [ ] **A scalar subquery's column, and a CASE folded by a computed
      constant, lose their type modifier.** `select (select
      2.5::numeric(5,2))` carries 327686 in every reader on PostgreSQL (the
      subquery's first column, `exprTypmod` of its SubLink); here it is -1
      everywhere. And `eval_const_expressions` simplifies a CASE whose WHEN
      folds to a constant before the planned readers (a portal's Describe,
      a populating CREATE TABLE AS, a SELECT from a view) see it, so `case
      1 when 1 then 'a'::varchar(3) end` and `case when 1 = 1 then
      'a'::varchar(3) end` describe 7 there (-1 in a prepared statement's
      Describe, a view's columns and WITH NO DATA, as here). Since RP-493
      only a literal `true` / `false` / `null` condition, or a literal
      COALESCE argument, is folded (`expr_typmod.rs`); a computed one needs
      the constant evaluator's own verdict on what folds.
      Verify: tests/backlog/test_rp_ctas_subquery.py::test_typmod_from_scalar_subquery_and_computed_constants
      Backlog-ID: RP-497

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

## Found fixing the READ COMMITTED move race (2026-11-07)

- [ ] **A move made inside a running statement still has the write window.**
      A READ COMMITTED block's move to a new snapshot now installs its guard
      and rolls back holding the write lock of every collection it has
      written, so no writer is between its guard check and its write
      (`rebase_user_transaction_to`, crates/secantus-storage/src/lib.rs). A
      move made mid-statement -- `rebase_active_transaction_to`, from a
      PL/pgSQL EXCEPTION block -- skips those locks, because the statement
      may already hold one of them (they are not re-entrant). There a writer
      past its check can still write a guarded row after the rollback, and
      the mover's replay then fails with 40001 after its hundred attempts,
      an answer PostgreSQL never gives at READ COMMITTED. Measured only for
      the statement-start move, where a forced interleaving reproduced the
      CI failure in all six writer shapes.
      Verify: none -- a timing window reachable only by forcing the interleaving through the storage test hook (`secantus_storage::set_test_hook`, as embedded.rs::a_write_cannot_slip_in_under_a_moving_transactions_guard does); no client-side gap test can hit it
      Backlog-ID: RP-495

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

## Found while working RP-573 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster, each statement in a new
session. The gap tests are in `tests/backlog/test_rp_found_rp573.py`. Since
RP-573 a DECLARE's type is looked up at CREATE FUNCTION and before a DO
block runs (`check_plpgsql_declarations`), from the type text libpg_query
keeps; what follows is what that lookup does not yet check.

- [ ] **A PL/pgSQL DECLARE whose type text is not one type name is
      created.** `read_datatype` passes the text to `parse_datatype` unless
      it is a `%TYPE` of a variable or column it finds, and a text that is
      not one type name is 42601 at the token, with the CONTEXT `invalid
      type name "..."`: on PostgreSQL 15.19 `declare x nosuch573%type` is
      `syntax error at or near "%"` at 78, `y nosuch573.x%type` the same at
      87, `x int(3)` `syntax error at or near "("` at 72 and `x
      varchar(nosuch)` `syntax error at or near "nosuch"` at 77. Here each
      is created. libpg_query stubs the `%TYPE` lookups out
      (`plpgsql_parse_wordtype` returns NULL), so finding the variable or
      column is this server's to do, in the block scopes PL/pgSQL keeps.
      Verify: tests/backlog/test_rp_found_rp573.py::test_declare_of_text_that_is_not_one_type_name
      Backlog-ID: RP-617
- [ ] **A PL/pgSQL variable of a pseudo-type is created.**
      `plpgsql_build_variable` refuses one: `declare x anyelement` is 0A000
      `variable "x" has pseudo-type anyelement`, no position, CONTEXT
      `compilation of PL/pgSQL function "e573" near line 1`, on PostgreSQL
      15.19 (`void` the same, near the variable's line). Here the function
      is created.
      Verify: tests/backlog/test_rp_found_rp573.py::test_declare_of_a_pseudo_type
      Backlog-ID: RP-618
- [ ] **A DECLARE's missing type is reported after a later syntax error.**
      PL/pgSQL compiles the body in one pass, so `declare x nosuch573; begin
      return 1 +; end` is 42704 `type "nosuch573" does not exist` at 69 on
      PostgreSQL 15.19. Here the whole body is parsed first, and the answer
      is 42601 `syntax error at end of input` at 104.
      Verify: tests/backlog/test_rp_found_rp573.py::test_declare_type_found_before_a_later_syntax_error
      Backlog-ID: RP-619
- [ ] **A `pg_temp` type in a session with no temporary schema is a
      missing type, not a missing schema.** `LookupExplicitNamespace`
      finds `pg_temp` only once the session has made its temp schema, so on
      PostgreSQL 15.19 `create function e573(a pg_temp.nosuch) ...` is 3F000
      `schema "pg_temp" does not exist` with no position, and `declare x
      pg_temp.nosuch` the same at 69. Here they are 42704 `type
      pg_temp.nosuch does not exist` and `type "pg_temp.nosuch" does not
      exist` (`routine_type_missing` lets `pg_temp` through as a schema).
      Verify: tests/backlog/test_rp_found_rp573.py::test_pg_temp_type_in_a_session_with_no_temp_schema
      Backlog-ID: RP-620
- [ ] **A PL/pgSQL compile error in a CREATE FUNCTION's body has no
      position.** `declare x int; begin y := 1; return 1; end` is 42601
      `"y" is not a known variable` at 80 on PostgreSQL 15.19, and `declare
      x alias for $2` in a one-argument function is 42704 `variable "$2"
      does not exist` at 82. Here the first has no position, and the second
      is 42601 with none. (A DO block's syntax errors are RP-492.)
      Verify: tests/backlog/test_rp_found_rp573.py::test_plpgsql_compile_error_in_create_has_a_position
      Backlog-ID: RP-621

## Found while working RP-458 (2026-11-08)

Measured on PostgreSQL 15.19; none needs an object.

- [ ] **`regclassin` reports a missing schema as a missing relation, and a
      quoted name's 42P01 has no position.** `select 'nosuch.s'::regclass`
      and `pg_relation_size('nosuch.s')` are 3F000 `schema "nosuch" does not
      exist` at the literal (8, 25) on PostgreSQL (`RangeVarGetRelid` looks
      the schema up first); here they are 42P01 `relation "nosuch.s" does
      not exist`. And `select '"Nosuch"'::regclass` and `select
      nextval($$nosuchseq$$)` are 42P01 at the literal (8, 16) there and
      have no position here: the server places a 42P01 by searching the
      statement for the name (`err_in`, `identifier_position`), which a
      quoted or dollar-quoted name does not match.
      Verify: tests/backlog/test_rp_found_rp458.py::test_regclassin_errors
      Backlog-ID: RP-675
- [ ] **The sequence functions over a relation that is not a sequence
      answer 42P01.** `nextval('pg_class')`, `currval('pg_class')`,
      `setval('pg_class', 1)` and `nextval('pg_class'::regclass)` are 42809
      `"pg_class" is not a sequence` with no position on PostgreSQL 15.19
      (`init_sequence` opens the relation, then checks its relkind), and
      `nextval(1234)` is XX000 `could not open relation with OID 1234`.
      Here each is 42P01 `relation "pg_class"` / `"1234" does not exist`,
      at the argument.
      Verify: tests/backlog/test_rp_found_rp458.py::test_sequence_function_over_a_relation_not_a_sequence
      Backlog-ID: RP-676
- [ ] **A named argument to a built-in whose parameters have no names is
      refused as a gap.** `select nextval(regclass := 'nosuchseq')` and
      `nextval(x => 'nosuchseq')` are 42883 `function nextval(regclass =>
      unknown) does not exist` (`x => unknown`) at 8 with the
      no-function-matches HINT on PostgreSQL 15.19 (`proargnames` is NULL
      for `nextval`, so no candidate takes a named argument); here they are
      0A000 `NamedArgExpr is not supported yet` with no position.
      Verify: tests/backlog/test_rp_found_rp458.py::test_named_argument_to_a_builtin_without_argument_names
      Backlog-ID: RP-677

## Found while working RP-517 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with `fe(a int, b text)` and
`fs(a int, c text)` holding `(1, 'x')` and `(1, 'y')`. Each also fails with
the column written out, so none is the star's. The gap tests are in
`tests/backlog/test_rp_found_rp517.py`.

- [ ] **A GROUP BY naming a correlated subquery target is refused.**
      `select (select v.a) from (values (1)) v(a) group by 1`, its `(select
      v.*)` form and `group by (select v.a)` each answer `1` on PostgreSQL
      15.19: the grouped item is the subquery, so its outer column is
      grouped. Here the first is 42803 `subquery uses ungrouped column "v.a"
      from outer query`, the star form 42P01 `missing FROM-clause entry for
      table "v"`, the written-out one 42803 `column "v.a" must appear in the
      GROUP BY clause or be used in an aggregate function`, each at 16.
      Verify: tests/backlog/test_rp_found_rp517.py::test_group_by_a_correlated_subquery_target
      Backlog-ID: RP-700
- [ ] **A LATERAL subquery's outer column is named by its type, and a star
      over an outer item there is 42P01.** `select l.* from (values (3))
      v(a), lateral (select v.a) l` names its column `a` on PostgreSQL 15.19
      (`FigureColname`), and `lateral (select fe.a, fe.b) l` names `a`, `b`;
      here they are `int4` and `int4`, `text`. `lateral (select v.*) l` and
      `lateral (select fe.* from fs) l` answer the outer item's columns
      there (`ExpandColumnRefStar` finds it in the level's range table);
      here each is 42P01 `invalid reference to FROM-clause entry for table
      "v"` (`"fe"`) with the hint that the entry cannot be referenced from
      this part of the query. RP-517 expanded such a star in an expression
      subquery only.
      Verify: tests/backlog/test_rp_found_rp517.py::test_lateral_outer_columns
      Backlog-ID: RP-701
- [ ] **A FROM function's whole row under a column alias is 42703 inside an
      expression.** `select f + 1 from generate_series(1, 2) f(x)` answers
      2, 3 on PostgreSQL 15.19, `coalesce(f)` and `(select f)` 1, 2 (the
      whole row of a function returning a base type is that value), and
      `(select (f).x)` is 42809 `column notation .x applied to type integer,
      which is not a composite type` at 17; here each is 42703 `column "f"
      does not exist`, though a bare `select f` answers, as does every form
      over an unaliased `generate_series(1, 2) f`.
      Verify: tests/backlog/test_rp_found_rp699.py::test_function_whole_row_under_a_column_alias
      Backlog-ID: RP-702
- [ ] **A correlated subquery in the WHERE of a query over a FROM function
      is 42883 and names the internal call.** `select 1 from
      generate_series(1, 2) f(x) where (select f.x + 1) > 0` answers two
      rows on PostgreSQL 15.19 and `where exists (select 1 where f.x > 1)`
      one; here each is 42883 `function \x1fcorrelated(integer, unknown,
      ...) does not exist`, the per-row call that stands in for the
      subquery leaking into the client's error. The same subquery in the
      select list, or over a VALUES list, is answered.
      Verify: tests/backlog/test_rp_found_rp699.py::test_correlated_subquery_in_a_function_scan_where
      Backlog-ID: RP-703
- [ ] **`(v).*` in a select list is 0A000.** `select (v).* from (values
      (1)) v(a)` answers `1`, column `a`, on PostgreSQL 15.19 and over `v(a,
      b)` both columns (`transformIndirection` expands a whole row's star
      as `v.*`); here each is 0A000 `this field selection is not supported
      yet`. Since RP-699 the same star inside an expression subquery is
      answered.
      Verify: tests/backlog/test_rp_found_rp699.py::test_whole_row_star_in_a_select_list
      Backlog-ID: RP-704
- [ ] **A field of an outer item's whole row in a LATERAL subquery is
      42703.** `select l.* from (values (3)) v(a), lateral (select (v).a + 0
      as k) l` answers `3` on PostgreSQL 15.19 and `lateral (select 1 as k
      where (v).a = 3)` `1`; here each is 42703 `column "v" does not exist`
      at the `v`. `v.a` there is answered, and since RP-699 `(v).a` in an
      expression subquery is too.
      Verify: tests/backlog/test_rp_found_rp699.py::test_lateral_whole_row_field
      Backlog-ID: RP-705

## Found while working RP-697 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp697.py`: `rn697(k int, a int, c text)` and
`ch697(k int, c char(3), v varchar(4))`.

- [ ] **An uncorrelated `row op ANY / ALL (subquery)` with an ordering
      operator is refused.** `select k, (k, a) < any (select 2, 3) from
      rn697` answers per row on PostgreSQL 15.19 (each subquery row a
      `RowCompareExpr` with the row); here it is 0A000 `a row comparison
      with < and a subquery is not supported yet`, as is `>= all`.
      `row_subquery_comparison` builds only `=` and `<>`; the correlated
      form is answered since RP-697.
      Verify: tests/backlog/test_rp_found_rp697.py::test_uncorrelated_ordered_row_any
      Backlog-ID: RP-727
- [ ] **A row compared with a subquery whose column has no operator with
      the row's field is not 42883.** `(a, c) = (select 1, 2)`, `c` text,
      is 42883 `operator does not exist: text = integer` at 18 on
      PostgreSQL 15.19 (`make_row_comparison_op` resolves each pair's
      operator as the statement is analysed), and so is a correlated `in`
      -- even over no rows (`... where false`). Here each is 0A000
      `comparing string with int32 using = is not supported yet` with no
      position, raised only when a row is compared, and nothing at all
      over no rows.
      Verify: tests/backlog/test_rp_found_rp697.py::test_row_pair_with_no_operator_is_42883
      Backlog-ID: RP-728

## Found while working RP-726 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp726.py`: `ch726(k int, c char(3), v
varchar(4), t text)` holding `(1, 'y', 'y', 'y')` and `(2, 'ab', 'ab ',
'ab ')`.

- [ ] **EXPLAIN of a JOIN ... ON a `varchar` column equal to a `char(n)`
      one prints the Hash Cond through `text` casts.** `explain (costs
      off) select x.k, y.k from ch726 x join ch726 y on x.v = y.c` prints
      `Hash Cond: ((x.v)::bpchar = y.c)` on PostgreSQL 15.19, the operator
      it resolved (`bpchareq`, the `varchar` side relabelled); here
      `((x.v)::text = (y.c)::text)`. The rows are right since RP-736.
      Found while working RP-736 (2026-11-09).
      Verify: tests/backlog/test_rp_found_rp726.py::test_explain_join_on_varchar_against_char_n
      Backlog-ID: RP-738

## Found while working RP-737 (2026-11-09)

- [ ] **A cancel while a 200,000-item `IN` list is planned still waits
      up to ~0.25 s, about one time in twenty.** RP-202's
      `test_a_cancel_while_a_huge_in_list_is_planned_over_a_table[0.7]`
      measured 0.33 s and then 0.31 s (its bound is 0.3 s) in the backlog
      step of `gate_pg.sh --fast` on 2026-11-09, at load 10-25 on 12
      cores. With that test's own `_cancel_after` at delay 0.7 against a
      debug `secantusd-pg`: at `b484522c`, 24 runs, 23 at 0.013-0.023 s
      and one at 0.238 s; at the `pg-batch` tip `adb234a2`, without RP-726
      and with it, 30 runs each, twice, medians 0.021-0.022 s and one late
      cancel per 30 at 0.256-0.276 s on both. So some planning pass still
      runs a stretch that does not look for a cancel, as the rewrite
      passes did before RP-202; find which one by sampling the server when
      a cancel is late. The tail reaches further under load: on the RP-733
      branch (lane pg-batch-2, debug build) the whole file's run had the
      cancel 0.62 s and 0.72 s late in two of three runs at a load average
      of about 16, then passed ten runs at loads of 11 and 23, four of them
      on a build of `aae69552`, before RP-733. Again on the RP-741 branch
      (lane pg-batch-3, debug build, 2026-11-10): 0.32 s late once in the
      gate's backlog step at a load of about 13, then the file passed five
      runs in a row at loads of 13-16. Again on the RP-707 branch (lane
      pg-batch-2, debug build, 2026-11-10): 0.68 s late once in the gate's
      backlog step at a load of about 11; then 30 cancels at delay 0.7
      each on that branch and on its base `49f2f12e` without it, medians
      0.022 s and 0.021 s, one late cancel each at 0.250 s and 0.244 s.
      Verify: none -- a timing tail that does not fail on demand; RP-202's own test is the check
      Backlog-ID: RP-735

## Found while working RP-732 (2026-11-09)

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written. The gap tests are in
`tests/backlog/test_rp_found_rp732.py`.

- [ ] **A DECLARE CURSOR computes every row's select list at the DECLARE.**
      `declare c cursor for select 1/(k - 1) from (values (2), (1)) s(k)`
      then `fetch 1 from c` answers `1` on PostgreSQL 15.19, whose cursor
      runs its plan as it is fetched; here the DECLARE itself raises the
      second row's 22012 `division by zero`, and the cursor is not
      created.
      Verify: tests/backlog/test_rp_found_rp732.py::test_a_cursor_computes_its_rows_as_they_are_fetched
      Backlog-ID: RP-744
- [ ] **A sorted cursor computes an expensive column below its Sort.** A
      cursor is planned with `cursor_tuple_fraction` (0.1), and
      `make_sort_input_target` postpones an expensive column (more than
      10 x `cpu_operator_cost`, `to_tsvector`'s `procost` is 100) past the
      Sort whenever `tuple_fraction > 0`, as for a LIMIT: `declare c
      cursor for select k, to_tsvector(case when k = 1 then 'nope' else
      'simple' end::regconfig, 'a')::text from (values (1), (2)) s(k) order
      by k desc` then `fetch 1` answers `(2, 'a':1)` on PostgreSQL 15.19.
      Since RP-732 `sort_input.rs` sees only a LIMIT, so the column is
      computed for every row and the first row's 42704 is raised. Needs the
      item above first: today the DECLARE computes every row anyway.
      Verify: tests/backlog/test_rp_found_rp732.py::test_a_sorted_cursor_postpones_an_expensive_column
      Backlog-ID: RP-745
- [ ] **A cast over a CASE of untyped literals reports a branch's input
      error at the literal.** `select (case when k = 1 then 'x' else '5'
      end)::int from (values (1)) s(k)` is 22P02 `invalid input syntax for
      type integer: "x"` with no position on PostgreSQL 15.19, which
      resolves the CASE to `text` and casts its result as the row runs;
      here the error carries the position of `'x'`, the cast having been
      applied to each branch's literal.
      Verify: tests/backlog/test_rp_found_rp732.py::test_a_cast_over_a_case_of_literals_reports_no_position
      Backlog-ID: RP-746

## Found while working RP-740 (2026-11-09)

Measured on PostgreSQL 15.19 (the shared reference on port 5415).

- [ ] **A scalar compared with an array value names the types by their
      internal names.** `select 1 = (select array[1])`, `select x = y from
      (values (1, array[1])) v(x, y)` and `select 1 = any (select
      array[1])` are each 42883 `operator does not exist: integer =
      integer[]` on PostgreSQL 15.19; here the message reads `int4 =
      int4[]`: the per-row evaluation's mismatch prints `inferred_type`
      without `display_type`.
      Verify: tests/backlog/test_rp_found_rp722.py::test_a_scalar_against_an_array_value_names_its_types
      Backlog-ID: RP-742

## Found while working RP-698 (2026-11-10)

Measured on PostgreSQL 15.19 (the shared reference on port 5415).

- [ ] **An outer column in a set operation's ORDER BY is 42703.** `select
      (select a from (values (1)) t(a) union select 9 order by b limit 1)
      from (values (5, 6)) v(a, b)` is 0A000 `invalid UNION/INTERSECT/EXCEPT
      ORDER BY clause`, DETAIL `Only result column names can be used, not
      expressions or functions.`, at 65 on PostgreSQL 15.19: the ORDER BY
      sees only the output columns, so `b` resolves to the outer row and is
      a sort expression (`transformSetOperationStmt`); `order by v.b` is the
      same. Here each is 42703 `column "b" does not exist`. A name neither
      level has is 42703 on both.
      Verify: tests/backlog/test_rp_found_rp698.py::test_an_outer_column_in_a_set_operations_order_by
      Backlog-ID: RP-767

## Found while working RP-756 (2026-11-10)

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.

- [ ] **A FROM subquery's set operation is not typed by the operator
      check.** `select x = 1 from (select 'a'::text union select 'b')
      s(x)` is 42883 `operator does not exist: text = integer` at 10 on
      PostgreSQL 15.19 (the set operation's column is its arms' common
      type, `text` when both are untyped literals); here it is 0A000
      `comparing string with int32 using = is not supported yet`, raised
      at run time with no position. `optype.rs`'s `add_derived` marks a
      set-operation FROM item incomplete, so none of its columns is
      typed. A scalar subquery over a set operation is typed since RP-756
      (`first_output_type`).
      Verify: tests/backlog/test_rp_found_rp756.py::test_a_set_operation_from_subquerys_column_is_typed
      Backlog-ID: RP-765

## Found while working RP-769 (2026-11-10)

Measured on a private PostgreSQL 15.19 cluster (locale C.UTF-8, built
with ICU) with the `_SETUP` in `tests/backlog/test_rp_found_rp769.py`.
Since RP-769 a domain, and its array type, takes its base type's
collation (`catalog_fill.rs`'s `domain_collations`).

- [ ] **`information_schema.columns.collation_name` shows only a declared
      COLLATE.** PostgreSQL 15.19's view joins `pg_collation` on
      `attcollation` and leaves out only `pg_catalog.default`, so a `name`
      column, a `name[]` column and a column of a domain over `name` show
      `C` (`collation_catalog` and `collation_schema` `pg_catalog`), and a
      declared `collate "default"` shows nothing. Here the first three
      show NULL and the last `default`: the row is built from the
      column's declared collation alone (`lib.rs`, "Only a column that
      DECLARES a collation shows one").
      Verify: tests/backlog/test_rp_found_rp769.py::test_information_schema_columns_collation_is_the_columns
      Backlog-ID: RP-783

## Found while working RP-782 (2026-11-10)

Measured on a private PostgreSQL 15.19 cluster (locale C.UTF-8, built
with ICU) with the `_SETUP` in `tests/backlog/test_rp_found_rp782.py`:
`xu782f` a domain over `text` `collate "und-x-icu"`, `xc782f` one
`collate "C"`.

- [ ] **`information_schema`'s text columns are not collated "C".**
      `information_schema.sql` declares `character_data` and `yes_or_no`
      `COLLATE "C"` (kept on the built-in domains since RP-782), and its
      views' columns are of those domains and of `sql_identifier`, so
      `collation for (data_type)` of `information_schema.columns` is
      `"C"` on PostgreSQL 15.19; here it is `"default"`, as is
      `is_insertable_into` of `information_schema.tables`. The views'
      columns are typed by their base types (`varchar`), so they also sort
      by the database's collation.
      Verify: tests/backlog/test_rp_found_rp782.py::test_information_schema_character_data_columns_are_collated_c
      Backlog-ID: RP-797
- [ ] **`pg_get_viewdef` of a system view is NULL.** `select
      pg_get_viewdef('pg_tables'::regclass)` deparses the view's rule on
      PostgreSQL 15.19 (` SELECT n.nspname AS schemaname, ...`), as it does
      for every `pg_catalog` and `information_schema` view; here every one
      answers NULL, though a user view's definition is printed.
      Verify: tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_a_system_view
      Backlog-ID: RP-798

## Found while working RP-796 (2026-11-10)

Measured on a private PostgreSQL 15.19 cluster (locale C.UTF-8, built
with ICU) with the `_SETUP` in `tests/backlog/test_rp_found_rp782.py`
(`xu782f` a domain over `text` `collate "und-x-icu"`, `xc782f` one
`collate "C"`). The gap tests are in `tests/backlog/test_rp_found_rp796.py`.

- [ ] **A `COLLATE` over an expression of a collated domain is 42804.**
      `select 'a'::xu782f collate "C" < 'B'` is false on PostgreSQL 15.19,
      `v::xu782f collate "C" < 'C'` over a VALUES list compares bytewise,
      and `order by v::xu782f collate "C"` sorts `B, D, a, c`; here each is
      42804 `collations are not supported by type xu782f` (a domain over a
      domain too). `transformCollateClause` asks `type_is_collatable` of
      the expression's type, which a domain over `text` is. The type
      checks that refuse it list base type names (the FROM-less constant
      `COLLATE` in `lib.rs`, among others) rather than asking
      `collation::collatable`, which follows a domain to its base. A
      `COLLATE` naming a locale collation (`"und-x-icu"`) over one is
      answered.
      Verify: tests/backlog/test_rp_found_rp796.py::test_collate_over_a_collated_domain_expression
      Backlog-ID: RP-802

## Found while working RP-803 (2026-11-10)

Measured on a private PostgreSQL 15.19 cluster (locale C.UTF-8, built
with ICU) with the `_SETUP` in `tests/backlog/test_rp_found_rp782.py`.
Since RP-803 a VALUES or set-operation column whose rows' implicit
collations conflict has none (`collation::INDETERMINATE`): ORDER BY and
`max` / `min` over it are 42P22, and `collation for` it is NULL.

- [ ] **A column of no collation is still grouped, made distinct and
      compared bytewise.** Over `(values ('a'::xu782f), ('B'::xc782f))
      s(v)`, `group by v` is 42P22 `could not determine which collation to
      use for string hashing` on PostgreSQL 15.19 (`hashtext`'s
      `check_collation_set`), and `select distinct v`, `count(distinct
      v)`, `select v = v` and `select v < v` are 42P22 `... for string
      comparison` (`texteq`, `text_lt`); here each answers, bytewise. A
      sort by it over no rows (`... where false order by v`) is 42P22 there,
      as `varstr_sortsupport` checks as the sort starts; here it is empty,
      since the check rides on each row's sort key. (`where v = v` answers
      both rows on both: the planner reduces it to `v IS NOT NULL`.)
      Verify: tests/backlog/test_rp_found_rp796.py::test_a_column_of_no_collation_is_not_hashed_or_compared
      Backlog-ID: RP-820

## Found while working RP-808 (2026-11-10)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp808.py`: `t808(i int, b bool,
s text)`.

- [ ] **AND / OR / NOT report a non-boolean argument at the operator.**
      `coerce_to_boolean` reports at the argument's `exprLocation`, an
      operator expression's leftmost token: `select ('x' || 'y') and
      true` is 42804 at position 9 on PostgreSQL 15.19, `(s || 'y') or
      true` at 9, `insert ... values (1 and true)` at the `1` (30). Here
      they are reported at the `||` or the `and` (25, 11, 36), and `not
      ('x' || 'y')` one character early (12 for 13). `optype`'s
      `bool_argument` and `const_value`'s BoolExpr arm use
      `expr_location`; RP-808's boolean test uses `srf_place::leftmost`.
      Verify: tests/backlog/test_rp_found_rp808.py::test_a_connectives_argument_is_reported_at_its_leftmost_token
      Backlog-ID: RP-812

## Found while working RP-811 (2026-11-10)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp811.py`: `c811(x int, a int[])`
and `create domain di811 as int`.

- [ ] **A domain's CHECK and a partial index's WHERE are not coerced to
      boolean.** `create domain d as int check (value)` is 42804 `argument
      of CHECK must be type boolean, not type integer` on PostgreSQL 15.19
      (`domainAddConstraint`'s `coerce_to_boolean`), and `check (value is
      true)` its test's own 42804, both with no position; `create index on
      c811 (x) where x` is 42804 `argument of WHERE ...` at the `x` (32,
      `transformIndexStmt`). Here the domain and the index are created. A
      table's CHECK is coerced since RP-811 (`optype::check_constraint`).
      Verify: tests/backlog/test_rp_found_rp811.py::test_a_domain_check_and_an_index_predicate_are_coerced_to_boolean
      Backlog-ID: RP-813
- [ ] **ALTER TABLE's expressions are not coerced to boolean when no row
      reaches them, and SET DEFAULT's error has a position.** On PostgreSQL
      15.19 `alter table c811 alter column x type bool using (x is true)`
      is 42804 `argument of IS TRUE ...` at 50 and `add column y bool
      generated always as (x is true) stored` the same 42804 with no
      position, both over an empty table; here each is `ALTER TABLE`, the
      expression judged only per row. `alter column x set default (1 is
      true)` is that 42804 with no position (`AddRelationNewConstraints`
      gets no query text from ALTER TABLE); here it is placed at the `1`.
      Verify: tests/backlog/test_rp_found_rp811.py::test_alter_table_expressions_are_coerced_to_boolean
      Backlog-ID: RP-815
- [ ] **A system column in a CHECK is 42703, not 42P10.** `create table
      t811 (x int check (xmin))` is 42P10 `system column "xmin" reference in
      check constraint is invalid` at 33 on PostgreSQL 15.19
      (`cookConstraint` refuses any system column but `tableoid`); here it
      is 42703 `column "xmin" does not exist`.
      Verify: tests/backlog/test_rp_found_rp811.py::test_a_system_column_in_a_check_is_42p10
      Backlog-ID: RP-816

## Found while working RP-823 (2026-11-10)

Measured on PostgreSQL 15.19; none of the statements creates an object.

- [ ] **A FOR loop over a query or an EXECUTE runs no portal.**
      `exec_stmt_fors` and `exec_stmt_dynfors` open the query as an
      unnamed portal and walk it through `exec_for_query`, which pins it:
      on PostgreSQL 15.19 `pg_cursors` lists `<unnamed portal 1>` while
      `for x in select 1 loop` (or `in execute 'select 1'`) runs, and an
      `execute 'close all'` in its body is 24000 `cannot drop pinned portal
      "<unnamed portal 1>"`. Here the loop reads the query's rows with no
      portal, so `pg_cursors` is empty and the CLOSE ALL runs. The cursor
      FOR loop pins its portal since RP-823 (`CursorState::pinned`, held by
      a procedure's COMMIT or ROLLBACK); the faithful fix walks these loops
      through a portal of their own the same way, which also brings
      `HoldPinnedPortals`' 2D000 `cannot perform transaction commands
      inside a cursor loop that is not read-only` for a loop over an
      `INSERT ... RETURNING`.
      Verify: tests/backlog/test_rp_found_rp823.py::test_a_query_loop_walks_a_pinned_portal
      Backlog-ID: RP-828

## Found while working RP-822 (2026-11-10)

- [ ] **A DROP or ALTER FUNCTION signature's `%TYPE` names no type.**
      `drop function f(t.v%type)` and `alter function f(t.v%type) rename
      to g` over a `varchar(3)` column find `f(varchar)` on PostgreSQL
      15.19 (`LookupFuncWithArgs` looks each argument type up with
      `LookupTypeName`), with the NOTICE `type reference t.v%TYPE
      converted to character varying`. Here the DROP is 42704 `type "t.v"
      does not exist` and the ALTER 42883 `function f(t.v) does not
      exist`. CREATE FUNCTION resolves one since RP-822
      (`plpgsql_pct_type::routine_percent_type`), which a signature's
      lookup can reuse.
      Verify: tests/backlog/test_rp_found_rp822.py::test_a_signatures_percent_type_is_resolved
      Backlog-ID: RP-830

## Found while working RP-799 (2026-11-10)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp799.py`: `dx` a domain over
`int` with `default 42`, `dx2` a domain over `dx`, `dnd` one over `int`
`not null default 5`, and `dpos` one over `int` `check (value > 0)`. Each
holds for a column CREATE TABLE declares and, since RP-799, for one ADD
COLUMN adds, and since RP-834 for one ALTER COLUMN TYPE retypes into a
domain.

- [ ] **`information_schema.columns.data_type` of a domain over a domain
      names its base.** A column of `dx2` is `USER-DEFINED` on PostgreSQL
      15.19 -- the view reads the domain's immediate base type, itself a
      domain -- and `integer` here.
      Verify: tests/backlog/test_rp_found_rp799.py::test_data_type_of_a_domain_over_a_domain_is_user_defined
      Backlog-ID: RP-827

## Found while working RP-751 (2026-11-10)

Measured on PostgreSQL 15.19; none of it needs DDL. The gap tests are in
`tests/backlog/test_rp_found_rp751.py`.

- [ ] **A RAISE below ERROR carries no CONTEXT.** `do $$ declare x int;
      begin raise notice 'm'; end $$` notices `m` with CONTEXT
      `PL/pgSQL function inline_code_block line 1 at RAISE` on PostgreSQL
      15.19 (`plpgsql_exec_error_callback` runs for every `ereport`, a
      notice's included); here the NOTICE, WARNING or INFO has no
      CONTEXT. An error raised there carries the frame already; a notice
      needs the interpreter's current statement and the frames of the
      callers above it (`SQL statement "..."`, the calling function's
      line) as an error collects them on the way out.
      Verify: tests/backlog/test_rp_found_rp751.py::test_a_notice_carries_its_context
      Backlog-ID: RP-833

## Found while working RP-689 (2026-11-10)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp689.py`: `t689(a d689, v
varchar(3), w dv689, c char(3))`, `d689` an `int` with `check (value > 0)`.

- [ ] **Reading a field a record lacks is 42P01.** `do $$ declare r record;
      begin select 1 as a into r; raise exception '%', r.nosuch; end $$` is
      42703 `record "r" has no field "nosuch"` on PostgreSQL 15.19
      (`exec_eval_datum` over the RECFIELD), and so is the same read of a
      variable of a row type, assigned or not. Here the reference is left
      in the SQL text, which reads it as a column of a FROM item `r`: 42P01
      `missing FROM-clause entry for table "r"`. (An assignment to such a
      field is 42703 already.)
      Verify: tests/backlog/test_rp_found_rp689.py::test_reading_a_field_a_record_lacks_is_42703
      Backlog-ID: RP-837
- [ ] **A column a record variable's field shares is not ambiguous.** With
      `declare r t689`, `(select r.a from t689 r)` is 42702 `column
      reference "r.a" is ambiguous` on PostgreSQL 15.19
      (`plpgsql.variable_conflict` is `error` by default, and
      `plpgsql_pre_column_ref` / `resolve_column_ref` find both the FROM
      item's column and the variable's field); here the variable's field is
      substituted as a parameter, which the subquery then rejects as 42601
      `syntax error at or near "$2"`. The interpreter reads any name a
      variable has as the variable (the module comment of
      `plpgsql_fn.rs` says so), a bare column name included.
      Verify: tests/backlog/test_rp_found_rp689.py::test_a_column_a_record_field_shares_is_ambiguous
      Backlog-ID: RP-838

## Found while working RP-761 (2026-11-10)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp761.py`: `t761(x int)`.

- [ ] **EXPLAIN prints a computed sort key by its synthetic field.**
      `explain (costs off) select x from t761 order by x + 1` prints `Sort
      Key: ((x + 1))` on PostgreSQL 15.19, and a key that is a window
      target `Sort Key: (sum(x) OVER (?)) DESC`; here the first is `Sort
      Key: __order0` (the planner's field for a computed key) and the
      second `Sort Key: sum DESC` (the target's output name). The Sort's
      key wants deparsing as ruleutils does it (`deparse_expression` with
      the plan's tlist), not the field it is computed into.
      Verify: tests/backlog/test_rp_found_rp761.py::test_explain_prints_a_computed_sort_key_as_its_expression
      Backlog-ID: RP-840

## Found while working RP-839 (2026-11-10)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp839.py`: `"Mx839"` a domain over
`int` with `not null check (value > 0)`, `t839(x text collate "C", y text,
m "Mx839")` and `z839(o char(4) default 'ab', v varchar(3) default 'q')`.

- [ ] **A column `LIKE` copies drops the column's collation.** `create
      table l839 (like t839)` makes `x` a column collated "C" on PostgreSQL
      15.19 (`transformTableLikeClause` copies `attcollation` with the
      type), and `information_schema.columns.collation_name` is `C`. Here
      the copy takes its type's default collation: `attcollation` is
      "default" and `collation_name` NULL, so the copy sorts and compares
      by a different collation than the column it was copied from.
      `expand_table_like` (`crates/secantus-pgplan/src/lib.rs`) renders
      each column by its type alone.
      Verify: tests/backlog/test_rp_found_rp839.py::test_a_like_copy_keeps_a_columns_collation
      Backlog-ID: RP-844
- [ ] **A domain whose name needs quotes is named bare in its errors.** An
      INSERT of NULL into a column of `"Mx839"` is 23502 `domain "Mx839"
      does not allow null values` on PostgreSQL 15.19, and a value its
      CHECK refuses is 23514 `value for domain "Mx839" violates check
      constraint "Mx839_check"`: `domain_check_input` names the domain by
      `format_type_be`, which quotes a name as `quote_identifier` does.
      Here both print `Mx839` without the quotes, as does `select
      null::"Mx839"`.
      Verify: tests/backlog/test_rp_found_rp839.py::test_a_domain_error_quotes_a_name_that_needs_it
      Backlog-ID: RP-845
- [ ] **A `char(n)` column's DEFAULT prints its cast as `::character`.**
      `information_schema.columns.column_default` and `pg_get_expr(adbin,
      adrelid)` of `o char(4) default 'ab'` are `'ab'::bpchar` on
      PostgreSQL 15.19 (`get_const_expr` prints the folded constant's
      type by `format_type_with_typemod` with no modifier, `bpchar`); here
      they are `'ab'::character`. A `varchar(3)` column's `'q'::character
      varying` already agrees.
      Verify: tests/backlog/test_rp_found_rp839.py::test_a_char_columns_default_casts_to_bpchar
      Backlog-ID: RP-846
