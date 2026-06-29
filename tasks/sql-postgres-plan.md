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

### Heterogeneous collections: how the column set is resolved

The sharp edge of this whole design is that a Mongo collection has no fixed
shape — different documents carry different fields, types, and nesting — while
SQL expects a rectangular, typed result. One wire fact governs the entire
answer:

> **The Postgres v3 protocol sends exactly one `RowDescription` (the column list
> + type OIDs) *before* any `DataRow`. There is no per-row schema.**

So the column set for a result is decided **once, at plan time** — it cannot
vary document-to-document. Heterogeneity is therefore never expressed as
"different columns per row"; it is absorbed by *projecting each document onto a
fixed column set* chosen up front. There are three ways that fixed set is
established:

1. **Declared table — authoritative.** The columns come from the `__sql_catalog__`
   entry, not from the documents. Extra fields in a document are invisible to the
   projection (optionally reachable through a catch-all `doc jsonb` column).

2. **Reflected, whole-document `jsonb` — always correct, zero schema.**
   `SELECT *` returns a fixed 2-column shape `(_id <type>, document jsonb)`; the
   entire document, *whatever* its structure, lands in the one `jsonb` column.
   Heterogeneity is a non-issue because nothing is flattened. Navigation uses
   jsonb operators (`document->>'name'`, `document->'profile'->>'city'`,
   `document #> '{a,b}'`), which lower to **dotted-path projection** in the query
   engine (`profile.city` → `$profile.city`). This is the safe default for
   genuinely messy collections.

3. **Reflected, inferred/flattened columns — convenience, best-effort.** Sample
   N documents, take the **union of top-level fields**, and resolve one type per
   field via a type lattice: all-numeric (int32/int64/double/Decimal128) widens
   to the common numeric type; mixed scalar kinds collapse to `text`; any nested
   doc/array becomes `jsonb`. The resolved set is **recorded in the catalog** so
   the column list is stable across queries and doesn't flap as the sample
   changes.

Once the column set is fixed (modes 1 and 3), each document is projected onto it
with uniform per-row reconciliation rules:

| Document reality vs. column | Result |
|---|---|
| field present, type matches column | coerce to the column's PG type |
| field present, type differs | coercion ladder: numeric widening → render to `text` → `NULL` (lenient) / error (strict) |
| field absent | SQL `NULL` (missing == NULL) |
| field is sub-doc/array, column is scalar | `NULL`, or carried as `jsonb` if the column is `jsonb` |
| extra field not in the column set | invisible in flattened/declared modes; reach it via `doc jsonb` or jsonb mode |

**The honest limitation:** flattening (mode 3) is a convenience that degrades
gracefully, not a guarantee — a deeply heterogeneous collection is best served by
jsonb mode, where nothing is lost. The plan's default (§11 #2) is reflected-first
so any existing collection is immediately queryable, with `CREATE TABLE`
available to upgrade a collection to a typed, declared view when its shape *is*
stable. The coercion edge cases at this exact boundary — Mongo-written loose data
read through a strict SQL column — are precisely what the §8 cross-protocol
parity tests (write via pymongo → read via psycopg → assert round-trip) exist to
pin down.

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

- **P0 — Embedded executor spike. ✅ landed.** `src/secantus/sql/` (parser →
  planner → executor) with **no wire**, exposed as
  `run_sql(storage, db, sql) -> list[SQLResult]`. Covers `CREATE TABLE` /
  `DROP TABLE` (declared tables), `INSERT`, `SELECT` (WHERE with
  `=`/`!=`/`<`/`<=`/`>`/`>=`/`IN`/`BETWEEN`/`LIKE`/`ILIKE`/`IS [NOT] NULL`/`AND`/
  `OR`/`NOT`, `ORDER BY`, `LIMIT`/`OFFSET`, `COUNT(*)`), `UPDATE`, `DELETE`. PK
  column ↔ document `_id`; literals coerced to the declared column type (the
  BSON↔PG type map). WHERE lowers to a real Mongo filter, so SELECT rides the
  storage layer's index/matching engines unchanged — the translate-to-Mongo
  thesis is proven. Tests: `tests/test_sql_planner.py` (translation oracle) +
  `tests/test_sql_spike.py` (end-to-end over an in-memory storage double backed
  by the real `query`/`update` engines). Parser: `sqlglot` (the `[sql]` extra),
  still an open decision (§11) — easy to swap behind `planner.parse`. *(No
  client yet.)*
- **P1 — Wire bring-up, trust auth, simple query. ✅ landed.**
  `src/secantus/sql/pgwire.py` (PG v3 framing + message builders/parsers) +
  `pgserver.py` (`SecantusPGServer`: accept loop, one daemon thread per
  connection, `SSLRequest`/`GSSENCRequest`→`N`, startup, `trust` auth, the full
  `ParameterStatus` set + `BackendKeyData` + `ReadyForQuery`, then the simple
  `Query` protocol). Queries run through P0's `run_sql`; rows stream back as
  `RowDescription`/`DataRow`/`CommandComplete` in the v3 *text* format; SQL
  errors become `ErrorResponse` (SQLSTATE) + `ReadyForQuery` so the connection
  survives. `SELECT 1` (FROM-less constant select) works — the headline gauge.
  `Storage` is injectable so the server runs over the shared store in production
  or an in-memory double in tests. Tests: `tests/test_pgserver.py` drives a real
  server with a pure-Python PG3 client (startup, `SELECT 1`, CRUD round-trip,
  NULL encoding, error-then-recover, multi-statement, SSL-decline). psql/psycopg
  as real-client smokes come with P2/P3 (psycopg uses the extended protocol).
- **P2 — Session layer + catalog virtual tables. ✅ landed (partial).**
  Per-connection `Session` (db / user / GUC settings) threaded through
  `run_sql`. Scalar session functions in FROM-less SELECT — `version()`,
  `current_database()`/`current_catalog`, `current_schema()`, `current_user`/
  `current_role`/`session_user`, `current_setting(name)`, `set_config(...)`,
  `pg_backend_pid()` (`functions.py`). `SHOW` / `SET` / `RESET` (settings
  persist on the session; reportable GUCs echo a `ParameterStatus`).
  `BEGIN`/`COMMIT`/`ROLLBACK` were accept-and-no-op here, now real (see
  *Transactions* below).
  `information_schema.tables`/`.columns`/`.schemata` and
  `pg_catalog.pg_class`/`pg_namespace`/`pg_type`/`pg_database` as **virtual
  tables** (`virtual.py`) — computed from the catalog and run through the
  ordinary SELECT planner via an in-memory backend, so `WHERE`/`ORDER BY`/
  `LIMIT`/`COUNT(*)` over them work with no new query code. **Catalog joins ✅
  landed** (see *Catalog joins* below): JOINs / GROUP BY across `pg_catalog` +
  `information_schema` now route through the aggregation pipeline against a
  `CatalogBackend`, so SQLAlchemy's `get_table_names()` / `has_table()` and
  interactive `psql`'s `\dt` work. **Still deferred:** `\d`'s column-level
  reflection needs `pg_attribute` / `pg_attrdef` (system catalogs we don't
  model) plus `format_type` / `pg_get_expr` functions and multi-join subqueries.
  Tests: `tests/test_sql_catalog.py` + wire coverage in `tests/test_pgserver.py`.
- **P3 — Extended query protocol. ✅ landed.** `Parse`/`Bind`/`Describe`/
  `Execute`/`Close`/`Sync`/`Flush` (`pgwire.py` builders/parsers +
  `pgextended.py` state machine). Per-connection prepared-statement and portal
  registries; `$1` placeholders (sqlglot `exp.Parameter`) substituted into the
  AST as literals so the existing column-type coercion types them (a text
  `"5"` bound to an `int8` column lands as `Int64(5)`); text + binary param
  formats decoded. `Describe` resolves result columns **without executing**
  (`engine.describe_statement` — planning is side-effect-free, so Describe never
  runs an INSERT/UPDATE/DELETE): statement-describe → `ParameterDescription` +
  `RowDescription`/`NoData`, portal-describe → `RowDescription`/`NoData`.
  `Execute` honours `max_rows` with `PortalSuspended`; errors enter the
  protocol's skip-until-`Sync` state so a bad statement can't desync the stream.
  Tests: `tests/test_pgserver_extended.py` (pure-Python extended client —
  prepared-statement reuse, params, NULL binds, portal suspend/resume,
  error-recovery, empty query). **psycopg** as a live gauge needs libpq
  (unavailable in this dev env) — wire-level coverage stands in; revisit when a
  libpq-backed client is available. Result rows are text-format only (binary
  result format is a later optimisation).
- **P4 — TLS + SCRAM-SHA-256 auth. ✅ landed.** `pgauth.py` runs the Postgres
  SASL `SCRAM-SHA-256` exchange (AuthenticationSASL → SASLContinue → SASLFinal),
  reusing `secantus.auth`'s `derive_credentials` / `StoredCredentials` — the same
  RFC 5802 verifier as the Mongo side, so a client proof is checked from the
  StoredKey/ServerKey without ever holding the plaintext. `SecantusPGServer(...,
  require_auth=True, users={...})` derives verifiers at startup; an unknown user
  runs a mock exchange (random verifier) so it fails identically to a wrong
  password — no username enumeration. TLS: `tls_cert_file`/`tls_key_file` answer
  `SSLRequest` with `S` and wrap the socket (reusing the `SecantusDBServer`
  SSLContext pattern); without it the server declines (`N`). Channel binding
  (`SCRAM-SHA-256-PLUS`) is not offered. Tests:
  `tests/test_pgserver_auth.py` (pure-Python SCRAM client: success / wrong
  password / unknown user → `28P01`; trust default; TLS query via an ephemeral
  `trustme` CA; TLS-declined). psql/psycopg need libpq (absent here) — the wire
  exchange stands in.
- **P5 — Joins, aggregates, GROUP BY. ✅ landed.** SELECTs with a JOIN, GROUP
  BY, HAVING, or aggregate functions now compile to a **Mongo aggregation
  pipeline** run through the existing `apply_pipeline` engine (a second execution
  path alongside `find_matching`). Aggregates `COUNT(*)`/`COUNT(col)`/`SUM`/`AVG`/
  `MIN`/`MAX` → `$group` accumulators (whole-table → `_id: null`, else `_id` =
  the GROUP BY keys); `HAVING` → a post-`$group` `$match` (an aggregate used only
  in HAVING registers a hidden accumulator); single two-table `INNER`/`LEFT JOIN`
  with an equality `ON` → `$lookup` + `$unwind` (`preserveNullAndEmptyArrays` for
  LEFT), with `alias.column` resolved to the right side and joined columns read
  from the `$<alias>` path. WHERE on a join lands as a `$match` after the lookup.
  `ORDER BY`/`LIMIT`/`OFFSET` apply as pipeline stages. The WHERE translator was
  refactored to a `resolve(column) -> (field, type_tag)` callable shared by the
  single-table and join paths. **Deferred:** JOIN combined with GROUP BY,
  three-plus-table joins, non-equi joins, `DISTINCT`, window functions. Tests:
  `tests/test_sql_aggregate.py`. Interactive `psql`'s `\d` is now closer but
  still needs the pg_catalog *functions* (`format_type`, `pg_table_is_visible`)
  + casts those join queries use.
- **P6 — Reflected tables + jsonb. ✅ landed.** A collection with no
  `CREATE TABLE` is queryable schema-on-read: `reflect.py` samples N docs, infers
  a column + type per top-level field, and presents a `TableDef` flagged
  `reflected` (un-sampled fields still resolve, as the permissive `any` type;
  nested docs/arrays surface as `jsonb`). `SELECT *` expands to the inferred
  columns; missing fields read as `NULL`; WHERE coercion uses the inferred type
  (so `age > 18` compares numerically) or passes the literal through for `any`.
  **jsonb navigation** — `->` / `->>` / `#>` (sqlglot `JSONExtract` /
  `JSONExtractScalar` / `JSONBExtract`) lower to dotted field paths read via
  `paths.get_path`; `->>`/`#>>` type as `text`, `->`/`#>` as `json`. Works in
  both projection and WHERE, on reflected *and* declared `jsonb` columns. The
  WHERE translator's `_field` helper resolves either a column or a jsonb path.
  Reflected tables are **read-only** (no `CREATE TABLE` → INSERT/UPDATE/DELETE
  still `42P01`); a declared table shadows reflection. This is the dual-protocol
  payoff — data written via `pymongo` is readable via SQL with no DDL.
  **Aggregates / joins over reflected tables ✅ landed** (see *Reflected
  aggregates & joins* below). **Deferred:** writes to reflected tables, and
  `jsonb` containment operators (`@>` / `?`). Tests: `tests/test_sql_reflect.py`.
- **Reflected aggregates & joins (post-catalog-joins). ✅ landed.** GROUP BY /
  HAVING / aggregate functions and JOINs now work over reflected (schema-on-read)
  collections, not just declared tables and catalogs — SQL analytics directly
  over `pymongo`-written data. The pipeline planner gained a `storage` argument
  (`plan_pipeline_select` / `_plan_join_select` / `_lookup_table_def`) so that,
  after the user-catalog and `pg_catalog`/`information_schema` virtual lookups
  miss, an *unqualified* table name reflects via `reflect.reflect`. A reflected
  table exposes the Mongo field names (so joins key off `_id`, not a DDL `id`),
  and a reflected aggregate column types as the permissive `any`. **Deferred:**
  in a JOIN, an *unqualified* reference to an *un-sampled* reflected field still
  can't be routed (qualify it `alias.field`, or have it appear in the sample).
  Tests: reflected GROUP BY / HAVING / JOIN in `tests/test_sql_reflect.py` +
  a driver-level reflected agg+join in `tests/test_pgserver_pg8000.py`.
- **Richer queries: multi-table joins + DISTINCT (post-reflected-joins). ✅
  landed.** `_plan_join_select` now compiles *any number* of `INNER`/`LEFT JOIN`s
  to a chain of `$lookup` + `$unwind`, each join relating the new table to the
  base or an already-joined alias (`a⋈b⋈c` where `c` joins on `b` works — the
  lookup's `localField` is the dotted path into the unwound alias via
  `_alias_field_path`). `SELECT DISTINCT` (single-table via `_plan_distinct_select`
  and over a join) dedups on the projected columns with a trailing
  `$group`-by-all-columns + re-`$project` (`_append_distinct`); `select_needs_pipeline`
  now routes DISTINCT through the pipeline. **Deferred:** JOIN *combined* with
  GROUP BY, non-equi / `OR` / `RIGHT`/`FULL`/`CROSS` joins, `DISTINCT` with
  aggregates, window functions, subqueries, and scalar SELECT-list expressions.
  Tests: multi-table / chained-join / DISTINCT in `tests/test_sql_aggregate.py` +
  a driver-level 3-table-join + DISTINCT in `tests/test_pgserver_pg8000.py`.
- **P7 — Real-driver gauge (pg8000 + SQLAlchemy). ✅ landed.** psql/psycopg need
  libpq (absent in the dev env), but **`pg8000`** is a *pure-Python* Postgres
  driver — a real, strict, extended-protocol client that runs here. A gauge
  (`tests/test_pgserver_pg8000.py`) drives it against `SecantusPGServer`:
  connect, parameterised CRUD, type round-trips (`numeric`→`Decimal`,
  `timestamptz`→`datetime`, `bigint`→`int`, `bool`), GROUP BY, JOIN, reflected +
  jsonb reads, session functions, **SCRAM auth** (pg8000 uses `scramp`), **TLS**
  (pg8000 `ssl_context`), and a **SQLAlchemy** Core round-trip
  (`postgresql+pg8000://`). The strict driver immediately found two real bugs,
  now fixed + pinned: sqlglot mis-tokenising adjacent `$1,$2` placeholders (the
  driver emits no spaces — fixed by `planner._normalize_params`), and
  schema-qualified `pg_catalog.version()` from SQLAlchemy's init (fixed by
  unwrapping `exp.Dot` in `functions`). SQLAlchemy *reflection*
  (`inspect().get_table_names()` / `has_table()`) now works via the catalog-join
  surface (see below). **Deferred:** column-level reflection (`get_columns` needs
  `pg_attribute`), the JDBC driver, and a live psql/psycopg gauge (when libpq is
  available). pg8000 + sqlalchemy added to the `dev` extra so the gauge runs in CI.
- **Catalog joins (post-P7). ✅ landed.** JOINs / GROUP BY across the
  `pg_catalog` + `information_schema` virtual tables — what SQLAlchemy's
  reflection and interactive `psql`'s `\dt` emit — now execute. The pipeline path
  (`select_needs_pipeline`) runs ahead of the single-virtual-table branch and is
  fed a `virtual.CatalogBackend`: a `Storage`-shaped proxy whose `find_matching`
  serves a virtual collection's rows in-memory (via the same `MemoryBackend`) and
  delegates real collections to WT, with `list_indexes → []` for virtual tables so
  `$lookup` takes the hash-join path; every other `Storage` method forwards
  through `__getattr__`. The planner's `_lookup_table_def` resolves a table to the
  user catalog *then* the virtual registry, so `_plan_join_select` /
  `_plan_group_select` span the system catalogs. WHERE gained the constructs these
  catalog queries use: `CAST` / `::type` (unwrapped to the inner literal),
  `col = ANY(ARRAY[...])` → `$in`, and the always-true visibility predicates
  `pg_table_is_visible` / `pg_type_is_visible`. `pg_class` grew a `relpersistence`
  column (`'p'`) for SQLAlchemy's temp-table filter. Tests: `tests/test_sql_catalog.py` +
  SQLAlchemy `get_table_names` / `has_table` in `tests/test_pgserver_pg8000.py`.
- **Column-introspection catalog surface (post-multi-join). ✅ landed (partial).**
  Added the `pg_attribute` / `pg_attrdef` / `pg_description` virtual tables
  (`_table_oids` keeps `attrelid` aligned with `pg_class.oid`), so column-level
  introspection *joins* resolve and run on the multi-join engine — e.g.
  `SELECT a.attname, a.atttypid, a.attnotnull FROM pg_attribute a JOIN pg_class c
  ON a.attrelid = c.oid WHERE c.relname = 't' ORDER BY a.attnum`. `pg_attrdef` /
  `pg_description` are present-but-empty (no defaults / comments in our model).
  The join tail (`_append_join_tail`) gained ORDER-BY-on-a-non-selected-column
  (carried as a hidden projected field, sorted, then dropped) — Postgres-legal and
  needed by these catalog queries. **Still deferred — the *exact* SQLAlchemy
  `get_columns()` / psql `\d` query:** it's a 4-table outer-join with compound
  multi-condition `ON`s (`… AND attnum > 0 AND NOT attisdropped`), scalar functions
  in the SELECT list (`format_type` / `pg_get_expr`), correlated scalar subqueries,
  and `CASE` — each a query-engine feature beyond the current pipeline; it returns
  a faithful `0A000`. `information_schema.columns` remains the working
  column-reflection path. Tests: `tests/test_sql_catalog.py`.
- **Scalar SELECT-list eval + compound join ON (post-column-surface). ✅ landed.**
  The column-metadata query SQLAlchemy / `psql \d` emit now executes end to end.
  Three pieces: (1) `secantus.sql.scalar` evaluates SELECT-list / ORDER-BY scalar
  expressions per row — catalog functions (`format_type` via an OID→typename map,
  `pg_get_expr`/`pg_get_serial_sequence`→NULL, `json_build_object`, `coalesce`),
  `CASE`, comparisons (three-valued for NULL), and **correlated scalar subqueries**
  (read inner rows through the same storage view, falling through to the outer row
  for correlation). (2) Compound join `ON`s — multi-key joins and residual
  predicates on the joined table — compile to the `$lookup` `let`/`pipeline` form
  via `_OnTranslator` (single-equality stays the simple `localField`/`foreignField`
  form for index acceleration). (3) A join whose SELECT list / ORDER BY needs
  per-row evaluation produces an `EvaluatedSelectPlan` (`_build_evaluated_join`):
  the pipeline does the joins + WHERE and yields full docs, then
  `executor.execute_evaluated_select` computes each output column in Python and
  applies DISTINCT / ORDER BY / LIMIT. Added `pg_sequence` / `pg_collation`
  (present-but-empty) + `pg_type.typcollation`. **Still deferred — full
  `inspect().get_columns()`:** SQLAlchemy *also* fires a domain/type query using a
  **derived-table subquery in FROM** (`JOIN (SELECT … FROM pg_constraint GROUP BY …)
  AS dc`) + `array_agg` + `pg_constraint`; derived-table-as-join-source is the next
  slice. The column query itself + `information_schema.columns` are the working
  paths. Tests: `tests/test_sql_scalar.py`, `tests/test_sql_catalog.py`, and a
  driver-level catalog column query in `tests/test_pgserver_pg8000.py`.
- **Derived tables + array_agg → `get_columns()` end to end. ✅ landed.** The last
  pieces so SQLAlchemy's `inspect().get_columns()` returns typed column metadata:
  (1) a `(SELECT … GROUP BY …) AS alias` **derived table** as a join source is
  planned as a sub-plan (`DerivedTable`) and materialized into an ephemeral
  collection the executor registers (`virtual.CatalogBackend.register_ephemeral`)
  before running the main pipeline; (2) **`array_agg`** lowers to `$push` (argument
  via `_agg_arg_to_expr`, ignoring any intra-aggregate ORDER BY; always-NULL catalog
  funcs like `pg_get_constraintdef` → literal NULL); (3) the empty `pg_constraint` /
  `pg_enum` virtual tables + pg_type domain columns (`typnamespace`/`typtype`/...).
  **Root-cause fix:** a numeric `CAST` now coerces its inner value (`_coerce_cast`),
  so an extended-protocol text-bound param (`attnum > CAST($1 AS SMALLINT)`,
  `$1='0'`) compares numerically instead of as a string (Mongo orders numbers
  before strings, which had silently dropped every joined row). `get_columns` /
  `get_table_names` / `has_table` now all work over pg8000 + SQLAlchemy.
  **Deferred:** full `Table(autoload_with=...)` also needs `get_pk_constraint` /
  `get_indexes` / `get_foreign_keys` (a `pg_index` table + `pg_constraint.conrelid`/
  `confrelid` + PK/index/FK reflected from the catalog). Tests:
  `tests/test_pgserver_pg8000.py::test_sqlalchemy_get_columns_reflection` (headline)
  + the text-bound-cast regression in `tests/test_sql_catalog.py`.
- **Transactions (post-P7). ✅ landed.** `BEGIN`/`COMMIT`/`ROLLBACK` now open /
  commit / abort a real `Storage` user-transaction (the same
  `begin_user_transaction` / `use_user_transaction` / `commit_user_transaction`
  the Mongo multi-document path uses). A `Session` holds the open txn handle;
  every statement inside the block runs within `use_user_transaction` (so its
  WT session sees the in-flight writes and `ROLLBACK` undoes them — DDL
  included), and a statement that errors poisons the block (`25P02` on every
  command until COMMIT/ROLLBACK; COMMIT of an aborted block rolls back and tags
  `ROLLBACK`). The wire `ReadyForQuery` status byte now reflects the block
  (`I` idle / `T` in-transaction / `E` failed) on both the simple and extended
  paths, and a connection that drops mid-block aborts its open transaction.
  Tests: `tests/test_sql_transactions.py` + a real pg8000 commit/rollback over
  the wire. **Deferred:** `SAVEPOINT`, `SET TRANSACTION ISOLATION LEVEL` /
  read-only, and SQL-level `DECLARE CURSOR` (see backlog).

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
