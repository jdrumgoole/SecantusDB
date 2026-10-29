---
name: rust-pg-server
description: Working on the Rust PostgreSQL server (`secantusd-pg`; crates `secantus-pg`, `secantus-pgplan`, `secantus-pgcatalog`, `secantus-pgwire`). Fires when editing or reviewing those crates, working an `RP-nnn` item in `tasks/backlog-rust-postgres.md`, running a `pg_corpora` corpus or `pg_builtin_coverage.py`, running the psycopg gauge against the Rust server, or answering "does the Rust PG server support X?". Holds how to build, test and measure it, the reference servers and how to compare against them, where each kind of change goes in the planner and executor, and the rules earlier batches paid for (type-driven overload and operator resolution, char(n) padding, FMA on arm64, joined-row keys, side effects in planning, user code as its own transaction, catalog fields both servers must preserve).
---

# The Rust PostgreSQL server

`secantusd-pg` speaks the PostgreSQL wire protocol over the same
`secantus-storage` the Rust MongoDB server uses. SQL is parsed by
`libpg_query` (`pg_query`, the real PostgreSQL grammar), lowered by
`secantus-pgplan` to MQL filters `secantus-core` evaluates, against the
catalog in `secantus-pgcatalog`. **Its exemplar is PostgreSQL, never the
Python PG server** (CLAUDE.md, "Design constraints"), and when PostgreSQL
answers from state this server does not keep, build PostgreSQL's mechanism
(`xids.rs` is the model case) or refuse by name with `0A000`.

One session works on ONE Rust server (CLAUDE.md). Changes the item needs in
`secantus-storage` are fine; say in the PR that the MongoDB server links it.

## Before believing anything about it

- **The backlog, CLAUDE.md and this skill all lag the code.** The
  "what remains refused" lists have been wrong in both directions. Probe
  first: run the statement against `secantusd-pg` and PostgreSQL.
- **Confirm the binary matches the tree before quoting any number.**
  `secantusd-pg --version` prints the `crates/` tree it was built from;
  compare `git rev-parse HEAD:crates`. `tools/provenance.py` aborts the
  gauge runner and the probe launchers on a stale artifact; anything you
  launch by hand is on you. A gauge that finishes far faster than usual
  stopped reaching the code -- treat it as stale until proven otherwise.

## Build and test

It links WiredTiger and is excluded from the clean workspace, so it builds
**from its own directory**:

| what | command |
| --- | --- |
| debug build (what the gauge and probes expect) | `./inv rust-pgserver-build` |
| fmt + clippy `-D warnings` + `cargo test` | `./inv rust-pgserver-test` |
| one Rust test | `./inv rust-test-one --crate secantus-pg --test <file stem> --name <test>` |
| release build | `cd crates/secantus-pg && cargo build --release` (fat LTO, ~9 min) |

Set `CARGO_BUILD_JOBS=3` (machine budget, CLAUDE.md). `cargo fmt` / `clippy`
run from `crates/` skip this crate: run them here.

The released binary also gets a PGO profile, collected at release time by
`scripts/build_pg_release.sh` from `bench/pgo_workload_pg.py`; nothing is
committed for it. On macOS this server runs WiredTiger with `method=dsync`,
which is what `cmake/patch_wt_dsync_group.py` changes.

**Versioning:** `secantus-pgcatalog` / `secantus-pgplan` / `secantus-pg`
carry their OWN version line, separate from the thirteen MongoDB-side
crates. Never fold them into a MongoDB-server bump, and no feature PR bumps
either line.

## The reference servers

| server | where | used for |
| --- | --- | --- |
| PostgreSQL 15.19 | port 5415 (`brew install postgresql@15`), `SECANTUS_PG_ORACLE_DSN_15` | the version this server reports (15.0); gap tests, `pg_builtin_coverage.py` |
| PostgreSQL 14 | the live default cluster | older corpora |

- A corpus asks for 15 with `# reference-version: 15` and for a UTF-8
  locale with `# reference-locale: C.UTF-8` (`tools/probes/pg_differential.py`).
  The box's 14 cluster runs locale `C`, which maps no non-ASCII letter; the
  server reports `C.UTF-8`, so case and character-class corpora need the
  directive.
- **Never change the shared reference servers.** Create objects inside a
  rolled-back transaction; clean up anything you could not avoid creating.
- `pg_hba` / login rules: `tests/test_rust_pg_login_differential.py` (915
  scenarios) needs `SECANTUS_PG15_BIN` and **skips without it**, so a green
  local suite says nothing about a login change. Run it by name.
- Both reference servers' OpenSSL 3 builds lack Blowfish and CAST5; GnuPG is
  the reference for those pgcrypto ciphers.

## Measuring

| instrument | what it measures | how |
| --- | --- | --- |
| `tools/probes/pg_corpora/` (320 corpora, each with its `.setup.sql`) | statements someone thought to write | `tools/probes/pg_corpora_sweep.py`, **serial**; one corpus by hand is `pg_differential.py <setup> <corpus> --types`. Serial because, run in parallel, an event trigger in one corpus fires on another's DDL and shared schemas get dropped under each other (39 "divergences" where there were 13) |
| `tools/probes/pg_corpora/expected/` | PostgreSQL's recorded answer to every corpus line, which `tests/test_rust_pg_corpora.py` holds the server to in CI (`pg-oracle`, no PostgreSQL needed). A divergence the server has today is in that file's `KNOWN_DIVERGENCES`, each naming an open `RP-nnn`; a fix that makes one match FAILS the test until its entry goes | **A corpus you add or change must be re-recorded in the same PR**, or the test fails: record on FRESH 14 and 15 clusters, twice, with `pg_corpora_sweep.py --record DIR`, then `pg_corpora_expect.py DIR1 DIR2` (it marks clock- and process-dependent checks volatile). Never record on the shared clusters: corpora leave roles behind on them |
| `tools/probes/pg_builtin_coverage.py` | every immutable/stable function in PG 15's `pg_proc`, one sample per argument type | finds what no corpus asked; `pg_corpora/builtin_coverage.sql` pins fixes, `tests/test_rust_pg_builtin_coverage.py` runs them in CI without PostgreSQL |
| `tools/probes/pg_isolation.py` | PostgreSQL's isolation specs (`src/test/isolation/specs`): every permutation's rows, errors and blocking, step by step, on two servers | concurrency and SERIALIZABLE (`ssi.rs`, a port of `predicate.c`). `tests/backlog/test_rp_isolation.py` replays the vendored specs against 15.19's recorded traces; record a new spec with `--record` on the PRIVATE cluster, since specs create tables. Blocking is judged by `--wait`, so a step slower than it reads as blocked |
| psycopg 3's suite | protocol and type system | `./inv validate-psycopg-rust` (~20 min debug; writes `.validation/psycopg-raw-rust.json` and checks every started test reported). **Not** `invoke validate-psycopg`, which runs the Python server and overwrites its report. `--tls` adds the TLS run; `.github/workflows/psycopg-tls.yml` runs it weekly on Linux and on PRs touching TLS, re-running a TLS-only failure before counting it (`psycopg_validation/check_run.py`) |
| `tests/backlog/` | the open `RP-nnn` items | `python tools/backlog.py verify --file backlog-rust-postgres.md` (see the `backlog` skill) |

A clean corpus sweep is silent about what no corpus covers, and the psycopg
number measures the protocol, not the query language: do not read either as
"nearly done". Do not put the psycopg rate in a sentence beside the Python
server's published one; they cover different test populations.

Before a corpus or matrix counts as evidence, read the `differential-probe`
skill's "probes lie" rules: report exception TYPE with SQLSTATE, treat
both-sides-non-database-error as vacuous, compare values rather than a
normalising `::text`, reset state between scenarios, give the probe a
self-check that must pass on PostgreSQL, and count the lines each side
produced. Before naming a new corpus, `git ls-files tools/probes/pg_corpora`
-- a new `overloads.sql` was once written over an existing one.

## Where a change goes

| change | place |
| --- | --- |
| a built-in whose overload the name alone does not settle | `secantus-pgplan/src/builtins.rs` (asked first, at all three call sites: expression evaluator, FROM-less select list, call over one column) |
| an operator called by its function name (`int4pl(a, b)`) | `secantus-pgplan/src/builtin_alias.rs` (only names measured as refused; never one with a VARIADIC overload) |
| overload choice | `secantus-pgplan/src/funcsig.rs` (`func_select_candidate` over PG 15's `pg_proc`, implicit casts, type categories) |
| operator type checks | `secantus-pgplan/src/optype.rs` |
| a JOIN of any kind | `secantus-pgplan/src/joins.rs` (planned as a source) |
| a correlated subquery | `secantus-pgplan/src/correlated.rs` |
| rules | `secantus-pgplan/src/rule_rewrite.rs` |
| `pg_get_viewdef` / `pg_get_expr` text | `secantus-pgplan/src/ruleutils.rs` |
| a catalog function usable inside an expression | `secantus-pgplan/src/scalar.rs` (`is_scalar` is the gate: a name absent there answers `0A000` anywhere but a bare select target) |
| PL/pgSQL | `secantus-pg/src/plpgsql_fn.rs` (interpreter over `parse_plpgsql` JSON) |
| triggers | `secantus-pg/src/triggers.rs` |
| TLS, `pg_hba.conf`, CRLs, ident maps, reload | `tls.rs`, `hba.rs`, `crl.rs`, `ident.rs`, `live.rs` in `secantus-pg/src` |
| xid counter / `age()` | `secantus-pg/src/xids.rs` |

**A clause parsed and then ignored is the worst outcome.** `ON CONFLICT` and
`GROUPING SETS` were each parsed and dropped, so the client got a confident
wrong answer (a `23505` where PostgreSQL succeeds, a `42803` blaming the
user's query) instead of an honest `0A000`. When a parse-tree field is not
implemented, refuse it by name; grep the `pg_query` node you match for fields
the lowering never reads.

**Prefer a rewrite to a new plan node.** UPDATE ... FROM, DELETE ... USING,
views, aggregate ORDER BY and an aggregate over a residual WHERE are all AST
rewrites onto shapes the planner already runs, and none needed an executor
change.

## Rules earlier batches paid for

**Types decide, at plan time.**
- A built-in is picked by its argument TYPES. `substr(bytea, int)` once
  reached the text `substr` and rendered the bytes' debug text.
- Overload choice goes by TYPE CATEGORY and preferred type, and is often not
  the "natural" answer: `round(int)` is `float8`, because `float8` is the
  numeric category's preferred type. Read `pg_proc` / `typcategory` /
  `typispreferred`; do not reason about which overload "fits".
- A comparison across type categories (`text = int`, `date = int`) is
  `42883`. Lowered to MQL it compares across BSON types and matches NOTHING,
  which is a silent empty answer. `optype.rs` checks operands in the
  statement's own FROM scope; inside a subquery nothing is checked yet.

**Values.**
- `char(n)` is stored TRIMMED and padded where it enters an expression
  (`ROW_WIDTHS` / `pad_bpchar`), then trimmed again for comparison, `||` and
  text casts. `octet_length` / `concat` / `format` see the padding; most
  functions do not.
- PostgreSQL on arm64 contracts multiply-adds into FMA: the last digit of
  `percentile_cont`, `corr`, `regr_*`, `stddev` over floats only matches
  with `f64::mul_add` at the same sites.
- Case mapping is Unicode-aware with the SIMPLE mapping.
- Transcribe from the PostgreSQL C source, not the docs (`to_char`,
  `to_number`, datetime input, jsonpath, numeric scale rules were each
  matched that way, checked by a `SELECT <expr>` matrix).

**Planning and execution.**
- A joined row is keyed `alias<U+001F>column` and the source exposes ONLY
  those keys. The lowering resolves a column by its LAST name part, so every
  reference is rewritten to its key first; one the rewrite misses is a
  42703, never a binding to the wrong side. A bare name both sides have is
  42702.
- The narrow two-table join path still goes FIRST (psycopg's catalog queries
  rely on its regtype-aware OID equality). It must refuse the shapes it gets
  wrong: a WHERE on the nullable side of a LEFT JOIN, a bare name both sides
  have, a qualifier naming neither side.
- A statement is planned for Describe as well as Execute, and an
  uncorrelated subquery runs during planning. Anything with a side effect
  runs only under `planning_to_execute`; elsewhere a volatile subquery
  returns no rows. (`select (select nextval('s'))` once advanced the
  sequence three times.)
- Correlation is found by `foreign_qualifier` (a qualified reference naming
  nothing in the subquery's FROM) and, for unqualified outer names, by
  planning and catching 42703. Keep both honest.
- `ALTER TABLE ADD/DROP COLUMN` REWRITES the stored rows, otherwise a column
  dropped and re-added resurrects old values. `ALTER COLUMN TYPE` is governed
  by the TYPES (an assignment cast must exist), not by whether the values
  would convert.
- The default window frame is `RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT
  ROW` with or without ORDER BY, and CURRENT ROW under RANGE includes peers.

**Transactions and writes.**
- A statement that can run user code (trigger, function, `DO`) is its OWN
  transaction even in autocommit (`runs_user_code`), or a raise after a write
  leaves the write committed.
- A savepoint is an undo journal of rows (RP-018, `UndoJournal` in
  `secantus-storage`): every document write inside a user transaction
  records the row as it was, once per savepoint frame, at the ten storage
  write sites (`undo_note_row`); a table's create or drop is recorded too
  (`undo_note_collection`). A new storage write path must call the hook, or
  ROLLBACK TO silently keeps its writes. A sequence's advance is never
  undone, as in PostgreSQL.
- Autocommit row writes run in a transaction of their own, retried on a write
  conflict at READ COMMITTED (`run_autocommit_write`). Without it a SET
  computed from a row lost another session's update. Only a probe running two
  sessions at once could see that.
- `Storage::insert` REPORTS a rejected document in its result rather than
  failing. Use `insert_checked`; a dropped result once lost rows in a table
  rewrite.

**Catalog.**
- If you add a catalog field, check both servers preserve it; the golden test
  in `secantus-pgcatalog/src/golden.rs` catches this class. A Rust rewrite of
  a catalog row once erased keys only the Python server knew.
- Catalog state the Python server already keeps (`__sql_column_grants__`,
  `__sql_relation_acl__`, `param_modes`) goes in its collection and shape;
  grep `src/secantus/sql/catalog.py` before inventing a layout. (The Python
  servers no longer bind the format, but these layouts are what is there.)
- `information_schema.tables` / `columns` / `sequences` keep their schema in
  the name (`relation_name`), or a user's own `columns` table becomes
  unreachable.
- Dump reference catalogs with `oid < 16384`; the shared reference holds
  other sessions' TOAST relations.
- ruleutils prints the ANALYSED query: which casts appear depends on the
  operators PostgreSQL resolves. Fall back to the text as written rather than
  guess.

**Logins.** A role with a password must pass SCRAM-SHA-256; one with none is
trusted; an UNKNOWN user is refused unless `--trust-unknown-users`
(`trust_unknown_users` on the embedded handles). A harness connecting as
`test` / `probe` / the OS user needs the flag or `postgres`.

**Probes.** `pg_advisory_xact_lock` in autocommit releases at once: correct
behaviour, wrong probe. Two runs that die on the same client error read as
agreement.

## Cross-protocol facts

- SQL to MongoDB works: a table written by `secantusd-pg` reads back through
  `secantusd-rs` as a collection, `PRIMARY KEY` as `_id`.
- MongoDB to SQL does NOT: names resolve through `__sql_catalog__`, which
  only `CREATE TABLE` writes, so a `pymongo`-written collection is
  `relation "..." does not exist`. (The Python SQL layer samples
  uncatalogued collections; this one does not.)
- Never both at once: WiredTiger's exclusive lock makes the second server
  exit. Pointing both at one store is a hand-off.

## History behind the CLAUDE.md rule

The `age(xid)` case (model the real mechanism). This server had no
  32-bit xid counter, and the first version froze the catalog at each run's
  first xid, so `age(datfrozenxid)` counted from startup and reset on every
  restart. It was replaced before merging by PostgreSQL's own model: one
  persisted counter that runs on across restarts, and `datfrozenxid` fixed
  at 722 where `initdb` leaves it (`crates/secantus-pg/src/xids.rs`).
