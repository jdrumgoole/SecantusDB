# SQL / PostgreSQL interface plan

**Status: proposed.** This document plans a *third protocol persona* for
SecantusDB: a PostgreSQL-wire-compatible SQL front end that lets `psql`,
`psycopg` / `psycopg2`, the Postgres JDBC driver, and BI / ORM tooling connect
and run SQL against the **same WiredTiger-backed `Storage`** the MongoDB server
already owns. It is the SQL analogue of the existing Mongo server: a surrogate
single-node Postgres rather than a surrogate single-node MongoDB.

It is a **Python-server-only** effort to begin with (see §9 *Versioning* and §10
*Rust*). It does not touch the Mongo request path.

---

## 1. The decision: speak the Postgres wire protocol, don't invent an API

The whole identity of SecantusDB is *"a `pymongo` client cannot tell us apart
from `mongod`."* The on-brand SQL interface applies the same rule to a different
driver family:

> **`psql` + `psycopg` are the conformance target.** Behaviour is "correct" when
> a libpq-based client cannot tell SecantusDB apart from a real `postgres` for
> the SQL surface we support.

That rules out the tempting shortcut of an embedded `server.sql("SELECT ...")`
Python method as the *product*. An embedded executor is still worth building —
as the **bottom layer** the wire server sits on, and as the unit-test seam — but
the deliverable is a TCP listener speaking the PostgreSQL v3 frontend/backend
protocol so unmodified clients connect over a `postgresql://…` URL.

### Mirror the Mongo layering

The Mongo server is `server.py` → `wire.py` → `commands.py` → operator engines →
`Storage`. The Postgres server mirrors it one-for-one:

| Mongo layer | Postgres analogue | Responsibility |
|---|---|---|
| `server.py` (`SecantusDBServer`) | `pgserver.py` (`SecantusPGServer`) | accept loop, thread-per-conn, startup/auth/query message flow, owns shared `Storage` |
| `wire.py` (OP_MSG framing) | `pgwire.py` | PWP v3 message framing + every frontend/backend message type |
| `auth.py` (SCRAM) | `pgauth.py` | Postgres SASL `SCRAM-SHA-256` + cleartext/trust; reuse `secantus.auth` core |
| `commands.py` (dispatch table) | `sql/` package | parse SQL → plan → execute |
| `query`/`update`/`aggregate`/`projection` | **reused unchanged** | SQL is *translated into* these engines |
| `storage.py` | **reused unchanged** | the same WT store |

The key insight that makes this tractable: **SQL is compiled down to the
existing Mongo operator engines, not to a new execution engine.** `WHERE` →
`query.matches` filter dict; `ORDER BY` / `LIMIT` / projection → `find_matching`
arguments; `JOIN` / `GROUP BY` / aggregate functions → an aggregation pipeline
run through `apply_pipeline`. SQL therefore inherits index acceleration (IXSCAN,
compound/range/sort planning), collation, and transactions *for free*, exactly
where the Mongo side already has them. The SQL layer is a **translator**, and the
storage/operator layers stay protocol-agnostic — a layer boundary worth
defending as hard as "the wire layer never knows about commands."

### Tables are collections (the dual-protocol view)

A connection's Postgres *database* selects the SecantusDB `db`; a SQL *table* in
schema `public` maps to a `collection`; a *row* is a *document*; the primary key
column maps to `_id`. So the same data is reachable two ways — write via
`pymongo`, read via `psql`, and vice-versa. This dual view is the feature, and
it falls out of sharing `Storage`.

---

## 2. The critical decision: schema, and the BSON ↔ Postgres type map

Mongo is schemaless; SQL clients expect typed columns and a catalog. This is the
SQL analogue of the "type-mapping strategy" note in CLAUDE.md, and the single
most consequential design choice. We support **two table flavours**, both
backed by documents stored as opaque BSON (we never adopt a lossy intermediate
representation — same discipline as the Mongo side):

1. **Declared tables** — `CREATE TABLE public.users (id bigint primary key, name
   text, age int, profile jsonb)`. The column list + types + PK are persisted in
   a per-db catalog collection `__sql_catalog__`. `INSERT`/`UPDATE` coerce values
   to BSON per the declared type; `SELECT` projects declared columns and emits
   the matching Postgres type OIDs in `RowDescription`. This is what ORMs and
   migration tools expect, and it's what `information_schema` introspection
   answers from.

2. **Reflected tables** — an existing Mongo collection with no `CREATE TABLE`.
   Two access modes:
   - `SELECT *` returns a single `doc jsonb` column (the whole document), plus a
     promoted `_id` column. Always works, zero schema.
   - Columns are **inferred** by sampling N documents (configurable) so
     `SELECT name, age FROM users` works against Mongo-written data. Inferred
     columns that don't exist in a given row read as SQL `NULL`.

### Type map (the load-bearing table)

| BSON | Postgres type / OID | Notes |
|---|---|---|
| double | `float8` (701) | |
| int32 | `int4` (23) | |
| int64 | `int8` (20) | |
| Decimal128 | `numeric` (1700) | exact; the natural SQL fit |
| string | `text` (25) | |
| bool | `bool` (16) | |
| datetime (UTC) | `timestamptz` (1184) | Mongo dates are UTC instants |
| ObjectId | `text` (25) by default; opt-in `uuid`-like domain | 24-hex string round-trips |
| Binary | `bytea` (17) | subtype preserved only for UUID subtype-4 → `uuid` |
| document (nested) | `jsonb` (3802) | |
| array | `jsonb` (3802), or `_<elem>` array type when homogeneous + declared | |
| null / missing | SQL `NULL` | missing field == NULL |
| Timestamp (oplog) | `timestamptz` or composite | rarely surfaced to SQL |

Both directions go through one module, `sql/types.py`, and it is pinned by the
same kind of parity testing the Mongo engines use (§8). Text encoding is the
Postgres *text format* for the v3 protocol (every value rendered as its
canonical text representation); **binary result format** is a later optimisation,
not v1.

---

## 3. The wire protocol (`pgwire.py` + `pgserver.py`)

PostgreSQL v3 is simpler to frame than OP_MSG but has more message *types* and a
stateful startup. The work:

**Startup & auth flow**
- `SSLRequest` (8-byte magic): reply single byte `N` (no TLS) in v1, or `S` then
  wrap the socket when TLS is configured — reuse the Mongo server's `SSLContext`
  plumbing verbatim.
- `StartupMessage` (no type byte; carries `user`, `database`, params).
- Auth: respond `AuthenticationSASL` advertising `SCRAM-SHA-256`, run the
  SASL exchange (`AuthenticationSASLContinue` / `SASLResponse` /
  `AuthenticationSASLFinal`), then `AuthenticationOk`. **Postgres SCRAM is the
  same RFC 5802 core as Mongo's** — reuse `secantus.auth`'s SCRAM state machine;
  only the message envelope and the channel-binding flag (`n,,`) differ. Also
  support `trust` (AuthenticationOk immediately) and cleartext for first-cut
  bring-up.
- Emit `ParameterStatus` for the keys libpq requires (`server_version`,
  `server_encoding=UTF8`, `client_encoding`, `DateStyle`, `integer_datetimes`,
  `standard_conforming_strings`, `TimeZone`), `BackendKeyData`, then
  `ReadyForQuery('I')`.

**Simple query protocol** (`psql` default, many ORMs)
- `Query('Q')` → parse → plan → execute → `RowDescription('T')` +
  `DataRow('D')`* + `CommandComplete('C')` (`SELECT n`, `INSERT 0 n`, etc.) +
  `ReadyForQuery`.

**Extended query protocol** (psycopg parameterised statements, JDBC)
- `Parse('P')` / `Bind('B')` / `Describe('D')` / `Execute('E')` / `Sync('S')` /
  `Close('C')`. Needs a per-connection **prepared-statement** and **portal**
  registry (the SQL analogue of `CursorRegistry`). `$1`-style placeholders bound
  from `Bind` parameter values (text format in v1).
- `RowDescription` from `Describe` must report column type OIDs *before* execution
  — so the planner produces a result schema as part of planning, not just rows.

**Errors**: every failure becomes an `ErrorResponse('E')` with a SQLSTATE code
(e.g. `42P01` undefined_table, `42601` syntax_error, `23505` unique_violation,
`42703` undefined_column) + message, then `ReadyForQuery('E')` so the connection
survives — exactly the discipline of `dispatch` turning handler exceptions into
`{ok:0,...}` rather than dropping the socket. A `sql/errors.py` maps internal
exceptions (`DuplicateKeyError`, parser errors, planner "unsupported") to
SQLSTATEs.

`pgserver.py` is a near-copy of `server.py`'s accept loop, thread-per-connection
model, idle timeout, connection cap, max-connections, TLS wrap, and graceful
drain — refactor the shared parts of `server.py` into a small reusable mixin
rather than fork them. It can run **standalone** or **alongside** the Mongo
server on a second port sharing one `Storage` instance (the compelling
dual-protocol demo).

---

## 4. SQL parsing and planning (`sql/`)

**Parser choice (decision needed — recommendation below).** Build on
[`sqlglot`](https://github.com/tobymao/sqlglot): pure-Python, MIT-licensed,
zero native deps (fits the self-contained-wheel constraint — no `cmake`/C build,
unlike `pglast`/libpg_query), and ships a Postgres dialect. We parse with
`sqlglot.parse_one(sql, dialect="postgres")` and walk its typed AST. If exact
Postgres-grammar fidelity later proves necessary, `pglast` (real libpg_query)
is the fallback, at the cost of a native build dependency.

**`sql/catalog.py`** — `__sql_catalog__` collection per db: table → {columns,
types, pk, options}. Answers DDL and feeds `information_schema`. Also synthesises
entries for reflected (un-declared) collections.

**`sql/planner.py`** — AST → an execution plan over `Storage`:

| SQL | Lowered to |
|---|---|
| `SELECT ... WHERE` | `find_matching(filter=…, projection=…, sort=…, skip/limit=…)` |
| `WHERE a = 1 AND b > 2` | `{a: 1, b: {$gt: 2}}` (inherits IXSCAN planning) |
| `WHERE a IN (…)`, `LIKE`, `IS NULL`, `BETWEEN` | `$in`, `$regex`, `$eq:null`/`$exists`, `$gte`+`$lte` |
| `ORDER BY`, `LIMIT`, `OFFSET` | `sort`, `limit`, `skip` |
| `COUNT/SUM/AVG/MIN/MAX`, `GROUP BY`, `HAVING` | aggregation pipeline: `$group` + `$match` |
| `JOIN ... ON` | `$lookup` + `$unwind` + `$project` |
| `DISTINCT` | `$group` by the keys |
| expressions / functions in projection | `expressions.evaluate` via `$project` computed fields |
| `INSERT` | `Storage.insert` (type-coerced docs) |
| `UPDATE ... SET ... WHERE` | `update_matching` with `$set`/`$inc`/… |
| `DELETE ... WHERE` | `delete_matching` |
| `CREATE/DROP TABLE`, `CREATE INDEX` | catalog write + `Storage.create_index` |
| `BEGIN/COMMIT/ROLLBACK` | `Storage.begin/commit/abort_user_transaction` |

**`sql/executor.py`** — runs the plan, streams rows into `DataRow` batches, and
honours portal `max_rows` (extended protocol) by registering a server-side
cursor when a result is paged — reusing `CursorRegistry` semantics.

**`sql/typemap.py`** — §2's bidirectional map.

---

## 5. Catalog emulation: the real compatibility tax

The hardest *breadth* problem isn't `SELECT` — it's that real Postgres clients
fire a barrage of introspection and session setup before/around user SQL.
`psql` startup, JDBC `getMetaData`, SQLAlchemy reflection, and `\d` all need:

- `SELECT version()`, `current_schema()`, `current_database()`, `current_user`.
- `SHOW <param>` / `SET <param>` (`search_path`, `client_encoding`,
  `standard_conforming_strings`, `application_name`, `TimeZone`, `extra_float_digits`) —
  accept-and-record, echo via `ParameterStatus` where libpq expects it.
- `pg_catalog` virtual tables: `pg_type`, `pg_class`, `pg_namespace`,
  `pg_attribute`, `pg_database`, `pg_proc` (subset), `pg_settings`.
- `information_schema.tables` / `.columns` / `.schemata`.
- A pile of `pg_catalog` functions: `pg_get_expr`, `format_type`,
  `pg_table_is_visible`, `oid` casts, `::regclass`, etc.

Strategy mirrors the Mongo handshake commands: implement a **curated subset
driven by what target clients actually emit**, captured by running real
`psql`/psycopg/JDBC against the server and filling gaps until they're happy.
These resolve through a small **virtual-table provider** in `sql/catalog.py`
that answers `pg_catalog.*` / `information_schema.*` queries from the catalog +
`Storage.list_collections`, so the same SELECT planner serves them. Per the
"faithful *not supported* over half-implemented" rule, unimplemented catalog
bits return a clean `ErrorResponse`, never a wrong answer.

This phase is where most of the schedule risk lives — budget for it explicitly.

---

## 6. Explicitly out of scope (the discipline that keeps this honest)

Same posture as the Mongo server — return a faithful "not supported" rather than
a divergent half-feature. Out of scope, at least initially:

- Window functions, recursive CTEs, `GROUPING SETS`/`CUBE`/`ROLLUP`.
- Stored procedures / `PL/pgSQL`, triggers, rules, views (maybe simple views
  later), materialized views.
- Foreign-key / `CHECK` constraint *enforcement* (declarations parsed and stored
  in the catalog, but not enforced — like Mongo's schema validation scope).
- Real multi-statement planning, the full type system (ranges, enums, composite
  types, arrays-of-arrays), `COPY` (bulk), `LISTEN/NOTIFY`, cursors via SQL
  `DECLARE` (the extended-protocol portal path is supported instead).
- Anything that only makes sense in multi-node Postgres (replication slots,
  logical decoding) — same single-node honesty as the Mongo side.

Everything deferred lands in `tasks/backlog.md` as it's discovered.

---

## 7. Phasing (each phase independently testable and shippable)

- **P0 — Embedded executor spike.** `sql/` parser + planner + executor with **no
  wire**, exposed as `Storage`-backed `run_sql(db, sql) -> rows`. Covers
  `SELECT`/`INSERT`/`UPDATE`/`DELETE`/`CREATE TABLE` against declared tables.
  Unit-tested directly. De-risks the translate-to-Mongo-engines thesis before
  any protocol work. *(No client yet.)*
- **P1 — Wire bring-up, trust auth, simple query.** `pgwire.py` + `pgserver.py`,
  `SSLRequest→N`, startup, `trust` auth, `Query('Q')` round-trip,
  `ParameterStatus`/`ReadyForQuery`. Gauge: **`psql -c "SELECT 1"`** connects and
  returns. First "a real client connected" milestone.
- **P2 — Catalog + `psql` interactive.** §5 virtual tables/functions so
  `psql` connects cleanly, `\dt` / `\d table` work, `SELECT version()` etc.
- **P3 — Extended protocol + psycopg.** `Parse`/`Bind`/`Execute`/`Sync`,
  prepared statements, `$1` params, portals. Gauge: **psycopg** CRUD round-trip.
- **P4 — SCRAM-SHA-256 auth.** Reuse `secantus.auth`; gauge auth with `psql`
  PGPASSWORD + psycopg.
- **P5 — Joins, aggregates, GROUP BY, transactions.** The `$lookup` / `$group`
  lowering + `BEGIN/COMMIT/ROLLBACK`.
- **P6 — Reflected tables + jsonb.** Read Mongo-written collections via SQL,
  the dual-protocol view, `jsonb` for nested docs/arrays.
- **P7 — JDBC + tooling hardening.** Postgres JDBC driver, then SQLAlchemy /
  an ORM reflection smoke. These are the strict introspection clients (the
  SQL analogue of the Go/PHP-ext gauges being the strictest wire checks).

---

## 8. Testing & conformance (mirror the existing gauges)

- **Unit tests** pin SQL→Mongo-filter/pipeline translation in
  `tests/test_sql_planner.py` / `tests/test_sql_types.py` (the semantics oracle),
  and wire framing in `tests/test_pgwire.py`.
- **Integration tests** drive **psycopg** against a live `SecantusPGServer` on
  `port=0` + `tmp_path` storage (the `psql`/psycopg analogue of `tests/test_crud.py`),
  the conformance proof.
- **Cross-protocol parity**: write a doc via `pymongo`, read the row via psycopg,
  assert the round-trip — this is the dual-view correctness gate and the SQL
  analogue of "run the same code against SecantusDB and real mongod."
- **A new conformance gauge** (`invoke validate-postgres`): point a slice of
  Postgres's own regression `.sql`/`.out` pairs (or psycopg's test suite) at the
  server, dev-only, weekly in `validate.yml`. Start with a curated subset; grow
  it the way the driver gauges grew.
- **Ad-hoc port** for reproducers: pick a predictable alt like `127.0.0.1:55432`
  (Postgres's `5432` + offset), the SQL analogue of the Mongo `27018` convention.

---

## 9. Versioning, packaging, docs

- This is **Python-server** work: bump only `pyproject.toml` + `src/secantus/__init__.py`
  (`0.5.4bN`), per CLAUDE.md's two-version-line rule. No Rust crate bump.
- New optional dep `sqlglot` (pure-Python — no wheel/cmake impact). Gate the
  Postgres server behind an extra (`pip install secantus[sql]`) so the core
  Mongo wheel stays lean and the SQL stack is opt-in.
- New module tree under `src/secantus/`: `pgserver.py`, `pgwire.py`, `pgauth.py`,
  and a `sql/` package (`parser.py`, `planner.py`, `executor.py`, `catalog.py`,
  `typemap.py`, `errors.py`).
- CLI: a `--postgres-port` flag on the existing launcher to bring the SQL
  listener up beside the Mongo one; a `SecantusPGServer` for the one/two-line
  test ergonomic.
- Docs: a new `docs/sql.md` (architecture + supported-SQL matrix + the
  out-of-scope list, kept honest like `docs/compatibility.md`), and a row in the
  compatibility matrix.

---

## 10. Relationship to the Rust two-server model

This plan adds the Postgres persona to the **Python server** first, because the
operator engines and `Storage` it compiles down to are pure-Python and stable.
It deliberately does **not** entangle with the Rust server. Once the SQL layer
is proven in Python, a Rust port mirrors the existing pattern: the PWP wire +
SQL planner reimplemented over the PyO3-free `secantus-core` / `secantus-storage`
crates, pinned to the Python implementation by a parity suite — exactly how the
Mongo engines were ported. That is a separate, later effort with its own crate
version line; it is out of scope here beyond noting the seam.

---

## 11. Open decisions to confirm before P0

1. **Parser**: `sqlglot` (recommended — pure-Python, no native dep) vs `pglast`
   (exact grammar, native build).
2. **Schema model default**: declared-table-first (catalog-driven, ORM-friendly)
   vs reflected-first (zero-DDL over existing Mongo data). Recommendation: ship
   both, default a connection to *reflected* so existing collections are
   immediately queryable, with `CREATE TABLE` upgrading to a typed view.
3. **ObjectId surface**: `text` (24-hex, simplest, recommended) vs a custom
   domain/type.
4. **Auth default**: `trust` for the in-process test ergonomic (recommended,
   matches "two lines, no external processes") with SCRAM opt-in, mirroring the
   Mongo server's `require_auth=False` default.
