# SQL server external conformance gauges plan

**Status: first two gauges landed; portfolio open.** G1 (sqllogictest —
`invoke validate-slt`, 26/30 files, 4 declared divergences,
`docs/validation-report-slt.md`) and G2 (psycopg's unmodified suite —
`invoke validate-psycopg`, 91.3% at last full run, weekly in `validate.yml`,
`docs/validation-report-psycopg.md`) are committed tooling; the §6 results log
below records how they got there. Slice zero (§2) shipped along the way.
G6 (SQLAlchemy's dialect-compliance suite — `invoke validate-sqlalchemy`,
weekly in CI) landed 2026-07-31 at 572/738 (77.5%) and climbed to **713/735
(97.0%, zero errors)** the same day after the reflection / temp-table /
LIKE-ESCAPE / numeric-division rounds below.
Outstanding: the G1 `postgres-extended` second lane and gauges G3–G5 / G7
(per-item status in §5). This document plans the SQL/Postgres
analogue of the thirteen MongoDB driver gauges: comprehensive **external,
unmodified** test suites run against the `SecantusPGServer` over a real
`postgresql://…` connection, reported as pass/fail/skip counts the way
`invoke validate` does for pymongo.

Research basis: a July 2026 survey of the Postgres-compatibility ecosystem —
every candidate below was verified against its live repo (targeting mechanics,
size, license, maintenance status). Key precedent: **no from-scratch pg-wire
engine runs `pg_regress` unmodified** — CockroachDB, Materialize, RisingWave,
CedarDB, and Dolt all converged on the same portfolio this plan adopts:
sqllogictest for SQL correctness + unmodified driver suites for wire/client
conformance + a fuzzer for continuous stress.

---

## 1. The gauge model, restated for SQL

Same invariant as the Mongo gauges (`/conformance-gauges`):

- **Upstream suites run unmodified** — vendored as git submodules, never
  patched. Divergence is expressed only in include/deselect lists on our side
  (`*_validation/include_modules.py`-style) plus a documented
  `expected_failures` catalog.
- **Targeting is a URI/env var** — every chosen suite accepts a connection
  string or `PG*` env vars pointed at a daemon `SecantusPGServer`.
- **The number is the deliverable** — each gauge reports pass/fail/skip and a
  pass %; regressions block merges, growth is celebrated in the changelog.
- Gauge dirs are dev-only (excluded from sdist/wheel), run weekly in
  `validate.yml`.

One SQL-specific addition to the model: **pass-% is the headline, not 100%.**
The sqllogictest world tracks "nines of correctness" (Dolt publishes theirs);
a surrogate server's honest number is a fraction that grows, exactly like the
pymongo gauge did.

---

## 2. Slice zero — prerequisites nearly every suite gates on

The Postgres analogue of mongod's `hello` handshake. Without these, several
suites refuse to run *anything*:

- **`pg_type`-family bootstrap queries at connect.** Npgsql runs a
  four-statement `pg_type`/`pg_class`/`pg_proc`/`pg_range`/`pg_enum`
  type-loading batch on every connect (keyed off `pg_type.typreceive` →
  `pg_proc.proname = 'array_recv'`); asyncpg runs a recursive CTE over
  `pg_catalog`; Postgrex similarly. These must succeed with credible rows
  before those drivers execute a single test. `psql -E` output (the exact SQL
  behind every `\d`/`\dt`/`\df`) is the ready-made checklist for catalog
  fidelity.
- **`server_version` ParameterStatus is a free lever.** pgjdbc, Npgsql,
  psycopg, Postgrex, pg8000, and the PHP suites all self-skip
  newer-feature tests based on the advertised version — under-advertise to
  shed tests honestly.
- **SQLSTATE-accurate errors.** Returning `0A000 feature_not_supported` for
  unimplemented features converts Npgsql failures into skips (it probes with
  `0A000` explicitly). Wrong SQLSTATEs fail tests that would otherwise skip.
- **Auth**: trust + SCRAM-SHA-256 minimum (already shipped in `pgauth.py`);
  md5/cleartext matrices in pg8000/pgx/rust-postgres are env-gated and
  skippable.
- **`CREATE DATABASE` + a superuser-ish role** — assumed by most ORM suites
  and by sqllogictest-rs's parallel mode (run `-j 1` until it exists).
- **SAVEPOINT / transactional DDL** — load-bearing for every ORM suite
  (savepoint-based per-test isolation). Currently deferred in
  `sql-postgres-plan.md` (sqlglot parse gap → needs SQL-text preprocessing);
  it becomes a hard prerequisite at the ORM-gauge stage (§4 G6).

---

## 3. The portfolio — ranked, with verified facts

### G1 — sqllogictest corpus + sqllogictest-rs runner (fit 9/10, headline gauge)

- **What**: the SQLite-originated corpus — ~622 `.test` files, ~5.7M queries
  (Dolt's full-run record: 6,884,305 records). License is pick-any
  GPL/BSD/MIT/CC0 → vendorable as a submodule. Sources: canonical Fossil at
  sqlite.org/sqllogictest; git mirrors `gregrahn/sqllogictest` (active),
  `dolthub/sqllogictest`, `risinglightdb/sqllogictest-sqlite`.
- **Runner**: [`sqllogictest-rs`](https://github.com/risinglightdb/sqllogictest-rs)
  (Apache-2.0/MIT, v0.29.1 Feb 2026, actively maintained).
  `cargo install sqllogictest-bin`. Speaks real pgwire via tokio-postgres:
  `--engine postgres` (simple protocol) and `--engine postgres-extended`
  (extended protocol) — **two protocol lanes from one corpus**.
  `--host/--port/--db/--user/--pass` (or `SLT_*` env), `-j 1` (parallel mode
  needs CREATE/DROP DATABASE), JUnit XML output, `--keep-db-on-failure`.
- **Dialect handling**: run as engine `postgresql` so the corpus's own
  `onlyif`/`skipif` gating applies; `rowsort`/`valuesort` absorb NULL-order
  differences; `hash-threshold` MD5s canonical result text — the hashing is
  where value *rendering* gets stress-tested hardest (text format of floats,
  NULLs, negative zero).
- **Rejected runners**: CockroachDB's logictest harness (internals-bound;
  their corpus fork frozen 2018), DuckDB's (embedded-only), dolthub's Go
  runner (needs a hand-written harness). `hydromatic/sql-logic-test` (MIT,
  JDBC-based, `-e psql`) is a cheap second-opinion runner later.
- **Known runner defect (first run, §6)**: 0.29.1 can't parse the corpus's
  trailing-comment directive style (`skipif mysql # comment`) — needs an
  upstream fix or a preprocessing pass before corpus-wide numbers are
  meaningful.

### G2 — psycopg 3 test suite (fit 9/10, the pymongo analog)

- **What**: ~2,000 pytest test functions in `psycopg/psycopg` (LGPL-3.0,
  very active). Targeted with a single env var: `PSYCOPG_TEST_DSN`.
- **Why it's the right first driver gauge**: identical mechanics to the
  existing pymongo gauge (pytest, include/deselect lists, embedded or daemon
  server), *and* upstream already ships partial-server skip machinery — its
  CockroachDB support (`tests/fix_crdb.py`, ~40 known-unsupported feature
  markers auto-activated by server detection) is direct precedent for
  pointing this suite at a non-Postgres backend.
- **Friction**: needs libpq installed (dev-only, fine); version-gated tests
  key off `select version()`; TPC/COPY/pipeline/notify tests live in
  deselectable files.

### G3 — CockroachDB pgtest wire corpus (fit 9/10, the wire-protocol gauge)

- **What**: the closest existing thing to a pgwire protocol conformance
  suite. ~50 datadriven files of raw Parse/Bind/Describe/Execute/COPY/error/
  notice exchanges in `pkg/sql/pgwire/testdata/pgtest`, with a runner in
  `pkg/testutils/pgtest`. This is the SQL analogue of the php-ext/C gauges —
  the strictest framing check.
- **Targeting** (verified): `go test ./pkg/sql/pgwire -run TestPGTest
  -addr 127.0.0.1:PORT -user postgres`.
- **Tolerances built in**: `crdb_only`/`noncrdb_only` markers; ErrorResponse
  *details* are blanked in comparison — exactly the tolerance a surrogate
  needs. Our include-list starts with the non-crdb files.
- **License caveat**: CockroachDB Software License (source-available, not
  OSI). Acceptable as a dev-only vendored gauge like the driver submodules,
  but flag it in the vendor README.

### G4 — pgx / pgconn / pgproto3 (Go) (fit 9/10, strictest low-level client)

- **What**: `jackc/pgx` (MIT), ~809 test funcs total — `pgconn` (155) and
  `pgproto3` (71) are the mongo-c-driver analog: hand-rolled wire protocol,
  binary codecs, batch/pipeline mode.
- **Targeting**: `PGX_TEST_DATABASE` env var; optional `PGX_TEST_*_CONN_STRING`
  vars (auth/TLS variants) skip cleanly when unset. CockroachDB is in pgx's
  own CI matrix — another partial-server precedent.
- **Friction**: some tests want hstore + ltree types present and a nasty
  quoted username; all deselectable.

### G5 — Npgsql (.NET) or pgjdbc (Java) (fit 9/10 each, second wave)

- **Npgsql**: `NPGSQL_TEST_DB` env var (full conn string), 1,834 `[Test]`
  attrs; creates databases itself; `MinimumPgVersion` + `0A000`-probe
  self-skips. PostgreSQL license. **Hardest catalog gate**: the type-loading
  batch (§2) must be right first.
- **pgjdbc**: two-line `build.local.properties` (or `-Dtest.url.PGHOST=…`),
  236 test classes / ~1,691 annotations; ~15 classes need a privileged user;
  `-PskipReplicationTests`; deep `pg_catalog` use in DatabaseMetaData tests.
  BSD-2. CedarDB maintains a public pgjdbc fork with incompatible tests
  disabled — the closest published analog of our gauge pattern (we'd use
  include-lists instead of a fork).
- Toolchains already exist in this repo: .NET SDK (csharp gauge), JDK/Gradle
  (java/kotlin gauges).

### Later driver gauges (in rough order)

| Suite | Targeting | Size | Fit | Notes |
|---|---|---|---|---|
| node-postgres | `PG*` env + `PGTESTNOSSL=true`, `make test-all connectionString=…` | ~479 `it()` + ~389 custom-harness asserts | 8 | bespoke node harness, exit-code accounting like the .phpt gauge |
| pg8000 | `PGHOST/PGPORT/PGPASSWORD`; user hardcoded `postgres` | ~586 funcs | 8 | **moved to codeberg.org/tlocke/pg8000** (GitHub 404s); pure-Python wire = strict framing; already a dep |
| lib/pq (Go) | `PG*` env; `PQTEST_BINARY_PARAMETERS=1` second lane | ~156 funcs | 8 | revived upstream (commits July 2026); LISTEN/NOTIFY + COPY heavy |
| psycopg2 | `PSYCOPG2_TESTDB*` env | ~779 funcs | 7 | feature-frozen; overlaps psycopg 3 |
| asyncpg | setting `PGHOST` switches its harness to external-server mode | ~332 funcs | 7 | all-binary protocol, prepared-statements-for-everything; deselect cluster-reconfig tests |
| PHP ext/pgsql + pdo_pgsql | `PGSQL_TEST_CONNSTR` / `PDO_PGSQL_TEST_DSN` | ~287 .phpt | 7 | byte-exact `var_dump` error strings; SKIPIF-on-connect means unreachable server silently all-skips — **assert a minimum executed count** |
| Postgrex (Elixir) | `PG*` env, but test_helper shells to `psql` for setup | ~455 tests | 7 | pg_type bootstrap + auto-excluding tags |
| rust-postgres | **hardcoded `127.0.0.1:5433`** in test source | ~164 tokio tests | 6 | fixed-port serial gauge, like the mongocxx/27017 precedent; mandatory binary result format |
| ruby-pg | — | 1,120 examples | 2 | unusable unmodified: spec helper initdb's its own cluster |

### G6 — ORM suites (after SAVEPOINT lands)

- **SQLAlchemy** (fit 9, MIT): two modes — full suite via
  `pytest --dburi postgresql+psycopg://…` (README.unittests.rst), and the
  **third-party-dialect compliance suite** (`sqlalchemy.testing.suite` + a
  `requirements.py` where each capability is switched off,
  README.dialects.rst): a built-in "declare what you support, run the rest
  unmodified" mechanism — the exact analog of our include-lists. Already a
  dev dep.
- Then: GORM (`GORM_DIALECT=postgres GORM_DSN=…`, fit 8), Rails ActiveRecord
  (`ARCONN=postgresql` + config.yml; standalone
  `test/cases/adapters/postgresql/` subset; free simple-vs-extended axis via
  `arunit_without_prepared_statements`, fit 8), Django
  (`tests/runtests.py --settings=…`; needs CREATE DATABASE ×2 + savepoints,
  fit 8), Ecto (`PG_URL=… ECTO_ADAPTER=pg mix test`, small but
  binary-format-strict, fit 7).

### G7 — continuous fuzz + smoke (not pass/fail gauges, but always-on)

- **SQLsmith** (GPLv3 — dev-only tool, never vendored into the wheel; v1.5
  May 2026, fit 8): `--target="host=… port=…"`; query-only (no DDL); reads
  the schema straight from `pg_catalog` (`pg_type`/`pg_class`/`pg_proc`);
  **degrades gracefully** — auto-blacklists grammar productions that
  consistently error. The invariant it buys: *any connection drop is a real
  wire bug*. The SQL analogue of `invoke rust-stress`.
- **pgbench** (fit 8): `pgbench -i` = DROP/CREATE TABLE + client-side COPY +
  ALTER ADD PRIMARY KEY; the TPC-B script = BEGIN/UPDATE×3/SELECT/INSERT/END;
  `-M extended` / `-M prepared` flip it into an extended-protocol exerciser.
  Doubles as the seed for a SQL perf-regression suite later.
- **psql `-E` scripted smoke** (fit 8): drive `\d`/`\dt`/`\df`/`\l` and
  assert non-error; startup alone validates ParameterStatus handling.
- **SQLancer** (MIT, active, fit 6, later): PG provider with NOREC / TLP /
  QUERY_PARTITIONING oracles; needs CREATE DATABASE and patching its
  per-oracle `ExpectedErrors` lists (a fork, not a flag) to tame
  "unsupported" noise — revisit once the SQL surface is broad.
- **pgmockproxy** (`jackc/pgmock`, fit 6): logging MITM proxy emitting every
  frontend/backend message as JSON — run the same script against real
  Postgres and against us, diff transcripts. A debugging instrument, not CI.

---

## 4. Rejected / not viable

- **`pg_regress` unmodified (fit 3/10).** The harness does accept
  `--host/--port/--use-existing/--schedule` (verified in `pg_regress.c`),
  but `test_setup.sql` creates a C function from `regress.so` and loads
  fixtures via *server-side* `COPY FROM :'filename'` (superuser + server
  filesystem access to the PG source tree), and a "pass" is a byte-identical
  diff — error text, NOTICEs, row order, `\d` output, pinned locale/timezone.
  The escape hatch is maintaining our own `expected/` files, which breaks the
  unmodified invariant. Only PG-source *forks* run it (YugabyteDB, Greenplum,
  Neon); every from-scratch engine chose the G1–G5 portfolio instead.
  **Long-term option (fit 6)**: a curated ~30-file pure-SQL `--schedule`
  (boolean/int/join/aggregates/union/subselect/with/…) with client-side
  fixture seeding — an upgrade path, not a starting point.
- **pgTAP** (fit 1): server-side PL/pgSQL framework — needs a PL/pgSQL
  executor; it's a framework, not a corpus.
- **NIST SQL Test Suite** (fit 1): dead since 1996, downloads gone,
  embedded-SQL execution model anyway.
- **SQLRight / Squirrel** (fit 2): coverage-instrumented fuzzers that compile
  the DBMS under test — unusable over TCP.
- **pgbouncer/pgcat/Odyssey suites** (fit 2–3): test the proxy, not the
  backend. (pgbouncer-in-the-middle as an extra-strict client smoke is a
  cheap later idea.)
- **Postgres Compatibility Index** (`secp256k1-sha256/postgres-compatibility-index`,
  MIT; pgscorecard.com — published: AlloyDB 93%, YugabyteDB 85%, CockroachDB
  40%, Aurora DSQL 21%): live feature probes, 12 categories. Breadth-only
  but a cheap public scorecard number (fit 6) — nice-to-have once the real
  gauges exist.

---

## 5. Rollout order

1. **Slice zero** (§2): ✅ shipped incrementally through the G2 rounds (§6) —
   type-OID fidelity, `pg_typeof`, TypeInfo catalog flows, enum/composite/range
   OID minting, `0A000` for unimplemented features.
2. **G1 sqllogictest** — ✅ landed (#417): corpus vendored at
   `vendor/sqllogictest`, `invoke validate-slt` (preprocess → fresh daemon per
   file → per-file report), curated 30-file include list in
   `slt_validation/include_paths.py` with 4 declared divergences.
   Weekly CI landed 2026-07-31 (`validate.yml` installs a pinned
   `sqllogictest-bin 0.29.1` via cargo, cached by version). **Still open**:
   the `postgres-extended` second lane and growing toward the 622-file
   corpus.
3. **G2 psycopg 3** — ✅ landed: `vendor/psycopg` submodule pinned to the
   installed 3.3.4, `invoke validate-psycopg` via `PSYCOPG_TEST_DSN`,
   include/deselect in `psycopg_validation/`, weekly in `validate.yml`.
   Headline trajectory: 42% → 91.3% (§6).
4. **G3 pgtest wire corpus** — ❌ not started. Vendor cockroach's `pgtest`
   runner + testdata (sparse checkout or a small extraction repo given
   monorepo size — decide at implementation; license note in vendor README),
   `invoke validate-pgwire`.
5. **G4 pgx**, then **G5 Npgsql or pgjdbc** — ❌ not started (pick by which
   catalog gaps §2 surfaces first).
6. **G7 SQLsmith + pgbench** as always-on stress/smoke (`invoke sql-stress`) —
   ❌ not started; both need binaries not in the dev env (SQLsmith build,
   pgbench from a Postgres install).
7. **G6 SQLAlchemy compliance suite** — ✅ landed (2026-07-31): `invoke
   validate-sqlalchemy` runs `sqlalchemy.testing.suite` (nothing vendored — it
   ships inside the sqlalchemy package) over `postgresql+psycopg` against a
   daemon server, with capability declarations in
   `sqlalchemy_validation/requirements.py` (`schemas` closed — tables aren't
   namespaced per schema; see backlog). Baseline 572 P / 166 F (77.5%); now **713
   passed / 22 failed / 680 skipped (97.0%, zero errors)**, weekly in
   `validate.yml`, `docs/validation-report-sqlalchemy.md`. The climb came
   from: temp-table reflection semantics (relpersistence 't', session-scoped
   visibility via `pg_table_is_visible` → own-session `Session.temp_tables`,
   drop at connection teardown), typmod fidelity (`Column.decl_oid`/`typmod`
   → pg_attribute + `format_type`), `pg_get_expr` passthrough (SERIAL
   defaults → autoincrement), plain-view columns in pg_attribute (described
   from the stored SELECT), constraint comments in pg_description,
   `quote_ident` in `pg_get_constraintdef` (the whole BizarroCharacterTest
   class), LIKE ESCAPE + computed patterns (incl. a Describe fallback that
   strips an unlowerable WHERE), IS [NOT] DISTINCT FROM, numeric-cast
   division (`CAST(15 AS NUMERIC)/10 = 1.5`), LIMIT/OFFSET expressions,
   INSERT DEFAULT VALUES, CREATE SEQUENCE NO MINVALUE/MAXVALUE, and declared
   composite-PK order. Standing it up forced three server
   fixes: `CREATE/DROP EXTENSION` (citext / hstore / plpgsql accepted, others
   0A000), `COMMENT ON CONSTRAINT` (check / unique / FK / PK), and a real
   correctness bug — a table-level ``CONSTRAINT <name> PRIMARY KEY`` was
   silently dropped (no `_id` mapping, no uniqueness); the declared PK name is
   now honored end-to-end (enforcement, reflection, duplicate-key messages).
   The remaining 22 are a residual tail (see the backlog entry): the
   insertmanyvalues sentinel shape (`INSERT … SELECT p0 FROM (VALUES …) AS
   alias(p0, c) ORDER BY c`), LIMIT/OFFSET inside union arms, FROM-less
   `SELECT … WHERE EXISTS`, DateTimeMicroseconds (the documented BSON
   millisecond-precision divergence), covering-index INCLUDE reflection,
   DISTINCT ON, scalar-subquery-in-row fetch, and sequence lastrowid.

Mechanics mirror the existing gauges throughout: daemon `SecantusPGServer`
subprocess on an ephemeral port (ad-hoc reproducers on `127.0.0.1:55432` per
`sql-postgres-plan.md` §8), per-gauge `*_validation/` dir with include lists,
`validation_summary/expected_failures.py` entries, weekly `validate.yml` job,
`/conformance-gauges` skill updated per gauge as it lands.

---

## 6. First bounded run — results (2026-07-12)

Both G1 and G2 were exercised against a live `SecantusPGServer` (trust auth,
fresh WT storage). Harness wiring works end-to-end for both: no hangs, no
connection drops, psycopg subset finished in 29s, every sqllogictest file in
under a second (fresh server per file).

### psycopg 3.3.4 — 6-file subset: 409 passed / 570 failed / 10 skipped (42%)

Files: `test_connection.py`, `test_cursor.py`, `test_cursor_common.py`,
`types/test_numeric.py`, `types/test_string.py`, `types/test_bool.py`.
Failure causes, biggest first:

1. **RowDescription type-OID fidelity for computed columns** (~100+ tests,
   the highest-leverage fix): computed / parameter-derived result columns
   are described as `text` (25) instead of `bool` (16) / `int4` (23);
   `float4` widens to `float8` (701 vs 700); array columns report 25
   instead of the array OID (e.g. 1007). Same root cause broke
   sqllogictest hashing (below).
2. **`pg_typeof()` unsupported** (69 tests) — psycopg's type tests wrap
   values in it; one function unlocks all of them.
3. **`relation "" does not exist`** (46 tests) — empty relation name in
   the error points at a statement shape the SQL layer misparses.
4. **`CREATE FUNCTION` missing** (70 tests) — the `test_leak` block defines
   a helper `exploding_generate_series()` server-side.
5. Long tail: 31 `InternalError_`, array-literal text-format escaping
   (~16), missing `cidr` type (13).

### sqllogictest corpus via sqllogictest-rs 0.29.1 — 5/10 files pass

PASS: `evidence/slt_lang_{aggfunc,dropindex,droptable,dropview,reindex}`.
FAIL:

- `evidence/{in1,in2,slt_lang_createview}` — **harness bug, not ours**:
  sqllogictest-rs 0.29.1 fails to parse the corpus's trailing-comment
  directive style (`skipif mysql # comment`). This hits many corpus files;
  needs an upstream fix, a preprocessing pass, or the hydromatic JDBC
  runner as fallback. (Contrary to the first-run guess, `hash-threshold`
  IS implemented and works — verified with a probe file whose MD5 matched
  sqlite's canonical hash, confirming our value rendering is
  byte-identical where column types are declared.)
  **UNBLOCKED (2026-07-13)**: a preprocessing pass that strips trailing
  comments from `skipif`/`onlyif` lines (3.2M lines across the 622-file
  corpus) makes the whole corpus runnable. Bounded 26-file sweep on
  current code (fresh server per file): **11/26 files pass end-to-end**
  — all of `evidence/` except `in2.test` (constant-LHS `WHERE 1 IN (2)`)
  and `slt_lang_createview` (DELETE on a view must error). The
  `random/*` corpus fails at its first heavily-decorated expression
  (`- - col0`, `+ +`, aggregates inside arithmetic, `NOT BETWEEN
  (NULL)`) — the planner's expected-a-column/unsupported-value-expression
  rejections; closing those unlocks the random corpus wholesale since
  the runner is first-error-fatal per file. Next: formalize as
  `invoke validate-slt` (preprocess step + runner + per-file report).
  **Expression-shape cluster fixed (PR #408)**: untranslatable WHEREs
  route to per-row evaluation (dry-run probe in `where_needs_per_row`),
  computed unary projections type correctly, `ORDER BY <ordinal>`
  resolves on the evaluated path, and the scalar evaluator implements
  three-valued NOT/AND/OR/BETWEEN. The random corpus's wholesale
  first-query rejections are gone — files fail 30–80 records deeper on
  the next tail: aggregate arithmetic under DISTINCT (`- MAX(- - col0)`
  value bugs, `SUM(DISTINCT *)`), empty `IN ()`, DELETE-on-view must
  error, and the pushdown `$not`'s two-valued semantics
  (`NOT (a + NULL > 0)`).
  **Aggregate-args round (PR #410)**: expression aggregate arguments at
  all five single-table accumulator sites, identity-wrapper stripping,
  group-path WHERE residual, empty implicit-aggregate row synthesis,
  `<> NULL` pushdown match-nothing, three-valued/empty `IN`, and the
  preprocessor now reflows multi-column expected blocks (the second
  sqllogictest-rs comparison incompatibility; 610k blocks). Remaining
  tail, started on the uncommitted `sql-join-agg-tail` branch in the
  SecantusDB-sql-rowdesc-oids worktree (`_join_accumulator` groundwork in
  place but unreached): the JOIN-group planner extracts aggregate args
  via a bare-column-only helper (planner.py ~4828 "expected a column")
  and its WHERE lowering predates the residual probe — `COUNT(*) * 32
  FROM a CROSS JOIN b`, `… CROSS JOIN … WHERE NOT (15) IS NULL`,
  `MAX(cor0.col0 + 1)` over joins. Also open: constant-LHS
  `IN (SELECT …)` (the residual probe skips subquery WHEREs),
  select1.test:94 (CASE with scalar subquery value mismatch), and
  DELETE-on-view.
- `select1.test:94` — **real**: `ORDER BY 1` on a computed numeric column
  sorts as text (358 after 1700); hashing was skipped because the
  computed column's type OID is unknown (§6 psycopg cause 1).
- `evidence/slt_lang_update.test:75` — **real**: `UPDATE t1 SET x=3+1` →
  "unsupported value expression" (no arithmetic in UPDATE SET).

### Slice-zero priorities confirmed empirically

1. ~~RowDescription type-OID fidelity for computed/derived columns.~~
   **Shipped** (PR #396): int2/float4 tags, CASE/array/arithmetic/aggregate
   inference, declared-param-OID describe, binary array codec.
2. ~~`ORDER BY` numeric sort on computed expressions.~~ Fixed by the tag
   inference above (the text sort was a symptom of the text tag).
3. ~~`pg_typeof()`~~ **shipped** (PR #396, with `::regtype` normalization and
   Parse-time declared-OID typing); ~~arithmetic in `UPDATE SET`~~ and
   ~~SRF-in-FROM (`relation ""`)~~ fixed by parallel sessions (b232–b234 era).
4. Remaining from the subset run (415 failures at 58%): `CREATE FUNCTION ...
   RETURNS SETOF` (the `test_leak` block), binary-parameter/`Float4`-exact
   round-trips, `cidr`/composite-type gaps, error-detail fields.

Subset trajectory (same six files, psycopg 3.3.4): 409 → 542 (OID fidelity)
→ 564 (pg_typeof, PR #396) → 637 (PR #401: client_encoding, DML-RETURNING
Describe, stream-over-SRF, array/numeric binary codecs, quoted-type DDL)
→ 685 (PR #403: bare COPY options, computed projections over SRF/catalog
row sources) → **887 passed of 979 (91%, PR #405**: oid/regtype,
declared-parameter typing on every path, binary codecs for
time/interval/uuid/net/json/range/multirange, tstzrange types, wide
numerics; psycopg `test_leak` 72F → 72P), from 42% at the first run.
Remaining (~92): sub-millisecond timestamp fidelity (BSON datetime is
int64-millis — storage-representation change), numeric >34 significant
digits (Decimal128 cap), server-side `exp()` function, unknown-param
`concat` resolution (42P18), client-side literal-quoting edges.

Run artifacts (clones, venv, per-file JSON, logs) are in the session
scratchpad; the runs are cheap to reproduce — a fresh-server-per-file
sqllogictest sweep is ~2s/file, the psycopg subset ~30s.

### 2026-07-14 session — preprocessor corrected, three-valued round (PR pending)

**Preprocessor rules, corrected empirically (the earlier "reflow all multi-column
blocks" note was WRONG — it broke `valuesort` records):**

1. Strip trailing ` # comment` from `skipif`/`onlyif` lines (0.29.1 parser bug).
2. Reflow value-per-line expected blocks into row-per-line ONLY for
   `nosort`/`rowsort` multi-column records; `valuesort` records are compared
   value-per-line by sqllogictest-rs and must stay untouched. Hash blocks stay.
3. Inject `hash-threshold 8` (sqlite's default) into any file with hash-form
   expectations but no directive — sqllogictest-rs defaults to 0 (never hash),
   which made select1/select4's hash records unpassable.

**Full psycopg gauge on main (invoke validate-psycopg, 2026-07-14): 2419
passed / 1704 failed / 115 skipped = 58.7%** over the whole vendored suite (the
91% figure above is the 6-file subset). Top clusters: pg_range/pg_cursors/
typarray catalog gaps (→ TypeInfo/enum/composite/range cascades, ~550), dump/
load round-trips (~340, datetime 259), mypy missing in gauge env (125,
environmental), COPY gaps (~230), server-side cursors + prepared (~110),
CREATE SCHEMA (71).

**sqllogictest bounded sweep (27-file: evidence/ + select1-4 + random samples +
index samples): 11/27 → in-flight branch `sql-slt-tail` fixes →** all of
`evidence/` except `slt_lang_createview` (SQLite read-only-view semantics —
real Postgres auto-updates simple views; permanent corpus divergence, needs a
deselect), `random/expr/slt_good_1` + `random/select/slt_good_1` pass
end-to-end. Fixed this round: three-valued NOT/<>/NOT IN/NOT BETWEEN pushdown,
IN NULL-candidates, constant-LHS IN (list+subquery), FROM-less aggregates,
COUNT(<expr>), SUM(DISTINCT <expr>), SUM-all-NULL→NULL, GROUP BY computed
projections + DISTINCT dedup + `SELECT *` over full group keys, parenthesized
join FROM, join computed-over-aggregate routing, silently-dropped
non-lowerable join WHERE, empty-aggregate row on the evaluated path, lazy
COALESCE, constant HAVING, div-by-zero → 22012, float8 text `12`-not-`12.0`,
`_infer_scalar_tag` exponential blowup (0.5s/query → 2ms; expr files no longer
time out).

**Final sweep on the branch (26-file bounded set): 19/26 pass** — added this
session: select1, select2, select3 (full sqlite-canonical files, hashes and
all), random/expr/slt_good_1 + slt_good_10, random/select/slt_good_1,
random/aggregates/slt_good_1, evidence/in2. Late-round fixes beyond the list
above: operand-form `CASE x WHEN v` NULL-equality, expression-aggregate
accumulator-key collision (`MAX(3)` vs `MAX(a-b)` shared one accumulator!),
integer `/` truncation in aggregate args and `$expr`, wrapped-NULL comparison
operands (`51 <> (NULL)`), null-guarded `$expr` computed comparisons (BSON
order matched `NULL <> 19`), lazy COALESCE, constant HAVING folding,
join-group-window silent WHERE drop, DISTINCT-agg empty-input crash.

**Landed 2026-07-14, five PRs:** #414 (three-valued NULL + aggregate tail),
#416 (HAVING/ON/dup-group-keys), #417 (`invoke validate-slt` — the gauge is
now committed tooling at 26/30 with 4 declared divergences), #418 (psycopg
TypeInfo catalog fidelity: `pg_type.typarray`/`typdelim`, `pg_range`,
`to_regtype()`, user-type `oid::regtype` — all five TypeInfo fetch flows work
end-to-end), #419 (report refresh). **psycopg gauge: 58.7% → 59.8%** (2465
passed). Then **#420** (this plan checked in) and **#421** (COPY runs inside
the open transaction block — the sub-protocol handler never entered
`use_user_transaction`, so same-block `CREATE TABLE` was invisible to COPY
*and copied rows survived a ROLLBACK*; the three copy-heavy psycopg suites
moved 230 → 374 passing). Next psycopg levers, in measured order: `CREATE
SCHEMA` (71), server-side cursors (`InvalidCursorName`, ~110 with
test_cursor_server), `pg_prepared_statements` (21), mypy in the gauge venv
(125, environmental); then a full gauge re-run to restamp the headline.

**2026-07-15 session (continuing the same run):** #425 (CREATE SCHEMA +
schema-qualified user types — clears the 71-test cluster), #427 (server-side
cursors: a DECLAREd cursor IS a portal, Describe('P')/Close('P') fallback,
$N-in-Command substitution, pg_cursors + pg_prepared_statements; cursor +
prepared suites 26 → 102), #429 (**WT session leak → eviction livelock**:
every writer connection leaked its cached WT session because pgserver's
teardown never called _reset_thread_session — the single-daemon gauge wedged
at ~test 420 3/3; fixed + flip-tested regression test; gauge now completes in
~125s). **psycopg headline: 2554 passed / 61.9%** (from 2465 / 59.8%).
Next levers: enum result OIDs in RowDescription (the ~150-test enum-behavior
cluster + "unknown oid" cluster, 212 — needs minted user-type array oids and
catalog-aware ColumnDesc resolution; ~26 ColumnDesc sites), CREATE TYPE AS
RANGE, schema-qualified tables, select4/5 join perf.

**2026-07-16 session (enum result OIDs — the lever above, landed):**
RowDescription reports the minted enum oid for enum result columns (SELECT /
correlated / RETURNING incl. MERGE / Describe, via `executor._out_column_descs`
over the shared `Catalog.enum_type_oids` mint — now allocation-stable:
persisted at CREATE TYPE, never renumbered/reused, because a positional mint
renumbers types under a client's registered loaders). Two adjacent root causes
fell out of the gauge A/B: (1) `::regtype`/`to_regtype` of user-type names
didn't apply Postgres identifier folding, so `EnumInfo.fetch(conn,
"StrTestEnum")` returned None and poisoned psycopg's whole enum suite; (2) ALL
user types reported `pg_type.typarray = 0`, and psycopg's `test_register_scope`
pops the loader keyed on `array_oid` — popping oid 0 deleted the **global
unknown-oid fallback loader** and poisoned every later unknown-oid text load in
the process. That was the entire pre-existing 212-test "unknown oid loader not
found" cluster (spread over range/numpy/multirange/string/uuid/composite).
User types now mint `typarray = oid + 100000`. Controlled A/B (deterministic
order, `-p no:randomly`, pinned base worktree): **2554 / 61.9% → 2809 / 68.1%
(+255, zero stable regressions; "unknown oid loader not found" 212 → 0)**.

**Second slice (same session): the cast / Bind / array paths.** `%s::mood`
constant-select casts describe with the enum oid and validate labels (22P02);
a Bind parameter declared with an enum oid (a registered psycopg dumper) is
label-validated; `oid::regtype::text` quotes mixed-case names (psycopg's
ClientCursor pastes the fetched regtype verbatim as a cast suffix — unquoted
`CamelCaseEnum` folds back to lowercase and misses the type); `%s::mood[]`
round-trips as a list through the minted array oid in text AND binary
(`pgextended` now derives user-type array element oids from the offset
scheme in both directions). psycopg's `tests/types/test_enum.py`: **197/197**
(base: 45/197). Full-gauge deterministic A/B vs origin/main: **2554 / 61.9% →
2900 / 70.3% (+346; regressions only the churny `test_leak` parametrization
flips + one load-flake copy timeout that passes in isolation)**. Remaining
enum-adjacent gaps (backlog b107): enum tags dropped by JOIN/GROUP BY plan
shapes, `mood[]` table columns, no pg_type row for the paired `_name` array
type.

**2026-07-16 session, second round (json + datetime + typing clusters):** the
three biggest remaining clusters diagnosed in parallel by read-only subagents,
then fixed: `types/test_json.py` 181→0 (json/jsonb parsed at ingress — casts,
params via typed `JsonText` cast substitution, oid 114/199 aliases;
`array[…]::text` renders array_out literals; `E'…'` escape strings),
`types/test_datetime.py` 259→0 (temporal params substitute as typed casts like
Decimal always did; interval unit abbreviations + justified-duration
comparison; epoch/infinity/BC/wide-year sentinels with proleptic-Gregorian
ordinal math for beyond-Python-range arithmetic; TimeZone GUC honoured on
input+output incl. POSIX numeric zones, set_config ParameterStatus, and
case-insensitive GUC names; DateStyle rendering for date/timestamp;
integer-µs binary encoders with infinity sentinels), and `test_typing.py`
125→0 (environmental — mypy added to the dev extra). Full-gauge deterministic
A/B: **2900 / 70.3% → 3473 / 84.2% (+573; regressions only the `test_leak`
churn, one `test_hold`, and 2 `test_copy_from_leaks` variants that now reach
the known BSON ms-precision storage limit)**. Remaining levers by failure
count: composite/range behavior incl. CREATE TYPE AS RANGE, column 42
(typmod/type identity 33), copy 34 (binary COPY 14, one-CopyData-per-row 9),
cursor_common 27, connection 23, typeinfo 18 (`to_regtype` quoted idents).

**Third round (same session): range/multirange parameters.** Diagnosed by a
read-only subagent (root causes: params never coerced 94, range arrays 48,
CREATE TYPE AS RANGE 36, untyped-binary 10), then fixed the coercion class:
range/multirange params (and arrays) travel as `typemap.TaggedText` and
substitute as `::type` casts; `'{…}'::int4range[]` coerces elements; untyped
literals take the range operand's type (PG context inference); bounds store
canonically (`ranges.make_range` coerces per subtype) and equality compares
`ranges.canonical` identity; `range::text` renders the literal;
ParameterDescription resolves undeclared params to text like PG.
`test_range.py` + `test_multirange.py`: 149 failed + 31 errors → **10 + 31**
(remainder: untyped-binary params needing Parse-time type inference, and
CREATE TYPE AS RANGE — both in `tasks/backlog.md`). Composite (66) still open:
`row()` constructor 38, composite-value materialization 24, small catalog
bugs 4 (see the diagnosis in this entry's session notes).

**Fourth round: composite materialization.** `row(…)` anonymous records
(text + PG binary record layout), composite cast/record-literal parsing into
typed subdocs (quoted/escaped/nested fields, positional remap for
``row(…)::type``), minted-OID RowDescription overrides (casts, arrays of
composites, typed field access), binary composite params decoded via the
embedded per-field oids, composite/domain OIDs moved to the allocation-stable
mint, and reserved-word quoting in regtype output.
`types/test_composite.py`: 66 → **17** (remainder: binary record edge
samples, `test_dump_builtin_empty_range` interplay, suite-order singles).

**Fifth round: CREATE TYPE … AS RANGE.** Engine-level Command interception
(the statement exceeds sqlglot — same pattern as CREATE DOMAIN); the range
type and its auto-created companion multirange (Postgres' rename rule) mint
allocation-stable OIDs and reflect through `pg_type` (typtype `r`/`m`, real
`typarray`) + `pg_range`; literal casts parse with the declared subtype's
coercion (`ranges.custom_elem`), the constructor function works, registered
psycopg dumpers' binary params decode via PG's range wire layout, results
describe with the minted OID and encode in both formats, DROP TYPE removes
the pair. Full-gauge deterministic A/B: **3736 / 90.5% → 3764 / 91.2%**, and
the suite-wide error count fell 53 → 22 (first movement in nine runs — the
custom-range fixtures had errored out of 31 range/multirange tests before
any assertion ran). The four type suites now total 30 failed / 0 errors:
composite 17, range 11, multirange 2, enum 0. Remaining levers by failure
count: typmod/type identity in RowDescription 33, binary COPY 14,
transaction characteristics 14, untyped-binary Parse-time inference ~10,
plus residual edges (custom-range quoting ×3, composite binary samples ×4,
suite-order singles).

**Sixth round: enum OIDs through every plan shape + enum-array columns.**
The pipeline/evaluated planners flatten output columns to string type tags,
so GROUP BY keys, JOIN projections, DISTINCT, and per-row-evaluated selects
described enum columns as text (25); the enum identity now travels in a
parallel `out_enum_types` position map (populated by the DISTINCT / group /
join / evaluated builders, resolved by a shared `_tagged_out_column_descs`
in both Execute and Describe). `array['sad'::mood, …]` constructors report
the array-companion OID; `mood[]` table columns (previously rejected as
`unsupported column type`) store text arrays with per-element 22P02 label
validation and describe with the array OID. Full-gauge deterministic A/B:
**3764 / 91.2% → 3765 / 91.3%** — the visible fixes are
`test_prepared.py::test_change_type_execute{,many}` (they exercise enum-array
table columns); the plan-shape OID work is correctness beyond the gauge's
coverage (registered psycopg loaders now fire on grouped/joined results).
Gauge-runner observation for a future round: the `test_cursor_client.py::
test_leak[asyncio-*]` family flips parametrizations every run around a
persistent `FeatureNotSupported: unsupported value expression` — one
dedicated investigation would stabilise ~5 flapping tests.

**Second slice (same day, PR pending): 19/26 → 22/26.** HAVING grew
`IS [NOT] NULL` (bare / aggregate / computed group-key operands, exact under
any NOT nesting), three-valued `[NOT] IN` over group keys, and always-unknown
NULL-operand folds; JOIN ON folds constant (`ON 80 = 70`) and always-unknown
(`ON NOT NULL < expr`) conditions; join GROUP BY with the same bare column
name from two aliases (`GROUP BY cor1.col1, cor0.col1`) no longer collapses
to one key (wrong answers — both group and group-window paths); grouped
`SELECT DISTINCT` over a join dedups. `random/groupby/slt_good_{0,1}` and
`random/aggregates/slt_good_10` now pass end-to-end.

**Remaining failures, all corpus/harness divergences (the bounded sweep's
ceiling without harness work):**
- `evidence/slt_lang_createview` — corpus expects SQLite read-only views; real
  Postgres auto-updates simple views. Permanent divergence → deselect list.
- `random/aggregates/slt_good_0` (~22k in) + `random/expr/slt_good_0` (~75k
  in) — corpus expects SQLite's division-by-zero→NULL; real PG (and we, now)
  raise 22012. Permanent divergence unless a coercion layer is added.
- `random/select/slt_good_0` (~52k in) — `query I` over a REAL expression:
  the corpus expects the RUNNER to cast values per the type string;
  sqllogictest-rs doesn't. Harness gap (custom engine or upstream fix).
- select4 — runs past its early records into a 4-way-join section that
  exceeds the 300s per-file budget (join perf, `tasks/backlog.md`).
