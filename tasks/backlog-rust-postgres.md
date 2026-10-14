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

- [ ] **RUST pgserver: `SET TRANSACTION` outside an explicit block sends no
  WARNING.** It is a silent no-op here (correct), where PostgreSQL also emits
  `WARNING: SET TRANSACTION can only be used in transaction blocks`.
  psycopg never triggers it (it only adds the modes to `BEGIN`), and the
  `Statement::SetTransaction` arm in `crates/secantus-pg/src/lib.rs` has no
  notice-emission seam on that path. Isolation itself is accepted and
  reported, not enforced -- a scope decision (single node, one WiredTiger
  snapshot level), not a deferral.
  Backlog-ID: RP-001
  Verify: tests/backlog/test_rp_triage.py::test_set_transaction_outside_a_block_warns
- **Scope decision — psycopg's `test_generators.py::test_cancel` and the
  `proxy`-marked `test_cancel_safe_error` / `test_cancel_safe_timeout` fail on
  macOS in the CLIENT.** macOS libpq sends the cancel on a not-yet-connected
  non-blocking socket and waits forever; native PostgreSQL 15 fails the same
  call sequence (7 of 10 rounds), and `pproxy` cannot start under this
  venv's uvloop on Python 3.12. `psycopg_validation/include_paths.py` keeps
  them out of the macOS gauge. Nothing to do server-side.
- [ ] RUST pgserver: a cancel that arrives during Parse / Bind / Describe is kept
  (fixed 2026-10-09), but two differences from PostgreSQL remain. It is
  honoured at the first cancellation point of EXECUTION, not during the parse
  itself: against a statement that takes 2.8 s to parse here, PostgreSQL 15
  answers 57014 in 30 ms and this server answers it after the parse. And the
  idle window is wider: a cancel that lands while the server is still READING
  a large message is dropped (0-5 ms after a 1.4 MB statement in a debug
  build), where PostgreSQL has finished reading by then. Closing the second
  needs a hook in the vendored `secantus-pgwire` at the point a message's
  first byte is read. Probe: send `select pg_sleep(3), 1 in (0..200000)` with
  `send_query_params`, then a blocking `PQcancel` at a chosen offset.
  Backlog-ID: RP-002
  Verify: tests/backlog/test_rp_triage.py::test_cancel_during_parse_is_honoured_at_once
- [ ] RUST pgserver: `standard_conforming_strings = off` is honoured by a lexer
  pass (`secantus-pgplan/src/escape_strings.rs`) that rewrites each plain
  literal to the `E'...'` it means before pg_query (which has the setting
  hard-wired on) sees it, with the `escape_string_warning` notices. Left:
  `N'...'` literals are not rewritten (they keep the conforming reading), and
  `backslash_quote` is not a setting (2026-09-09).
  Backlog-ID: RP-003
  Verify: tests/backlog/test_rp_triage.py::test_national_literal_under_nonconforming_strings,tests/backlog/test_rp_triage.py::test_backslash_quote_off_refuses_backslash_quote
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
- [ ] **RUST pgserver: EXECUTE on a BUILT-IN function is not checked in
  every place.** A `REVOKE EXECUTE ON FUNCTION abs(int) FROM PUBLIC` is
  checked at the call by the overload `func_select_candidate` picks (corpus
  `b45_builtin_execute`), but not for a call whose overload the type analysis
  cannot determine (an argument of an unanalysed shape, a polymorphic
  overload), and not for a call inside a view, a rule, a user function's body
  or a DEFAULT expression. Reproduce: revoke EXECUTE on `abs(int)`, then call
  `abs(-1)` from a view or a column DEFAULT as the restricted role; PostgreSQL
  15 refuses with 42501.
  Backlog-ID: RP-004
  Verify: tests/backlog/test_rp_triage.py::test_revoked_builtin_refused_through_view_and_default
- **Scope decision — pgjdbc `BlobTransactionTest`** needs `CREATE FUNCTION
  lo_manage() ... LANGUAGE C` (the `lo` contrib trigger). `LANGUAGE C` loads
  a shared library into the server process, which a server written in Rust
  has no ABI to host; the `lo` contrib module is not shipped.
- **Scope decision — direct writes to a catalog** (an UPDATE that changes a
  catalog row, a DELETE that matches one, an INSERT into a catalog) are
  0A000: the catalogs are computed from the store, so there is no row to
  change. An UPDATE leaving rows as they are reports them, as pgjdbc's
  updatable-result-set write-back needs.
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
- [ ] **RUST pgserver: `pg_collation` lists only `und-x-icu` / `en-x-icu`
  among the ICU collations.** The list was limited because ordering by a
  locale once needed CLDR data the server did not have. `secantus-pgplan/src/
  collation.rs` now evaluates any `<tag>-x-icu` name with ICU4X over CLDR
  data, but `regobj::collation_names` and `secantus-pg/src/collations.rs`
  still list only `und-x-icu` (12713) and `en-x-icu` (12860). Reproduce:
  `select collname from pg_collation where collprovider = 'i'` on both
  servers; PostgreSQL 15 lists hundreds (`de-x-icu`, `sv-x-icu`, ...). Check
  first that each listed locale orders as PostgreSQL does.
  Backlog-ID: RP-005
  Verify: tests/backlog/test_rp_triage.py::test_pg_collation_lists_locale_icu_collations
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

      Correctness items found on the way, open:
      Backlog-ID: RP-007
      Verify: none -- Linux throughput/CPU-per-statement benchmark campaign
      - [ ] **RUST PG server: a parenthesised step inside `ROLLUP` / `CUBE`
            is refused with a wrong 42803.** `group by rollup ((g, z), v)`
            answers `column "gse_t.g" must appear in the GROUP BY clause`.
            `GroupElement::Rollup` holds single keys; a step of several
            needs a list of lists. (`GROUP BY (a, b)` on its own works.)
            `tools/probes/pg_corpora/grouping_set_exprs.sql`.
            Backlog-ID: RP-008
            Verify: tests/backlog/test_rust_postgres_gaps.py::test_rollup_with_a_parenthesised_step
      - [ ] **RUST PG server: a grouped query's subquery cannot name the
            outer table by its own name.** `select g, (select max(v) from t i
            where i.g = t.g) from t group by g` is `42P01 invalid reference
            to FROM-clause entry for table "t"`; with an alias on the outer
            table (`from t o ... o.g`) it works. Not a grouping-set matter:
            the plain GROUP BY above fails the same way.
            Backlog-ID: RP-009
            Verify: tests/backlog/test_rust_postgres_gaps.py::test_grouped_query_subquery_names_outer_table
      - [ ] **RUST PG server: a 42803 raised from HAVING names the column
            without its table.** PostgreSQL writes `column "t.z" must
            appear...`; the HAVING path writes `"z"` (`having_subject` has
            no statement to take the qualifier from).
            Backlog-ID: RP-010
            Verify: tests/backlog/test_rust_postgres_gaps.py::test_having_42803_names_the_table
      - [ ] **RUST PG server: with `public` off the `search_path`, a
            relation of `public` is still found from a STRING and by
            `DROP ... IF EXISTS`.** `select nextval('s')` and
            `'t'::regclass` answer where PostgreSQL 15 says 42P01, and
            `drop table if exists t` DROPS the table where PostgreSQL
            skips it with a notice. The check is on the statement's
            relation references (`schemas::hidden_in_public`); these
            three resolve the name elsewhere.
            `tools/probes/pg_corpora/search_path_public.sql`.
            Backlog-ID: RP-011
            Verify: tests/backlog/test_rust_postgres_gaps.py::test_public_off_path_not_found_from_a_string,tests/backlog/test_rust_postgres_gaps.py::test_public_off_path_drop_if_exists_skips
      - [ ] **RUST PG server: a syntax error from Parse (the extended
            protocol) carries no position.** `selct 1` sent with
            `prepare=True` has `P` unset; sent as a simple query it is 1.
            Every syntax error, not only the unterminated ones.
            Backlog-ID: RP-012
            Verify: tests/backlog/test_rust_postgres_gaps.py::test_parse_syntax_error_carries_position

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
  `--refuse-unknown-users`, `--hba-file`, revocation lists, user name maps,
  `--tls-client-name DN` and reload are in (`crates/secantus-pg/src/tls.rs`,
  `scram_binding.rs`, `hba.rs`, `crl.rs`, `ident.rs`, `live.rs`). The three
  login comparisons against PostgreSQL 15 (`tools/probes/pg_login_options.py`,
  `pg_login_tls_files.py`, `pg_login_hba.py`) run in
  `tests/test_rust_pg_login_differential.py` (CI's `pg-oracle` lane; locally
  only with `SECANTUS_PG15_BIN` set). Open:
  - **A user this server has never heard of is trusted BY DEFAULT**;
    PostgreSQL asks it for a password. Test fixtures connect as roles
    nobody created, so the default is a decision; `--refuse-unknown-users`
    answers as PostgreSQL does. Without a `pg_hba.conf` a role with no
    password is trusted, flag or not; with one a `scram-sha-256` line
    refuses it.
  - **With `--hba-file`:**
    - **Refused at startup, by line number**: the methods that need an
      outside service (`peer`, `ident`, `gss`, `sspi`, `ldap`, `radius`,
      `pam`, `bsd`), a host NAME for the address, `samenet`, `@file`
      inclusion, and `include` directives (PostgreSQL 16's).
    - **`samehost` matches loopback only**: the handler is not told the
      address a connection arrived on.
    - **`+role` and `samerole` go through `pg_has_role`'s membership**,
      where PostgreSQL uses the form that does not count a superuser as a
      member of everything. Not probed with a superuser that has no line.
    - **A `map=` on a method other than `cert` is refused**; PostgreSQL
      takes it for `ident`, `peer`, `gss` and `sspi` too, none of which is
      read here.
    - **Without a `pg_hba.conf` the database is still looked up before the
      login is proved**, so a wrong password for a database that is not
      there reads "database does not exist". With one the order is
      PostgreSQL's.
    - A password past its `VALID UNTIL` runs the exchange and fails it,
      where the path without a file fails at once. PostgreSQL does the
      first; not probed here.
  - **TLS files:**
    - **A revocation list with a critical extension is refused at
      startup** (`crl.rs`): one scoped by an issuing distribution point, a
      delta list, an indirect list. OpenSSL handles the first.
    - **A user name map's patterns are the `regex` crate's**, not
      PostgreSQL's advanced regular expressions; `@file` inclusion in
      `pg_ident.conf` is refused.
    - **`pg_reload_conf()` checks superuser as of login**, not the role
      `SET ROLE` chose, and a GRANT of EXECUTE on it is not honoured.
    - **SIGHUP reloads only on a server started with TLS**; without TLS it
      still stops the server (`ctrlc`'s termination handler).
  - **`pg_stat_ssl` prints `UNDEF` for an attribute type outside
    `x509::short_name`'s table**, where OpenSSL knows more names.
  - **Direct TLS (`sslnegotiation=direct`) is accepted**, which is
    PostgreSQL 17's behaviour; 15.19 answers "SSL error: unexpected eof".
  - The key must have no passphrase.
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
  - The embedded Python handle (`_secantus_server.PgServer`) **does not
    take a revocation list, a user name map or `clientname=DN` as
    options**; a `pg_hba.conf` passed as `hba_file` can say the last two.
  Backlog-ID: RP-013
  Verify: tests/backlog/test_rust_postgres_gaps.py::test_unknown_user_is_asked_for_a_password,tests/backlog/test_rust_postgres_gaps.py::test_wrong_password_checked_before_the_database

## 5. Known bugs and edge cases to watch

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

- [ ] **OPEN — RUST pgserver: what `pg_builtin_coverage.py` still reports
      (2026-10-14).** The probe calls every immutable or stable function in
      PostgreSQL 15.19's `pg_proc` on both servers with one sample value
      for each argument type. Of the 1,935 it can call, 1,503 answered the
      same on 2026-10-14 (what that day's batches fixed is recorded with
      PostgreSQL 15.19's answers in `pg_corpora/builtin_coverage.sql`). 325
      more read the server's own state and are not called, and 722 take a
      type the probe has no sample for (`internal`, `cstring`, `record`,
      `aclitem`, `regproc`, `xml`, `refcursor` and others), so nothing here
      is said of them. Re-run it before working an item: the lists below
      are from that day.

      What is left:
      - **Encodings other than UTF8, LATIN1 and SQL_ASCII convert only
        their ASCII range**; anything above it is 0A000. A conversion
        table per encoding is what PostgreSQL has.
      - **`age(xid)` of a normal transaction id is 0A000.** This server's
        xids are not PostgreSQL's 32-bit counter, so there is nothing to
        measure the age against; `age(datfrozenxid)` in a monitoring
        query fails. The special xids 0-2 answer INT_MAX, as in
        PostgreSQL.
      - **A set-returning function inside an expression**
        (`pg_typeof(json_each(j))`), and two of them in one select list,
        are refused.
      - **`ts_parse`, `ts_debug` and `ts_rewrite` are refused.** The first
        two need `wparser_def.c`'s parser, which reports 23 token types
        where this server's tokenizer tells two apart; `ts_rewrite` needs
        `tsquery_rewrite.c`.
      - **`aclexplode`, `acldefault` and `makeaclitem`** need role oids;
        **`pg_relation_is_updatable`**, **`pg_is_other_temp_schema`** and
        **`pg_settings_get_flags`** need catalog or GUC state the planner
        does not see; `table_to_xml` and the other XML exports, and the
        `fmgr_*` / `postgresql_fdw_validator` validators, are not
        written.
      - **The `tid` type does not exist here**, so its six comparisons and
        `min` / `max` over it are 42704.
      - **`ts_headline` refuses `MaxWords`, `MinWords`, `ShortWord`,
        `MaxFragments` above 0 and a true `HighlightAll`** (0A000, by
        name). The selection of the words shown is not `mark_hl_words`:
        a document over 35 words is cut by a rule of this server's own,
        and has not been compared with PostgreSQL.
      - **A column of a composite type cannot be taken apart**: `(v).*`
        and `(v).f` are 0A000 `this field selection is not supported
        yet`.
      - **Only `line <-> point` measures with `pg_hypot`.** Every other
        distance in `geom.rs` uses Rust's `f64::hypot`, which can differ
        from PostgreSQL in the last digit; no corpus line has shown one.
      - **A `"char"` with the high bit set is carried as its `\ooo` text**,
        so it orders by that text and not by the byte. Not measured.
      - **An alias call with a COLUMN of the wrong type still runs**
        (`int4pl(numeric_col, 2)`): the rewrite sees no column types.
      - **`pg_relation_is_publishable` is true for an unlogged table**:
        this server keeps no persistence apart from temporary.
      - **`65::int2::"char"` answers `A`**; PostgreSQL has no `int2` cast to
        `"char"` (42846). `"char"(x)` is planned as a cast, and as a call
        PostgreSQL promotes the `int2`, so the two cannot be told apart
        until `"char"` is a function here. `"char"(65::int8)` is 42846
        where PostgreSQL says 42883.

      **Refused OPERATORS the probe reached through their functions** (82
      signatures; each is `0A000` or `operator does not exist`):
      - multirange `-|-`, `<<`, `>>`, `&<`, `&>` and its ordering
        comparisons (`<`, `<=`, `>`, `>=`), with a multirange or a range
        on either side;
      - `macaddr` and `macaddr8` `&`, `|`, `~`;
      - the text pattern comparisons `~<~`, `~<=~`, `~>=~`, `~>~`;
      - `date + timetz` and `timetz + date`;
      - geometric: `box # box`, `box <^ box`, `box >^ box`, circle
        `&<` / `&>` / `&<|` / `|&>`, the closest-point operator `##` for
        six pairs of types, `<->` between a line or a box and a segment,
        `line # line`, `line ?# box`, `lseg ?# line`, the `path` count
        comparisons, `point <> point`;
      - `tsquery @> tsquery` and `<@` by their function names;
      - `json[b]_extract_path[_text]` and `jsonb_delete` given their
        path as `VARIADIC array`.

      **Casts by function name**: `int4("char")` and the length-coercion
      forms (`bpchar(bpchar, int4, bool)`, `numeric(numeric, int4)`,
      `timestamp(timestamp, int4)` and the like) are refused or wrong.

      **Not to be closed by an alias**: `bttextcmp`, `bpcharcmp` and
      `byteacmp` answer what the C library's `memcmp` returns, which is
      14 for `bttextcmp('b', 'a')` on macOS and 1 elsewhere; the type
      input and output functions (117), the index support functions (102)
      and the aggregates' own transition functions (57) take or return
      `cstring` and `internal` and are not called by a statement.

      **Error texts** (28): PostgreSQL 15 reworded `date_part` /
      `date_trunc` / `extract` of an unknown unit to `unit "x" not
      recognized for type timestamp with time zone`; this server still
      says 14's `timestamp with time zone units "x" not recognized`. The
      corpora that pin those lines are measured against the PostgreSQL 14
      reference, so changing the text moves them to
      `# reference-version: 15` in the same change. `date_part` and
      `extract` of a `timetz` with an unknown unit are 22007 here, a
      different error. A `jsonb` object or array cast to a number or a
      boolean is 22P02 `invalid input syntax` here and 22023 `cannot cast
      jsonb object to type integer` there. `ts_headline` with options
      that do not parse answers where PostgreSQL refuses.
      Backlog-ID: RP-015
      Verify: tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_residue,tests/backlog/test_rust_postgres_gaps.py::test_composite_column_star_expansion

- [ ] **OPEN — a backend-cancel test asserts a fixed 1 s bound on a shared
      runner.** `tests/test_rust_pgserver_slice.py::test_pg_cancel_and_terminate_backend_signal_a_running_statement`
      asserts `time.monotonic() - t0 < 1.0` for a cancelled / terminated
      `pg_sleep(5)`. On 2026-10-10 the cancel half measured 1.06 s in the
      `pg-oracle` lane, on a PR that changed only the MongoDB server (#1850 or
      #1851; which was not written down), and passed on the re-run. Same shape
      as the reap race that test used to have (fixed: the idle-session reap
      now polls `pg_stat_activity` with a 10 s deadline): a fixed bound
      standing in for "the cancel arrived".

      Practical note: `gh run rerun --failed` is NOT available on this
      workflow (it answers "run ... cannot be rerun; its workflow file may be
      broken"), so a single-lane re-run is not a diagnostic you can reach
      for. Re-triggering means a push, and therefore a full matrix.
      Backlog-ID: RP-016
      Verify: none -- a flaky fixed-time bound in a repo test on a shared CI runner

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
      its table leaves.** A WHERE conjunct that reads ONE alias's columns
      (qualified), built of operators, casts, constants and parameters, is
      applied inside that leaf (only on the PRESERVED side of every outer
      join above it); corpus `join_pushdown`. Not pushed: a conjunct with an
      unqualified column, a function call (it may be volatile), a subquery,
      or one over two aliases. Each such conjunct is filtered only after the
      join, so the leaf cannot use an index for it.
      Backlog-ID: RP-019
      Verify: none -- predicate pushdown/index use only; answers identical, EXPLAIN costs are a scope decision

## Warning-class RUSTSEC advisories (surfaced by cargo-audit, not gated) (2026-07-20)

The `cargo-audit` CI gate (`.github/workflows/cargo-audit.yml`) fails only on
*vulnerability*-class advisories; this warning-class one is printed in the
gate log every run but does not block. Not a reachable exploit; clear it when
the TLS dependency chain is next touched:

- **RUSTSEC-2025-0134** — `rustls-pemfile` 2.2.0 unmaintained. Its API folded
  into `rustls-pki-types`. A direct dependency of `secantus-pg` and
  `secantus-pgwire`.
