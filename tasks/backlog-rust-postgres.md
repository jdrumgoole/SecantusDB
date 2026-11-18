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
  query returns differs. A cursor shows it too: since RP-744 its select
  list is computed as it is fetched, and since RP-897 its WHERE runs at the
  first FETCH, so `declare c cursor for select 1/0 from (values (1)) s(k)`
  (or `... where 1/(k - 1) > 0`) raises at the first FETCH where
  PostgreSQL 15.19, which pulls the one-row VALUES up and folds, raises at
  the DECLARE.
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

- [ ] **NOT REPRODUCED -- left open as a watch item. SERIALIZABLE
  raised a false 40001 once in the GIN isolation spec (2026-11-10).**
  `tests/backlog/test_rp_isolation.py::test_ssi_index_access_methods[predicate-gin]`
  (RP-254's regression test) failed in a local `gate_pg.sh --fast` backlog
  step, under a load average of about 14 on 12 cores. In permutation
  `ra1 ro2 wo1 c1 wb2 c2`, `wb2` (`insert into gin_tbl values (array[2])`)
  was 40001 `could not serialize access due to read/write dependencies
  among transactions`, `Canceled on identification as a pivot, during
  write`. PostgreSQL 15.19 inserts and commits there: `ra1`'s `p @>
  array[1] limit 1` holds SIREAD locks only on key 1's GIN pages, so `wb2`
  makes no conflict. Here `ra1`'s read overlapped `wb2`'s write, which
  points at the read locking more GIN pages than PostgreSQL does, perhaps
  rows read past the LIMIT. That was not measured. The batch's change at
  the time, RP-843 (select-list literal typing), is not on any path this
  spec runs, and the gate before the rebase passed it. On the same commit
  it then passed 4 of 4 runs alone and 24 of 24 runs six at a time. On a
  recurrence, record which GIN pages `ra1` locked (`pg_locks` of s1 before
  `c1`) and how many rows its scan read.
  Verify: none -- intermittent, not reproduced; RP-254's isolation test is the check
  Backlog-ID: RP-848
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
  **Fourth, traced (2026-11-10):** the user-code test at 0.37 s on PG
  snapshot PR #2655 (`cd41d329`, job 130097550779). The trace shows no
  stretch without a check on the statement's own thread. Instead the
  statement's first parse ran 0.5 s (about 70 ms elsewhere) beside two
  parse threads earlier statements had abandoned, and after the cancel
  (+0.507) a privilege pre-pass that swallowed the cut-short parse
  started a SECOND parse thread (+0.526, which took 100 ms to start);
  the 57014 left at +0.866. Each abandoned thread parses to the end and
  then frees its 200,000-item tree (113 to 144 ms here). Fixed in part:
  `cancel::interruptible` now answers a pending cancel before starting
  any work, as PostgreSQL's next CHECK_FOR_INTERRUPTS would, so no parse
  starts after the cancel (unit test `cancel::tests::a_pending_cancel_starts_no_work`).
  What remains is the CPU the parse threads abandoned BEFORE the cancel
  take until they finish, since libpg_query cannot be stopped mid-parse.
  Left open as a watch item until the lane stays green.
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
    Since 2026-11-13 an ORDER BY of several plain columns under a LIMIT is
    searched too, its first column's class order the presorted key: each
    path in that order goes under an Incremental Sort beside the
    cheapest-total path's bounded Sort (`create_ordered_paths`), priced by
    `cost_incremental_sort` over the groups of the class's first member,
    and run as `nodeIncrementalSort.c` runs (`tuplesort::incremental_sort`:
    batches of 32 sorted by every key, a group past 64 rows moved to a sort
    by the later keys, each bounded by what the LIMIT still needs); a later
    key in the first key's class is a redundant pathkey. On the matrix with
    a second key (`--then m0.c`, `--then "m1.c desc"`, `--then m0.a`):
    810 of 810 shapes each (692, 676 and 778 before); with two more
    (`--then "m0.c, m1.b desc"`), 809 of 810 (711); the one left, not
    diagnosed, is two rows tying on all three keys in another order
    (`multi5/20/idx1/two_hubs2/lastleft/ordlastlim5`). Since 2026-11-17 a
    descending first key (its NULLs first) in an inner join's class is
    searched with keys after it too: the merges on its class sort
    ascending (`select_outer_pathkeys_for_merge` adopts the query's
    pathkeys only when every key is a merge clause's class), so only an
    index's backward scan, or a nested loop over one, is in its order
    (`Search::merge_asc`); `--desc --then m0.c` 1,665 of 1,665 (789 of 810
    before), `--desc --then "m1.c desc, m0.b"` 18 differences (33), the
    18 not diagnosed. Since 2026-11-17 an ASC NULLS FIRST first key with
    keys after it is searched too, under `Search::merge_asc`: no merge and
    no index scan is in its order, so the plan is the cheapest under a
    bounded Sort (`--nulls-first --then m1.c`, `--then m0.c`, `--then
    "m1.c desc, m0.b"`: 810 of 810 each, 10 differences before). Since
    2026-11-17 a DESC NULLS LAST first key in an inner join's class with a
    key after it is searched too, under `Search::merge_asc`: the index's
    forward scan is the wrong direction for merging and its backward scan
    (`back_keys`) of use for merging only, and a merge on it takes the
    scan's DESC NULLS FIRST (`generate_mergejoin_paths` follows the
    outer's pathkeys) -- `joincost` matched it against the class's
    `asc_keys` id and never weighed it (`--desc-nulls-last --then "m0.c
    desc"` 781 of 810, 776 before, none worse; `--then m1.c` 791 of 810).
    `--desc --then m0.c` measured 797 of 810 on 2026-11-17 with and
    without that change (13 differences, 10 two-table LEFT joins), not
    the 0 recorded above: not diagnosed. Declined still (the join planned
    for every row and sorted, as before): a descending first key a LEFT
    join NULL-extends or no clause reads (`ORDER_CLASS`), with a key after
    it (the 12 two-table LEFT join shapes left under `--desc-nulls-last
    --then "m0.c desc"`); a later key a
    WHERE may fix to a constant; a table with an index of several columns;
    and a two-table join with a subquery side.
  - Sorts that spill to disk runs are not PostgreSQL's merge.
  Backlog-ID: RP-028
  Verify: tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_desc_nulls_last_key_with_another_sorts_the_merge

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

- [ ] **OPEN — the first statement after DDL still rebuilds per-version
      session state from every table.** Every statement that may change the
      catalog moves the version `bump_catalog_version` keeps, and the next
      statement on each connection republishes what the planner reads
      (`install_session_state`, `publish_user_types`): the tableoid
      expressions, the relations, the constraint and index definitions and
      the comments, each listed from every table. PostgreSQL invalidates
      only the cache entries of the rows a statement changed (`inval.c`).
      psql's `\d` collation subquery straight after a `CREATE TABLE`, debug
      build, 2026-11-13: about 0.010 s with no other table and 0.014 s with
      800 (PostgreSQL 15.19: 0.33 ms with 200, 0.20 ms with 400). Fixed so
      far: keyed `pg_type` reads, unchanged table definitions reused across
      versions (RP-715); the republication reads the definitions in place
      with one oid map, and the storage keeps each collection's index list
      until a change of that collection (2026-11-12, from 0.036 s at 800);
      the index relations derived once per catalog version and planner
      generation rather than per ask, one oid map for the comments,
      constraint definitions and `pg_attribute` fill instead of a catalog
      fetch per table, and a composite found by name through a map rather
      than a scan of every row type per described column (2026-11-13, from
      0.027 s at 800); the type catalog's documents and the table
      definitions shared (`Arc`) across versions, an unchanged one reused
      by its stored bytes and its sorted place carried forward, and the
      composite listings built once per version (2026-11-13, from 0.021 s
      to 0.018 s at 800, 0.010 s with none); the row types, the plain
      tables' `tableoid` texts and the publishable relations published as
      the catalog documents they derive from and derived at a statement's
      first lookup, a `pg_catalog` type's name answered without them (in
      the planner and in the RowDescription's wire types), a keyed
      `pg_attribute` read decoding its own relation's composite alone, and
      each table's index list read in place (2026-11-13, to 0.014 s at
      800); the constraint texts rendered and every table's comments
      listed at a statement's first lookup of one (the texts render under
      the session state of that lookup, as `pg_get_constraintdef` does in
      PostgreSQL; a changed search path is part of the publication's key),
      a table with no key, unique constraint or index of its own skipped
      before any oid lookup in the index relations, and the oid map built
      only when a table has one (2026-11-13: the republication per catalog
      version at about 800 tables, sampled, from 3.4 ms to 2.6 ms); a
      table's index list read only when the storage's index-name map
      (RP-928's, kept current from the index catalog's change log) says it
      holds an index, and the relations for `regclass` listed at a
      statement's first relation lookup (2026-11-15: a `SELECT 1` straight
      after a `CREATE TABLE` at 800 tables, median of a loop, from 8.1 ms
      to 4.0 ms; the collation query's gap ratio from 0.017/0.013 s to
      0.013/0.010 s); the composite and table catalogs patched from the
      storage's catalog key log (`Storage::catalog_doc_changes_since`:
      each committed `__`-collection document write's `_id`, read off its
      oplog entry and logged at commit, as `inval.c` queues the catalog
      rows a transaction changed), so the statement after DDL re-reads
      only the documents its commit changed (2026-11-15: their full reads
      and decodes gone from a profile at 800 tables; the gap ratio, best
      of five, from 16.6/13.7 ms to 12.5/10.4 and 15.0/12.2 ms, one run
      in three under the bar; a loop's median 16.3 ms at 800 against
      15.8 ms with none). What is left, sampled at 800 tables (debug
      build, 2026-11-15), each a pass over every relation per catalog
      version, about 2 ms in all: `publish_user_types` republishing every
      table's row type, `install_session_state`'s per-table
      closures and `inheritance`, `patch_table_defs` copying the 800
      definitions' stored bytes, and `relation_oids` listing every
      relation at the collation query's `regclass` lookup. (A per-table
      memo of the index relations by the identity of their inputs was
      measured slower than deriving them, 2026-11-13.) The faithful
      end state keys the published state per relation and invalidates
      what the key log names; a user relation's entries depend on more
      than its own catalog row (its grants, its inheritance parents and
      children, the foreign keys naming it, the types its columns name),
      so each key has to carry them, or a stale entry is a wrong answer.
      Its gap test is a timing ratio and can pass once by noise (a
      `gate_pg.sh` backlog step on 2026-11-13 reported it `FIXED?`, then
      it failed three of three runs alone): rerun it alone before closing.
      (2026-11-15: whether the session holds a temp relation, for its temp
      namespace, no longer lists every relation -- it forced the deferred
      `regclass` list on every statement after DDL; a temp sequence is
      flagged when the list is deferred. The gap test then passed five of
      ten alone, and RP-928's eight of ten, at a load average near 14.)
      (2026-11-15, after RP-928's `relation_oid` became a keyed lookup:
      still five of ten alone at a load average near 12. A `sample` of a
      `CREATE TABLE` loop at 800 tables, debug build, put the statement
      start's republication at about half the samples: `patch_table_defs`
      copying every definition (~10%), `publish_user_types`' index
      relations (~6%), its `relations_derivation` temp check and the
      `session_has_temp` scans (~4% each), `inheritance` (~3%). Each is a
      linear pass, none quadratic, so what is left is the per-relation
      keying above, not another lookup to swap in.)
      (2026-11-15, in a pg-batch lane, cause of the spread unknown: it passed
      in two of four runs, so the remainder sits at its tenth's edge; a
      FIXED? from one run is not a fix, and a close needs it to pass
      every run.)
      (2026-11-17: still open. Its gap test passed CI once more, PR #2794,
      a min of five per server taken apart; a debug build here measured a
      ratio of 1.23 to 1.29 in ten of ten runs of fifteen rounds. The test
      now takes fifteen paired rounds and bounds their median, and failed
      ten of ten serially and three of three under `-n 9`.)
      (2026-11-17, platform: CI's pg-oracle (backlog) step builds the same
      `dev` profile binary, `crates/secantus-pg/target/debug/secantusd-pg`,
      and passed the paired-median test on x86_64 Linux (job 134018142698,
      PR #2821); this arm64 Mac failed six of six more runs of that build at
      a median ratio of 1.26-1.27 (14 ms against 18 ms; the test now prints
      each round's ratio). The gap shows only on macOS here, so its test
      runs `only_on("darwin")`, and verify reports it ELSEWHERE on CI. Not
      separated: macOS from arm64 (no arm64 Linux or x86_64 macOS runner
      measured). Close it when it passes every run on darwin.)
      Backlog-ID: RP-911
      Verify: tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables_within_a_tenth

## Found while working RP-911 (2026-11-15)

## Found while working RP-927 (2026-11-14)

- [ ] **OPEN — psql's `\d` column query straight after DDL builds every
      user type for a column of a user type.** The collation subquery
      (`COLLATION_QUERY`) over a table `(a e1, b e2)` of two user enums,
      first after its `CREATE TABLE`: PostgreSQL 15.19 takes 0.15 ms with
      no other table and 0.22 ms with 400 (private cluster); a debug
      `secantusd-pg` 22 ms and 79 ms. Since RP-927 a `pg_type` key of a
      built-in oid builds alone, but a user type's key builds every enum,
      composite (every table's row type) and range (`build_catalog_rows`'s
      `user_types`), and a second user key in a message cycle builds every
      row. PostgreSQL reads the one row through the oid index
      (`SearchSysCache`): the faithful fix builds a user key's row from
      that type's catalog entry alone.
      Verify: tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_over_two_user_types_costs_the_same_at_400_tables
      Backlog-ID: RP-970

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
        floats, bool and text (in byte order or under an ICU collation); an ANALYZEd array column (MCELEM
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
        an expression GIN index's scan (since 2026-11-13 its pages are
        modelled, each key evaluated from its expression); multi-column
        GiST indexes, expression GiST indexes, and a partial GiST index; a
        partial GIN index's scan (`predicate_implied_by` is
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
      first slice); an unmodelled one still reads 0 from
      `pg_relation_size`. Text keys under a column's ICU collation have
      been ordered as `varstr_cmp` orders them since 2026-11-17 (libc
      collations such as `en_US` do not exist here, a scope decision).
      A `btree_gin` index over a scalar `int2`/`int4`/`int8`/`float4`/
      `float8`/`bool`/`text` column has been modelled since 2026-11-17 (one
      key per row, the value itself); its scans are not, nor its other
      types (`date`, `timestamp`, `numeric`, ...).
      Backlog-ID: RP-287
      Verify: tests/backlog/test_rp_top10_found.py::test_a_btree_gin_date_index_has_its_pages


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
      queries of system views (`pg_stat_*`), the join in psql's `\d`
      pattern query (`mergejoinscansel`'s operators and its
      `get_actual_variable_range` probes of a histogram's ends; the nested
      loop's whole-row scans, whose columns' types the executor looks up,
      and its inner-unique scan stopping at the first match -- both follow
      from PostgreSQL's choice of join method, which the walk does not
      cost), a pattern whose fixed prefix is not all of it (`^(rpc.*)$`,
      `LIKE 'rpc_t'`: the range conditions and their estimates), ILIKE and
      `~*`, subqueries, CTEs, windows, PL/pgSQL bodies, and texts over 64
      KiB. A pattern whose fixed prefix is all of it becoming the index's
      equality, `COLLATE`'s lookups and `pg_table_is_visible` on the rows a
      scan returns are walked since 2026-11-13 (regression tests
      `tests/backlog/test_rp_catalog_reads.py::test_gap[a pattern's exact
      match is an index condition]` and `::test_gap[psql's pattern on one
      relation]`). Measured 2026-11-13: `\d rpc_t`'s first query 34
      counters match, 5 differ (10 of its statement's 24 differed before);
      a `pg_stat_*` view 16 match, 22 differ.
      Backlog-ID: RP-290
      Verify: tests/backlog/test_rp_catalog_reads.py::test_gap[a statistics view], tests/backlog/test_rp_catalog_reads.py::test_gap[psql's describe query]
- [ ] **An ORDER BY an index scan already returns in order is counted as
      sorted, and the index's pathkeys are not read.** With query pathkeys
      (an ORDER BY, a GROUP BY, a join), PostgreSQL 15.19 makes each
      B-tree's leading key a pathkey (`build_index_pathkeys`: the key's
      family's equality operator, an AMOPSTRATEGY search, and that
      operator's merge-join families), going on to the next key when an
      equality fixes the column; and when the index scan it chooses returns
      the rows in the ORDER BY's order it puts no Sort over it, so the
      executor never looks up the sort support function. On `SELECT c.oid
      FROM pg_class c WHERE c.relname = 'rpc_t' ORDER BY c.relname` this
      server counts the Sort's `pg_amproc` lookup; ordered by
      `relnamespace` it also misses `oid_ops`' equality (2026-11-13, found
      while working RP-290).
      Verify: tests/backlog/test_rp_catalog_reads.py::test_gap[an ORDER BY an index gives]
      Backlog-ID: RP-934
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
      still reads as all-visible. Since 2026-11-13 a catalog read directly
      through an index holding every column it reads is an index-only
      scan here too (`IndexOnlyNext`: the visibility map's page once, the
      heap only off an all-visible block; regression test
      `test_an_index_only_catalog_scan_reads_the_visibility_map`), with the
      same two gaps: each row DDL added is fetched as a block of its own,
      and an `initdb` row on a page DDL wrote to is not fetched (`select
      relname from pg_class where relname = 'pg_type'` after this module's
      DDL: PostgreSQL 15.19 reads 7 `pg_class` blocks, this server 6).
      Backlog-ID: RP-291
      Verify: tests/backlog/test_rp_catalog_reads.py::test_catalog_blocks, tests/backlog/test_rp_catalog_reads.py::test_a_page_ddl_wrote_to_is_not_all_visible
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
- [ ] **A catalog read directly is scanned by its leading equalities
      alone.** The executor's catalog scan (`catalog_reads.rs`) takes the
      first index whose leading column an equality matches, keyed by the
      equalities on its leading columns. PostgreSQL 15.19 builds its index
      paths from every clause (`match_clause_to_indexcol`): `select attnum
      from pg_attribute where attrelid = 1259 and attnum = 1` scans
      `pg_attribute_relid_attnum_index` for both keys (`[5, 5, 4]` over the
      statement), where this server scans `pg_attribute_relid_attnam_index`
      for `attrelid` alone (39 entries); and `select relname from pg_class
      where relname >= 'pg_s' and relname < 'pg_t'` returns 74 entries of
      `pg_class_relname_nsp_index`, where this server walks all 415. Their
      heap and index blocks follow (found while working RP-291).
      Verify: tests/backlog/test_rp_catalog_reads.py::test_a_catalog_scans_index_path[two equalities], tests/backlog/test_rp_catalog_reads.py::test_a_catalog_scans_index_path[a range]
      Backlog-ID: RP-935
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
- [ ] **A chain of inequality joins under a LIMIT runs far past the rows
      it returns.** `select e.a, e3.a, zs.a, e4.a from e join e e3 on e.a <
      e3.a join zs on e3.a > zs.a join e e4 on e4.a < zs.a limit 7 offset
      3` (`e` 1,000 rows, `zs` 10) answers at once on PostgreSQL 15.19,
      whose bushy `(e join e3) x Materialize (e4 join zs)` stops after ten
      rows; here it runs in the same tree and takes 100-140 s on a debug
      build (the two `test_a_bushy_chain_*` tests in
      `tests/backlog/test_rp_explain_costs.py`), as though a loop's rows
      were produced whole before the Limit reads them. The cause is not
      established: count the rows each loop produces before naming one
      (found while working RP-919).
      Verify: none -- a performance budget; the two tests named above time it
      Backlog-ID: RP-1020
- [ ] **EXPLAIN VERBOSE prints no `Output` lists on most joins' nodes.**
      A plan over one relation prints as VERBOSE does: its scan (RP-925)
      and the Sorts and Limits above it, each with the scan's `Output` and
      its `Sort Key` qualified; and a hash join of two relations on column
      equalities with no WHERE prints every node's `Output` and its `Inner
      Unique`, and a nested loop of two relations whose inner side is a
      parameterised index scan prints both scans' physical `Output` and the
      `Index Cond` prefixed, and since 2026-11-17 such a hash join with a
      WHERE whose conjuncts each read one relation prints them as its
      scans' `Filter`, prefixed (RP-966, in part:
      `ruleutils::explain_verbose_join`, `explain::attach_verbose_join`,
      `explain::attach_verbose_nested_loop`). Every other join does not:
      PostgreSQL 15.19 prints a Merge Join's, its Materialize's and
      Memoize's `Output` likewise; here the plain plan. (Since 2026-11-17
      a nested loop over a scan and a Materialize of a scan, the ON its
      `Join Filter`, prints every node's `Output`:
      `explain::attach_verbose_materialized_loop`.) Nor does a WHERE conjunct reading both relations, an ON that is not column
      comparisons, or three or more
      relations (since 2026-11-17 a Sort on a computed key over one
      relation prints its resjunk entry, `Output: a, ((a + 1))` and `Sort
      Key: ((k.a + 1))` over a scan whose `Output: a, (a + 1)`, and a
      nested loop into a parameterised index scan with a WHERE whose
      conjuncts each read one relation prints them as its scans'
      `Filter`, prefixed). Where to build it: every node's targetlist
      deparsed (`show_plan_tlist`, prefixed when the range table holds more
      than one relation; a node under a Hash, Sort or Material returns only
      the columns needed above, `CP_SMALL_TLIST`), its quals and keys
      printed with `unqualified = false`.
      (Since 2026-11-17 an inner index scan's `Filter` reading both
      relations, an ON comparison, prints prefixed: `VerboseRel::join_quals`.)
      (Since 2026-11-17 a hash join with a WHERE conjunct comparing a
      column of each relation prints it as its `Join Filter`, its columns
      in the Hash's `Output`: `VerboseJoin::join_filter`.) (Since 2026-11-17 a nested loop does too:
      into a parameterised index scan the comparison is that scan's
      `Filter`, prefixed, and over a Materialize it joins the ON's
      conjuncts in the one `Join Filter`.) (Since 2026-11-17 a WHERE
      comparison of an expression with the other relation's column does
      too, as an inner `Index Cond` or a `Join Filter`, both already
      prefixed: `VerboseJoin::unprinted_filter`.) Still the plain plan:
      such an expression evaluated as the parameterised inner index
      scan's `Filter` (`on e.a < k.a where e.a + k.a < 7`, PostgreSQL's
      `Filter: ((e.a + k.a) < 7)`): `join_quals` holds only
      column-to-column texts, so `attach_verbose_nested_loop` declines.
      The fix is to print each cross-relation conjunct with a Printer
      over both relations' scopes, bare for the scan's own columns, in
      place of the hand-built `join_quals` strings.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of an inner index scan filtering an expression of both]
      Backlog-ID: RP-966

- [ ] **A hash join whose WHERE compares an expression of one relation
      with the other's column is priced at zero.** `explain select e.b
      from e join k on e.a = k.a where length(k.b) <> e.a` (both tables
      analysed, 1000 rows) prints `Hash Join  (cost=0.00..0.00 rows=0
      width=0)` and zero costs on every node below it; PostgreSQL 15.19
      prints `Hash Join  (cost=27.50..45.14 rows=999 width=3)` over scans
      costed `0.00..15.00 rows=1000`. Found while working RP-966; the
      same query with a column-to-column WHERE is priced.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a hash join with an expression join filter is priced]
      Backlog-ID: RP-1039


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
      Sized 2026-11-15: `user_index_ctids` (lib.rs) refuses any heap with
      `has_dead_entries`, and the heap model keeps no index entries: a
      dead non-HOT version's key is not recorded (`Xmax` holds only the
      row's `_id`, and the update may have changed the key), and nothing
      models `_bt_killitems` -- PostgreSQL's first scan fetches the dead
      entry (`Heap Fetches: 2`) and kills it, so a second scan prints 1.
      The fix is an index-entry model (key per version, killed bit,
      removed by VACUUM's index pass), persisted beside the heap.
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
      too, and since 2026-11-15 a LEFT join filter over a char(n)
      column and a chain of plain Nested Loops over Seq Scans (`z join z
      z2 on z.a < z2.a join z z3 on z2.a < z3.a`), and since 2026-11-15
      a chain whose top join's inner side is a Memoize over an index scan
      (`z join z z2 on z.a < z2.a join kz on z2.a < kz.a`). Still
      unannotated: a LEFT join with a WHERE (unpriced too, RP-379), a
      parameterized inner index scan with its own Filter, a Merge Join (its
      Sorts' `Sort Method`), a hash table past `hash_mem` (several
      batches), and a join with an inner-unique inner side. A chain whose
      lower join is a Hash Join (`z join kz on z.a = kz.a join z z2 on
      z2.a < kz.a`) is counted since 2026-11-15, but is not priced: the
      join search does not take its `<` clause, so the top join's inner
      Seq Scan prints without the Materialize PostgreSQL 15.19 puts over
      it (`Materialize (actual rows=20 loops=20)`).
      Measured on a private 15.19 cluster (found while working RP-386,
      the chain while working RP-354).
      Sized 2026-11-15: `nest_loop_chain_estimate` (scan_estimate.rs)
      admits only inner joins with no hash or merge clause (`inner_join`
      refuses a non-empty `equi`), so the Materialize needs the join
      search to build hash-join paths for a level's relations
      (`hash_inner_and_outer`) beside its nested loops, then price the
      top nested loop's inner through `cost_material`.
      Backlog-ID: RP-390
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a nested loop over a hash join]
- [ ] **A join nested in an outer join prints no estimate, and its join
      order and method are not PostgreSQL's.** Since 2026-11-15 an inner
      join's one-table ON conjuncts under an outer join, and an outer
      join's ON conjunct over one table of an inner join on its nullable
      side, run in their tables' scans (RP-393, RP-397), and where the join
      search keeps the written order the plan prints as PostgreSQL 15.19's
      does (`s left join (e join s s2 on e.a = s2.a) on s.a = e.a and s2.c =
      'xx'`). But every node of such a plan prints `cost=0.00..0.00 rows=0
      width=0`, and the join is not searched: `select * from s left join (e
      join k on e.a > 998 and k.a = e.a) on true` is, in PostgreSQL, a
      `Nested Loop Left Join` over a `Materialize` of a `Nested Loop` into
      `Index Scan using k_pkey on k` (`Index Cond: (a = e.a)`,
      cost=0.28..36.16 rows=60); here a Hash Join of two Seq Scans with no
      Materialize, and under ANALYZE no actual block below the top join.
      `select * from s left join (e join k on e.a < k.a) on s.a = e.a and
      k.b = 'v3'` is PostgreSQL's `Hash Right Join` with `s` hashed and a
      `Materialize` over `k`; here a `Hash Left Join` hashing the inner
      join. Since 2026-11-17 the same inner join alone (`e join k on k.a =
      e.a where e.a > 998`) is PostgreSQL's nested loop into `k_pkey`
      parameterized by the outer row, priced (`JoinPick::NestParam`, drawn
      by `explain::price_param_nest_loop`), where the unique-key path
      search (`unique_key_other_paths`) picks it with no Memoize, and
      since 2026-11-17 that inner read index-only where it reads only its
      key (`select e.b from e join k ...`, `HashSide::key_only_read`, the
      visibility map's fraction applied), and since 2026-11-17 a range on
      the hashed side's key read through its index (`where k.a < 10`,
      `Hash Join` over an `Index Scan using k_pkey`; that scan is the
      side's ordered path in `unique_key_other_paths`), and since 2026-11-17
      bounds on both sides' keys (`where e.a > 998 and k.a < 2000`, the
      bound joining the inner's `Index Cond: ((a = e.a) AND (a < 2000))`).
      Sized 2026-11-18: every three-table plan with an outer join over a
      nested join prints zeros here, whatever its shape, including the
      one whose order and methods already match
      (`s left join (e join s s2 on e.a = s2.a) on ...` is PostgreSQL's
      `Hash Left Join (cost=20.16..21.58 rows=30 width=47)` over a hashed
      `Hash Join (cost=1.39..20.15 rows=1 width=27)`):
      `hash_join_estimate` / `joined_sides` price only a two-leaf
      `JoinNode::Join`. The work is a join search over the outer join's
      sides with the nested join as one priced relation
      (`make_rel_from_joinlist` keeps the outer join's order constraints),
      then each method as `joincost.rs` prices it for two leaves. The
      `on true` plan's arithmetic checks out from 15.19's own formulas:
      `Materialize` over the 34.10 nest loop is 0.28..34.11
      (`cost_material`, 2 x cpu_operator_cost per row), and the
      `Nested Loop Left Join` 0.28..36.16 (outer 1.30, inner 33.83 run,
      29 rescans of 0.005, 60 x cpu_tuple_cost). The `k.b = 'v3'` plan is
      `Hash Right Join (cost=1.68..1669.42 rows=1000 width=34)` over
      `Nested Loop (cost=0.00..1532.75 rows=33333)` and a `Materialize
      (cost=0.00..18.00 rows=100)` of `k`. Under ANALYZE the executor runs
      a different tree from the one printed, so the actuals need the
      executed shape to follow the chosen one.
      Measured on a private 15.19 cluster (found while working RP-393 and RP-397).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an inner join under an outer join with a one-table ON conjunct], tests/backlog/test_rp_explain_costs.py::test_plan[a left join's ON conjunct over a table of an inner join it null-extends]
      Backlog-ID: RP-978
- [ ] **EXPLAIN prints no estimate for a join over a scan filtered by an
      expression.** Since RP-404 a comparison of integer or text arithmetic
      with a constant, and a LIKE / ILIKE of an analysed string column, are
      estimated as PostgreSQL 15.19 estimates them; a comparison of a
      built-in call over columns and literals whose every overload is of
      one `procost` (`expr_side` in `secantus-pgplan`); a pattern match of
      such an expression (`lower(b) like 'v%'`, `rows=5`; `ExprQual::pattern`)
      and a pattern over a table never analysed (`where_selectivity`).
      Since 2026-11-13 a regex match over an analysed column is estimated
      (RP-354), and since 2026-11-17 a join over a scan a LIKE / ILIKE or
      regex match filters (the join's WHERE patterns collected by
      `explain_join_patterns`). Still zeros: a call of a function whose
      overloads differ in `procost`; a regex whose prefix walk is not
      modelled (an anchor in an optional group; embedded options). Since 2026-11-18 a `^` no
      character precedes, inside a group or not, anchors the match as
      `regc_nfa.c`'s optimisation pulls it into the `pre` state
      (`strip_bos`: `^(?:^w0)1` and `(?:^w0)1` are both prefix `w01`). A back reference is walked as a copy of
      the group it names since 2026-11-17, as the regex compiler copies it,
      and since 2026-11-17 a group repeated `+` (a capturing one is
      `parseqatom`'s `x*x`, its prefix the group's first character; a
      non-capturing one a whole copy), and since 2026-11-18 a group
      repeated `{m,}` with m of 2 or more (one whole copy: `repeat`
      builds `x{m-1,}x` down to `x+`), and since 2026-11-18 a group
      repeated within a bounded range, which fixes no character of
      itself (`^x(ab){1,2}` `x`, `^(ab){1,3}` none). Since 2026-11-17 an
      inner ON join whose WHERE pushes expression comparisons to its sides
      prices each side as that scan's residual
      (`ExplainOptions::join_expr_quals`, `hash_side_with`), and since
      2026-11-17 a comma join, whose residual holds the join clause beside
      the comparison; neither scan carries a column only its comparison
      reads; since 2026-11-17 a LEFT join whose WHERE compares only
      preserved-side expressions (`preserved_exprs`), and an inner ON
      join whose WHERE holds inequalities over both tables, its Join
      Filter (`where_join_filters`). Since 2026-11-18 a `^` in a branch
      of alternatives nothing precedes leaves no prefix, as its second BOS
      arc out of `pre` does in `findprefix` (`^(?:w|^x)0` is `rows=1000`,
      `^(?:^ab|^ab)` and `^(?:w|^x)?0` no prefix). Since 2026-11-18 a `^` every path meets after a character
      makes the regex `REG_UIMPOSSIBLE`, no prefix (`^w(?:^0)1` is
      `rows=1`), and since 2026-11-18 a branch whose `^` a character
      precedes is pruned as dead, as `regc_nfa.c` prunes it
      (`prune_dead_branches`: `^x(?:^w|y)0` is prefix `xy0`,
      `^w(?:^x|0)0` `rows=91`). Embedded options are still zeros
      (`(?x)^w 0 0` is `rows=91` on 15.19). Measured
      on the 15.19 reference on port 5415 (found while working RP-404 and
      RP-354). The repeated group's prefix is not a walk of the regex's
      text: `pg_regprefix` reads the NFA after `regc_nfa.c`'s `optimize`
      (`fixempties`, state merging), and a capturing group's `x{m,n}` is
      first rewritten `x{m-1,n-1}x` (`parseqatom`). The prefixes 15.19
      gives (an index's `Index Cond` over a `"C"` column): `^(w0)+1` `w`,
      `^(ab)+a` `a`, `^a+a` `a`, `^(ab){2,}c` `ab`, `^a{3,}` `a`,
      `^(ab){1,3}` none -- `^(ab){2,}c` stops short of what a hand walk of
      `repeat`'s arcs gives (`aba`). The faithful fix ports `regcomp.c` /
      `regc_nfa.c`'s NFA build and optimisation and runs `findprefix` on
      it (2026-11-17, sized while working RP-841).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with embedded options]
      Backlog-ID: RP-423
- [ ] **A semi join cheaper unique-ified keeps the semi join's shape.**
      Since RP-411's first slice (2026-11-16) a WHERE that is one
      uncorrelated `col IN (select col from t)` / `= any`, or one `EXISTS`
      correlated by a single column equality, prints PostgreSQL 15.19's
      `Hash Semi Join` shape (`secantus-pgplan/src/sublink_join.rs`, drawn
      by `explain::semi_join_tree`); over an inner column a unique index
      fixes it is planned as the inner join `reduce_unique_semijoins` makes
      of it (`sublink_join::as_inner_join`, chosen in the server's EXPLAIN
      arm by `unique_index_fixed`), costs included. The semi join is priced
      (`PgHandler::semi_join_estimate`, `hash_semi_join_of`:
      `eqjoinsel_semi` with its MCV branch, `final_cost_hashjoin`'s
      JOIN_SEMI) where it beats the inner join of the unique-ified inner
      side, hashed either way, and lower bounds of the merge and
      nested-loop paths, and where the inner join of the unique-ified inner
      side hashed over its `HashAggregate` is cheaper (`create_unique_path`,
      its semi factors those of the base relation as
      `compute_semi_anti_join_factors` takes them) it is planned and printed
      as that `Hash Join`. Still open: (1) Merge and nested-loop semi
      joins, the unique-ified side as the outer one, and an outer side read
      through an index, are not priced. Since 2026-11-18 an IN subquery's
      own WHERE over its relation filters the inner scan
      (`SemiJoin::inner_filter`, `inner_quals`) and is priced, read through
      the index it bounds where that is cheaper (`select * from e where b
      in (select b from k where a > 990)` hashes a `HashAggregate` over
      `Index Scan using k_pkey`), the unique-ified side's groups scaled to
      its rows (`estimate_num_groups`) and its hash buckets one entry each
      (`final_cost_hashjoin`'s UniquePath rule,
      `joincost::hash_join_over_unique_path`). (2) Wider shapes still print
      the scan: an IN comparing expressions (`a::bigint in (select
      a::bigint + 0 ...)`, which 15.19 groups by the expression), a
      cross-type comparison, and an
      outer conjunct other than a comparison of an expression with a
      constant that the scan's filter does not hold whole. Outer conjuncts
      are pulled up with the sublink and filter that scan
      (`sublink_join::split_where`); one that does not lower is priced as an
      `ExprQual` (`SemiJoin::outer_exprs`).
      Backlog-ID: RP-411
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery comparing expressions is planned as a join]
- [ ] **A node above an InitPlan's scan is unpriced.** Since
      RP-440's first slices (2026-11-16) a SELECT over one relation whose WHERE
      compares columns with uncorrelated FROM-less scalar subqueries plans
      each as an InitPlan on the top node, as PostgreSQL 15.19 does
      (`secantus-pgplan/src/initplan.rs`, `$n` printed through
      `ruleutils::with_init_plans`), and prices a plain Seq Scan over them
      (`var_eq_non_const`, `DEFAULT_INEQ_SEL`), and a Sort above that scan
      (keys qualified, as the InitPlan lengthens the range table); a join's
      ON conjunct of that shape is placed in its side's scan as `Filter: (b =
      $0)`, inner and LEFT join alike (`initplan::on_init_plans`). Counts
      over the scan are an Aggregate straight over it, priced
      (`PgHandler::init_plan_count_agg`), and a Limit (OFFSET included)
      straight over the scan (`limit_estimate`). Still open: a subquery with a
      FROM, a correlated one, or a sublink beside any other non-constant
      qual is still printed inline (`select * from e where b = (select b
      from k limit 1)`); a join's ON InitPlan is unpriced; other nodes above
      the scan (a computed sort key, a GROUP BY of anything but counts of
      stored columns) are unpriced. Counts grouped by stored columns
      straight over the scan, on a table with no index, are hashed and
      priced since 2026-11-18 (`init_plan_grouped_counts`); a grouped
      `sum` or other aggregate is not. A SELECT
      DISTINCT of stored columns straight over the scan, on a table with no
      index, is hashed and priced since 2026-11-18 (`create_distinct_paths`
      against a Sort and Unique); over an indexed table, or with an ORDER
      BY, it is still unpriced. A Limit above a Sort of the scan, the Sort bounded
      by OFFSET + LIMIT (`cost_sort`'s `limit_tuples`), is priced since
      2026-11-18. Plain aggregates other than counts over the scan are
      priced since 2026-11-18 (`plain_agg_estimate`). Measured on a private
      15.19 cluster.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a grouped sum above an initplan's scan is priced]
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


## Found while working RP-451 (2026-11-07)

Measured on a private PostgreSQL 15.19 cluster with `fe(a int, b text)` and
`fs(a int, c text)`, one row each (`(1, 'x')`, `(1, 'y')`).

## Found while working RP-465 (2026-11-07)

Measured on a private PostgreSQL 15.19 cluster.

## Found while working RP-501 (2026-11-07)

Measured on PostgreSQL 15.19 (the data-modifying WITH on a private cluster).


## Found while working RP-486 (2026-11-07)


## Found while working RP-485 (2026-11-11)



## Found while working RP-472 (2026-11-07)


## Found while working RP-454 (2026-11-07)



## Found while working RP-479 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `fs(a int, c text)`
holding `(1, 'y')`.


## Found while working RP-519 (2026-11-14)

Measured on a private PostgreSQL 15.19 cluster.

- [ ] **COMMENT ON a built-in type, operator or access method does not
      replace initdb's description.** PostgreSQL 15.19 keys the comment by
      the object's oid in `pg_description`, replacing initdb's row (IS NULL
      deletes it). Here initdb's rows are served for every catalog
      (`pg15_other_descriptions.tsv`, since RP-969's second part), but
      `comment on type int4 is 'x'` is accepted and changes nothing, and
      COMMENT ON an operator or access method is 0A000. Faithful: store
      such a comment by `(classoid, oid)` as `BUILTIN_PROC_COMMENT_COLLECTION`
      does for routines, a document with no comment keeping initdb's
      deleted, for every catalog `pg15_other_descriptions.tsv` covers.
      Verify: tests/backlog/test_rp_found_rp514.py::test_comment_on_builtin_object_replaces_initdb_description
      Backlog-ID: RP-969

## Found while working RP-527 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster. The gap tests are in
`tests/backlog/test_rp_found_rp527.py`.


## Found while working RP-532 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `fs(a int, c text)`
holding `(1, 'y')` twice, `(2, 'z')` and `(null, null)`, and `fk(k fs)`
holding `row(null, null)`, `row(1, null)`, `row(1, 'a')` and `null`. The
gap tests are in `tests/backlog/test_rp_found_rp532.py`.


## Found while working RP-590 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp590.py`.

- [ ] **The `record_image` operators refuse some field types.** `*=` ..
      `*>=` compare a record's fields by their binary images since RP-650,
      for int2 / int4 / int8 / oid / bool / float4 / float8 / numeric /
      date / timestamp / timestamptz / uuid / text / varchar / bpchar /
      bytea fields. Every other type is 0A000 `a record_image comparison
      over a field of type ... is not supported yet`; on PostgreSQL 15.19
      `row('1 day'::interval) *= row('24 hours'::interval)` is `f` (16
      fixed bytes: time, day, month), a time compares its int64
      microseconds, a name its 64 NAMEDATALEN bytes. Each needs its
      datum's image modelled in `record_image.rs` (`record_image_cmp`:
      by-value words, fixed-length by-reference bytes, varlena data).
      Verify: tests/backlog/test_rp_found_rp590.py::test_record_image_over_remaining_field_types
      Backlog-ID: RP-976

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
      Surveyed 2026-11-14 (no code changed): a sized `char(n)` stored
      trimmed and padded on output is already PostgreSQL's answer, so only
      the unsized column needs the as-written value. The trimmed form is
      assumed at many sites that each need a `bpchar`-aware key instead:
      `join_text_hash` (secantus-pg lib.rs), `coerce_to_column`'s
      `bpchar[]` arm and `arrays.rs` equality, `hashpart.rs`,
      `hashfn.rs` `hashbpchar`, `cmpfn.rs` `bpcharcmp`, and the sort-key
      paths in lib.rs that trim before comparing. UNIQUE could reuse
      `value_unique` / `check_collated_unique` (already used for jsonb and
      nondeterministic collations); a PRIMARY KEY is `_id`, so it needs a
      trimmed key with the as-written value kept in the row. Plan: (1)
      keep blanks for unsized `bpchar` on write, plus UNIQUE through
      `value_unique`; (2) a PK key; (3) GROUP BY / DISTINCT / indexed `=`.
      Reviewed 2026-11-14 for a bundle session: step (1) cannot land
      alone. Once the blanks are stored, a PK, GROUP BY, DISTINCT and an
      indexed `=` over an unsized `bpchar` would separate `'k'` from
      `'k '`, which is a wrong answer PostgreSQL never gives. (2) and (3)
      have to land with it, or before it behind trimmed keys (in
      `group_key_ident` at lib.rs ~2782, `join_text_hash`, and an `_id` key).
      So the first PR should be the keys, which change no answer.
      Probed 2026-11-14 (bundle session, no code changed): an expression
      keeps the blanks already (`octet_length('ab  '::bpchar)` is 4 here
      as on 15.19); only the stored cell loses them, at `cast_value`'s
      `target == "bpchar"` arm (secantus-pgplan lib.rs ~40876), which trims
      every `bpchar` cast, sized or not. GROUP BY / DISTINCT / PARTITION BY
      already key a `bpchar` value trimmed (`typed_ident`, RP-725) and
      `join_text_hash` trims, so those two keys are done. What remains
      before the write can keep blanks: the `_id` key of a `bpchar`
      PRIMARY KEY (trimmed key, as-written value kept in the row -- a
      storage-layout change), UNIQUE through `value_unique`, and an
      indexed `=` over a trimmed index key.
      Re-read 2026-11-14 (second bundle session, no code changed): add to
      that list the pushed-down `=` filter. `cast_value`'s comment says
      equality "lowers to a filter against the stored value", so a
      `where a = 'xy'` over a stored `'xy '` would miss in the MQL engine,
      which compares strings exactly. The faithful shape is a `bpchar`
      comparison that ignores trailing blanks in every key and filter
      path (`bpchareq` / `bcTruelen`), not a trimmed copy of the value;
      that is several PRs touching the stored row, so it needs its own
      planned sequence, not a bundle slot.
      Verify: tests/backlog/test_rp_found_rp672.py::test_create_table_as_of_an_unsized_bpchar_keeps_its_blanks
      Backlog-ID: RP-693

## Found while working RP-665 (2026-11-10)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp665.py`: `t665(a d665, c dc665, b int)`
over `create domain d665 as int` and `create domain dc665 as char(4)`.


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


## Found while working RP-750 (2026-11-14)

Measured on PostgreSQL 15.19 (port 5415) with statements that create
nothing. The gap tests are in `tests/backlog/test_rp_found_rp750.py`.

- [ ] **A `numeric` type name with precision 0 is accepted.**
      `select '1.5'::numeric(0,1)` is 22023 `NUMERIC precision 0 must be
      between 1 and 1000` at 15 on PostgreSQL 15.19 (`numerictypmodin`
      checks the modifiers when the type name is read); here the cast runs
      and is 22003 `numeric field overflow`. Found while working RP-953,
      whose `numeric_in(c, 0, 5)` applies that same typmod at run time and
      must keep doing so.
      Verify: tests/backlog/test_rp_found_rp750.py::test_numeric_precision_zero_is_refused
      Backlog-ID: RP-1042
- [ ] **A cast from `cstring` to a type with no such cast is accepted.**
      `select 'x'::cstring::int4` is 42846 `cannot cast type cstring to
      integer` at 20 on PostgreSQL 15.19 (`cstring` casts only to the
      string types, by I/O); here it reads the value as `int4` (22P02 for
      this one, 12 for `'12'`).
      Verify: tests/backlog/test_rp_found_rp750.py::test_cstring_casts_only_to_strings
      Backlog-ID: RP-1041

## Found while working RP-570 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `create domain d570c as
char(3)` and `t570(k int, t text, ta text[])` holding `(1, 'ab', '{ab}')`
and `(2, 'abcd', '{abcd}')`. The gap tests are in
`tests/backlog/test_rp_found_rp570.py`.


## Found while working RP-607 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `create domain d607q
as int`. The gap tests are in `tests/backlog/test_rp_found_rp607.py`.


## Found while working RP-612 (2026-11-14)

Measured on PostgreSQL 15.19; none of it needs DDL. The gap tests are in
`tests/backlog/test_rp_found_rp612.py`.


## Found while working RP-552 (2026-11-14)

Measured on PostgreSQL 15.19 with `create domain pint as int check (value >
0)`, `create domain nnint as int not null`, `create type ctp as (a pint, b
text)` and `create table tct (c ctp, d pint[])` holding `('(1,x)', null)`.
The gap tests are in `tests/backlog/test_rp_found_rp552.py`.

- [ ] **A `text` column an `INSERT ... SELECT` writes to a composite
      column is accepted.** `insert into tct(c) select '(0,x)'::text` is
      42804 `column "c" is of type ctp but expression is of type text` at
      27 on PostgreSQL 15.19, as is the bare literal under a UNION, an
      ORDER BY, a DISTINCT or a subquery in FROM (each resolves the
      `unknown` column to `text` first; `text` has no assignment cast to a
      composite). Here the text is read as the composite (a field's domain
      error, 23514, for these). Found while working RP-945, which placed
      the plain SELECT's literal (`check_assigned_literal` over its target
      list).
      Verify: tests/backlog/test_rp_found_rp552.py::test_insert_select_text_into_composite
      Backlog-ID: RP-1044

## Found while working RP-534 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster with `fs(a int, c text)`,
`create domain dfs as fs` and `fd(k dfs)` holding `row(null, null)`,
`row(1, null)`, `row(1, 'a')` and `null`.


## Found while working RP-500 (2026-11-07)

Measured on a private PostgreSQL 15.19 cluster with `fe(a int, b text)`.

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
      changes EXPLAIN only, and locally the test took 3.5 s), and on
      snapshot PR #2497's `pg-oracle (backlog)` job (`b8976e15`,
      2026-11-08), waiting for ReadyForQuery after the test's `CREATE
      VIEW`. (This was RP-606.)
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

## Found while working RP-619 (2026-11-14)

Measured on a private PostgreSQL 15.19 cluster.

- [ ] **A CREATE FUNCTION's compile error without a position has no
      compile context.** An error PL/pgSQL's grammar raises without a
      scanner position is framed `compilation of PL/pgSQL function "<name>"
      near line N` (`plpgsql_compile_error_callback`): on PostgreSQL 15.19
      `create function u958() returns void language plpgsql as $$ begin
      raise unknown_thing; end $$` is 42704 `unrecognized exception
      condition "unknown_thing"` with CONTEXT `compilation of PL/pgSQL
      function "u958" near line 1`. Here the CONTEXT is missing. A DO block
      has it since RP-958 (`do_compile_error`).
      Verify: tests/backlog/test_rp_found_rp619.py::test_create_function_compile_error_has_context
      Backlog-ID: RP-1040

## Found while working RP-573 (2026-11-08)

Measured on a private PostgreSQL 15.19 cluster, each statement in a new
session. The gap tests are in `tests/backlog/test_rp_found_rp573.py`. Since
RP-573 a DECLARE's type is looked up at CREATE FUNCTION and before a DO
block runs (`check_plpgsql_declarations`), from the type text libpg_query
keeps; what follows is what that lookup does not yet check.


## Found while working RP-458 (2026-11-08)

Measured on PostgreSQL 15.19; none needs an object.


## Found while working RP-697 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp697.py`: `rn697(k int, a int, c text)` and
`ch697(k int, c char(3), v varchar(4))`.


## Found while working RP-726 (2026-11-09)

Measured on a private PostgreSQL 15.19 cluster with the `_SETUP` in
`tests/backlog/test_rp_found_rp726.py`: `ch726(k int, c char(3), v
varchar(4), t text)` holding `(1, 'y', 'y', 'y')` and `(2, 'ab', 'ab ',
'ab ')`.


## Found while working RP-732 (2026-11-09)

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written. The gap tests are in
`tests/backlog/test_rp_found_rp732.py`.


## Found while working RP-782 (2026-11-10)

Measured on a private PostgreSQL 15.19 cluster (locale C.UTF-8, built
with ICU) with the `_SETUP` in `tests/backlog/test_rp_found_rp782.py`:
`xu782f` a domain over `text` `collate "und-x-icu"`, `xc782f` one
`collate "C"`.

- [ ] **`pg_get_viewdef` of most system views is NULL.** PostgreSQL
      15.19 deparses every `pg_catalog` and `information_schema` view's
      rule. Here `system_view_defs` prints `system_views.sql`'s queries
      through `ruleutils`, which reproduces 38 of the 75 `pg_catalog`
      views exactly (`pg_tables`, `pg_roles`, `pg_stats`, `pg_settings`,
      `pg_locks`, ...; none printed wrong, measured 2026-11-15) and
      leaves the rest NULL rather than print them as written. Remaining:
      the 37 other `pg_catalog` views (`pg_sequences`, `pg_policies`,
      `pg_seclabels`, `pg_publication_tables`, `pg_stats_ext*`, and every
      `pg_stat_*` / `pg_statio_*` view), which the printer refuses; and
      every `information_schema` view, left out because the printer casts
      to the bare domain where ruleutils qualifies it and casts a literal
      to the base type first
      (`('c'::character varying)::information_schema.character_data`,
      `(x)::information_schema.sql_identifier`). A function in FROM
      returning a record of OUT parameters prints since 2026-11-15.
      Verify: tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_the_remaining_system_views
      Backlog-ID: RP-798

## Found while working RP-816 (2026-11-11)

Measured on a private PostgreSQL 15.19 cluster.


## Found while working RP-860 (2026-11-14)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp860.py` (`t860 (x int)`).


## Found while working RP-939 (2026-11-17)

Measured on PostgreSQL 15.19 (the shared reference on port 5415).

- [ ] **A renamed partition key column leaves the key naming the old
      column, and the table takes no more rows.** After `alter table p939k
      rename column x to y` on `p939k (x int) partition by list (x)`,
      PostgreSQL 15.19 still routes `insert into p939k values (1)` and
      prints the key `LIST (y)`: the key holds the column's attnum. Here
      the stored key text still names `x`, so the INSERT is 42703 `column
      "x" does not exist`. The RENAME COLUMN arm renames the column in
      stored CHECKs (`rename_column_in_expression`, RP-939); the partition
      key, the partitions' bounds, and any other stored expression text
      (defaults, generated columns, index expressions and predicates) were
      not checked.
      Verify: tests/backlog/test_rp_found_rp860.py::test_a_renamed_partition_key_column_still_routes
      Backlog-ID: RP-1024

## Found while working RP-751 (2026-11-10)

Measured on PostgreSQL 15.19; none of it needs DDL. The gap tests are in
`tests/backlog/test_rp_found_rp751.py`.


## Found while working RP-833 (2026-11-15)

Measured on PostgreSQL 15.19 (the shared reference on port 5415). The gap
test is in `tests/backlog/test_rp_found_rp833.py`.


## Found while working RP-1018 (2026-11-17)

Measured on PostgreSQL 15.19 (the shared reference on port 5415). The gap
test is in `tests/backlog/test_rp_found_rp833.py`.

- [ ] **A function's CONTEXT label follows the search path of each call,
      not of its first.** `plpgsql_compile` labels a function once, when the
      session first compiles it, and keeps that label while the compiled
      function is cached: on PostgreSQL 15.19, `s.f()` called first with
      `s` off the path is `PL/pgSQL function s.f()`, and still is after
      `set search_path = s`. Here `function_label` runs on every call, so
      the second is `f()`. Faithful: label at the session's first compile,
      kept until the function is replaced or dropped.
      Verify: tests/backlog/test_rp_found_rp833.py::test_a_function_label_is_fixed_at_its_first_compile
      Backlog-ID: RP-1034

## Found while working RP-689 (2026-11-10)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp689.py`: `t689(a d689, v
varchar(3), w dv689, c char(3))`, `d689` an `int` with `check (value > 0)`.



## Found while working RP-845 (2026-11-11)

Measured on PostgreSQL 15.19 (the shared reference on port 5415) with the
`_SETUP` in `tests/backlog/test_rp_found_rp839.py` and `"Over845"` a domain
over its `"Mx839"`.


## Found while working RP-692 (2026-11-10)

Measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with the
`_SETUP` in `tests/backlog/test_rp_found_rp692.py`: `t692j(x numeric, k
int)` holding `(1.0, 1)`, and an empty `t692e(x numeric, k int)`. Since
RP-692 an INTERSECT or EXCEPT runs the plan PostgreSQL makes for it
(`setop_order.rs`): the input with fewer estimated groups first, hashed or
sorted as `choose_hashed_setop` prices them, each group's row the first
its first input gave.


## Found while working RP-727 (2026-11-10)

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written. The gap tests are in
`tests/backlog/test_rp_found_rp727.py`.


## Found while working RP-849 (2026-11-11)

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written. The gap test is in
`tests/backlog/test_rp_found_rp849.py`.



## Found while working RP-423 (2026-11-17)

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written. The gap test is in
`tests/backlog/test_rp_found_rp423.py`.

- [ ] **`clamp_row_est` rounds a half away from zero.** PostgreSQL 15.19's
      `clamp_row_est` is `rint`, ties to even: `select max(x) from hv group
      by k having max(x) <> 5` over 300 groups (`hv(x numeric, k int)`,
      3,000 rows, `k = g % 300`, analysed) is `HashAggregate (... rows=298
      ...)`, 300 x 0.995 = 298.5. `costsize::clamp_row_est` uses
      `f64::round`, which makes it 299; every estimate that lands on an
      exact half with an even integer part is one row high. Measured on
      the 15.19 reference on port 5415.
      Verify: none -- no EXPLAIN this server prints reaches an exact half yet (its HashAggregate over a HAVING prints zeros); `setop_order::tests::having_selectivity_is_postgres` notes the case
      Backlog-ID: RP-1023

## Found while working RP-966 (2026-11-17)

- [ ] **A nested loop whose inner relation has its own WHERE conjunct is
      not priced.** `explain select e.b from e join k on e.a <> k.a where
      k.b = 'v1'` (`e`, `k` 1,000 analysed rows) is, on PostgreSQL 15.19,
      `Nested Loop (cost=0.00..1532.75 rows=99900 width=3)` over `Seq Scan
      on e` and a `Materialize (cost=0.00..18.00 rows=100 width=4)` of
      `Seq Scan on k (cost=0.00..17.50 rows=100 width=4)`; here zeros and
      no Materialize. With the conjunct on the OUTER table PostgreSQL
      swaps the sides and this server matches it (found while working
      RP-966).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a nested loop over a filtered inner relation is priced]
      Backlog-ID: RP-1038

- [ ] **A computed ORDER BY key written through an alias prints its
      synthetic name.** `explain select a from k x order by x.a*2` prints
      `Sort Key: __order0`; PostgreSQL 15.19 prints `Sort Key: ((a * 2))`
      (the unaliased `order by a*2` already does). The plain plan's key
      text is not deparsed against the alias's range-table entry.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a computed sort key through an alias prints its expression]
      Backlog-ID: RP-1035

- [ ] **A Sort on a computed key is priced in one shape only.** Since
      2026-11-17 (RP-966) a Sort straight over one relation's scan whose
      computed keys are operator expressions over the select list's
      columns is priced, each key a resjunk entry of the scan's target
      list (its operators' `cpu_operator_cost` per row, its type's width).
      Still `cost=0.00..0.00`: a select list with a computed entry (`select
      a+1 from k order by a+1`, PostgreSQL 15.19 `Sort (cost=67.33..69.83
      rows=1000 width=4)` over `Seq Scan (cost=0.00..17.50 ...)`), a LIMIT
      above the Sort, a key that calls a function (`order by lower(b)`,
      `width=36`), and a key reading a column the select list does not
      return. `sort_key_cost` and the Sort pricing in `lib.rs` (RP-318)
      decline them.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a sort on a computed key under a computed select list is priced]
      Backlog-ID: RP-1036

- [ ] **A join on an expression key PostgreSQL prices is unpriced in
      three shapes.** Since RP-981 a join of two analysed tables on one
      equality between a column and an operator expression over one column
      of the other is a priced Hash Join (inner, LEFT, anti), hashing the
      side whose key has statistics. Still a `Nested Loop` with `Join
      Filter` and zero costs: against a unique column, `select * from e join
      k on k.a = e.a - 1` (PostgreSQL 15.19 `Hash Join (cost=27.50..45.26
      rows=1000 width=14)` hashing `k`, inner unique; `hash_join_of`'s
      unique-key paths are not modelled over an expression); where the
      merge wins, `e.b = e2.b || 'x'` over a ten-value column (`Merge Join
      (cost=129.66..224.66 rows=5000 width=14)`, the expression's Sort Key
      `((e2.b || 'x'::text))`; priced with `mergejoinscansel`'s defaults
      and declined, the plan is not drawn); and across types, `e.a::bigint =
      e2.a` (`Hash Join (cost=27.50..57.50 rows=1000 width=14)`; declined on
      `pg_type`). An aggregate over such a join (`select count(*)`) and a
      LEFT join filtered on a column not its key (`where e2.b is null`, a
      `Hash Right Join` with `Filter`) stay unpriced as they do over a
      column key. Measured on the 15.19 reference on port 5415.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[an expression key against a unique column is a hash join], tests/backlog/test_rp_explain_costs.py::test_plan[an expression key over a column of few values is a merge join]
      Backlog-ID: RP-1032
- [ ] **ANALYZE of a hash join on an expression key counts nothing below
      it.** `explain (analyze, costs off, timing off, summary off) select *
      from e left join e e2 on e.a = e2.a + 1 where e2.a is null` prints,
      in PostgreSQL 15.19, `actual rows` on both scans and the Hash and the
      Hash's `Buckets: 1024  Batches: 1  Memory Usage: 47kB`; here only the
      join's own `actual rows=1`. `join_analyze` reads the `Hash Cond` as
      two columns. Measured on the 15.19 reference on port 5415 (found
      while working RP-981).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an anti join on an expression key]
      Backlog-ID: RP-1033
- [ ] **A hash join written inner side first is not `Inner Unique`.**
      `explain verbose select * from k join e on e.a = k.a` (`k.a` the
      primary key, `k` hashed) prints `Inner Unique: true` in PostgreSQL
      15.19, as the same join written `e join k` does; here `false`, in
      the structured formats without VERBOSE too (`"Inner Unique":
      false`). The `unique_probe` of the join's inner table is taken from
      the side the FROM writes second, not the side the plan hashes.
      Measured on the 15.19 reference on port 5415.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a hash join written inner side first is inner unique]
      Backlog-ID: RP-1030
- [ ] **A structured EXPLAIN VERBOSE names no `Schema` on a scan.**
      `explain (verbose, costs off, format json) select * from e` prints
      `"Relation Name": "e", "Schema": "public", "Alias": "e"` in
      PostgreSQL 15.19 (`ExplainScanTarget` adds the schema under
      VERBOSE); here no `Schema` key, on every scan of every structured
      format. Measured on the 15.19 reference on port 5415.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a structured verbose scan names its schema]
      Backlog-ID: RP-1031
- [ ] **A Limit over a Sort of a filtered scan is not priced.**
      PostgreSQL 15.19 prints `explain select a from k where a > 5 order by
      b desc limit 3` (`k` analysed, 1,000 rows) as `Limit
      (cost=30.36..30.37 rows=3 width=7)` over `Sort (cost=30.36..32.85
      rows=995 width=7)` (a top-N heapsort, `cost_sort` with its bound)
      over `Seq Scan on k (cost=0.00..17.50 rows=995 width=7)`; here every
      node is `cost=0.00..0.00 rows=0 width=0`, with or without VERBOSE,
      and with `select *` too. The same Sort without the LIMIT is priced.
      Measured on the 15.19 reference on port 5415.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a limit over a descending sort]
      Backlog-ID: RP-1025

## Found while working RP-980 (2026-11-17)

- [ ] **An anti join with an outer conjunct beside it is not planned as
      one.** `select b from e where a > 500 and not exists (select 1 from e
      e2 where e2.a = e.a)`, and its LEFT join spelling `select e.b from e
      left join e e2 on e.a = e2.a where e.a > 500 and e2.a is null`, are
      in PostgreSQL 15.19 both `Hash Anti Join (cost=27.50..46.88 rows=1
      width=3)` with `Filter: (a > 500)` on the outer Seq Scan (`rows=500`).
      Here the LEFT join form prints an unpriced `Hash Left Join` with
      `Filter: (e2.a IS NULL)` (RP-908's `left_anti_key` recognises the
      anti join only where the WHERE is the IS NULL alone), and the NOT
      EXISTS form a Seq Scan with `Filter: (...)`:
      `sublink_join::anti_join_of` (RP-980) rewrites NOT EXISTS into that
      LEFT join form only when it is the whole WHERE, and should take the
      outer conjuncts once the LEFT join form prices them. Measured on the
      15.19 reference on port 5415.
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a not exists beside an outer conjunct is a hash anti join], tests/backlog/test_rp_explain_costs.py::test_plan[a left join anti join beside an outer conjunct is a hash anti join]
      Backlog-ID: RP-1027

## Found while working RP-981 (2026-11-17)

- [ ] **EXPLAIN names a temporary table's scan by its schema and repeats
      it as an alias.** PostgreSQL 15.19 prints `explain select * from t`
      (`t` a temporary table) as `Seq Scan on t (cost=0.00..35.50
      rows=2550 width=4)`, and `... from t x` as `Seq Scan on t x`; here
      `Seq Scan on pg_temp_22.t t` and `Seq Scan on pg_temp_22.t x`: the
      scan's relation name is the temp schema's storage key, which the
      join-leaf alias logic then sees as differing from the alias. Only
      VERBOSE prints a schema (`pg_temp_3.t`). Measured on the 15.19
      reference on port 5415. The same for any schema but `public` on the
      search path: with `set search_path = q1`, `Seq Scan on q1.t t` where
      PostgreSQL prints `Seq Scan on t`, in a join's scans too (found while
      working RP-978, 2026-11-18).
      Verify: tests/backlog/test_rp_explain_costs.py::test_plan[a scan of a temporary table names it bare], tests/backlog/test_rp_explain_costs.py::test_plan[a scan of an aliased temporary table names it and its alias], tests/backlog/test_rp_explain_costs.py::test_plan[a scan of a table in a schema on the search path names it bare]
      Backlog-ID: RP-1029

## Found while working RP-971 (2026-11-17)

- [ ] **A `pg_temp` name before the session has a temp namespace answers
      "does not exist" for the object, not the schema.** In a fresh
      session PostgreSQL 15.19 answers `select 'pg_temp.zz'::regclass` and
      `select 'pg_temp.zz'::regtype` with 3F000 `schema "pg_temp" does not
      exist` (`LookupExplicitNamespace` finds no `myTempNamespace`);
      `secantusd-pg` answers 42P01 `relation "pg_temp.zz" does not exist`
      and 42704 `type "pg_temp.zz" does not exist`. A function call
      already answers 3F000 (`test_a_pg_temp_function_left_by_a_killed_server_is_gone_at_restart`).
      Verify: tests/backlog/test_rp_qualified_calls.py::test_pg_temp_name_without_a_temp_namespace_is_an_undefined_schema
      Backlog-ID: RP-1037
