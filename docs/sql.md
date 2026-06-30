# SQL / PostgreSQL interface

SecantusDB can also speak **SQL over the PostgreSQL wire protocol**. The same
WiredTiger data the MongoDB server stores is reachable a second way: a
`SecantusPGServer` accepts connections from PostgreSQL clients and drivers, so a
document written with `pymongo` can be read back as a row with `psql`, pg8000,
or SQLAlchemy — and vice-versa.

It is the SQL analogue of the MongoDB server: where the conformance target there
is `pymongo`, here it is a PostgreSQL client. SQL is compiled down to the same
query / update / aggregation engines the Mongo side uses, so it inherits index
acceleration, the type system, and transactions for free.

:::{note}
The SQL interface is an **opt-in extra**. Install it with:

```console
$ pip install "secantus[sql]"
```

The core MongoDB server never imports the SQL layer, so the base install stays
lean.
:::

## Starting the server

`SecantusPGServer` mirrors `SecantusDBServer`: construct, `start()`, `stop()`,
and a context-manager form. `port=0` picks a free port (handy in tests).

```python
from secantus.sql import SecantusPGServer

with SecantusPGServer(port=5432, storage_path="./secantus-data") as server:
    print(server.uri)          # postgresql://127.0.0.1:5432/postgres
    ...                        # connect and query; the server stops on exit
```

For a long-running daemon, call `start()` and keep the process alive yourself:

```python
server = SecantusPGServer(port=5432, storage_path="./secantus-data")
server.start()
# ... the accept loop runs on a daemon thread; block your main thread here ...
server.stop()
```

The connection's **database** selects the SecantusDB storage database; a SQL
**table** is a collection; a **row** is a document.

### Both protocols, one dataset

Point a `SecantusPGServer` at the *same* `Storage` a `SecantusDBServer` owns and
the two protocols serve the same data live:

```python
from pymongo import MongoClient
from secantus import SecantusDBServer
from secantus.sql import SecantusPGServer

mongo = SecantusDBServer(port=27017)
sql = SecantusPGServer(port=5432, storage=mongo.storage)  # share the store
mongo.start()
sql.start()

# Write through MongoDB...
MongoClient(mongo.uri)["shop"]["products"].insert_one(
    {"_id": 1, "name": "Widget", "price": 9.99}
)
# ...read through SQL (database "shop" -> the same storage db).
```

## Connecting

SecantusDB speaks the PostgreSQL v3 wire protocol, so standard clients connect
over an ordinary `postgresql://` URL.

### pg8000 (pure-Python, no libpq)

```python
import pg8000.dbapi

conn = pg8000.dbapi.connect(user="postgres", host="127.0.0.1", port=5432, database="shop")
cur = conn.cursor()
cur.execute("SELECT 1")
print(cur.fetchall())          # ([1],)
conn.close()
```

### SQLAlchemy

```python
import sqlalchemy as sa

engine = sa.create_engine("postgresql+pg8000://postgres@127.0.0.1:5432/shop")
with engine.connect() as conn:
    rows = conn.execute(sa.text("SELECT name, price FROM products")).fetchall()
```

### psql / psycopg

Any libpq-based client connects too:

```console
$ psql "postgresql://postgres@127.0.0.1:5432/shop" -c "SELECT name FROM products"
```

```python
import psycopg

with psycopg.connect(host="127.0.0.1", port=5432, dbname="shop", user="postgres") as conn:
    rows = conn.execute("SELECT name FROM products WHERE price > %s", (10,)).fetchall()
```

:::{note}
The bundled conformance gauges run **pg8000** (pure-Python, text parameters) and
**psycopg 3** (libpq via the `psycopg[binary]` wheel — the strictest wire exercise:
binary-format parameters, server-side prepared statements, and the psycopg SQLAlchemy
dialect's catalog reflection), each paired with a **SQLAlchemy** Core round-trip. `psql`
and a JVM/JDBC client speak the same protocol but need a system libpq / a JVM, so they
aren't run in CI.
:::

## Declared tables

`CREATE TABLE` records a typed schema in a per-database catalog. The single
`PRIMARY KEY` column maps to the document `_id`, so PK uniqueness rides the
storage layer's `_id` index.

```sql
CREATE TABLE users (
    id    bigint PRIMARY KEY,
    name  text,
    age   int,
    active boolean
);

INSERT INTO users (id, name, age, active) VALUES
    (1, 'alice', 30, true),
    (2, 'bob',   17, false),
    (3, 'carol', 42, true);

SELECT id, name FROM users WHERE age >= 18 ORDER BY name;
--  id | name
-- ----+-------
--   1 | alice
--   3 | carol

UPDATE users SET active = false WHERE id = 1;
DELETE FROM users WHERE age < 18;
SELECT COUNT(*) FROM users;        -- 2
```

Parameterised statements work over the extended protocol (`%s` in pg8000 /
psycopg, `$1` on the wire):

```python
cur.execute("INSERT INTO users (id, name, age) VALUES (%s, %s, %s)", (4, "dave", 25))
cur.execute("SELECT name FROM users WHERE age > %s", (21,))
```

### Type mapping

| SQL type | stored as (BSON) | back out as |
|---|---|---|
| `int` / `integer` | int32 | `int` |
| `bigint` | int64 | `int` |
| `real` / `double precision` | double | `float` |
| `numeric` / `decimal` | Decimal128 | `Decimal` |
| `text` / `varchar` | string | `str` |
| `boolean` | bool | `bool` |
| `timestamptz` / `timestamp` | UTC datetime | `datetime` |
| `json` / `jsonb` | embedded document / array | `dict` / `list` |
| `bytea` | binary | `bytes` |

```sql
CREATE TABLE m (id bigint PRIMARY KEY, price numeric, at timestamptz);
INSERT INTO m (id, price, at) VALUES (1, 19.99, '2020-01-02T03:04:05Z');
SELECT price, at FROM m;
-- price -> Decimal('19.99'),  at -> datetime(2020, 1, 2, 3, 4, 5, tzinfo=UTC)
```

## Querying

`WHERE` supports the common operators; they lower to the same match engine the
MongoDB `find` uses, so an indexed column is index-accelerated.

```sql
SELECT * FROM users WHERE age = 30;
SELECT * FROM users WHERE age >= 18 AND active = true;
SELECT * FROM users WHERE age < 18 OR age > 40;
SELECT * FROM users WHERE id IN (1, 3);
SELECT * FROM users WHERE age BETWEEN 18 AND 40;
SELECT * FROM users WHERE name LIKE 'a%';      -- ILIKE too
SELECT * FROM users WHERE name IS NOT NULL;
SELECT name FROM users ORDER BY age DESC LIMIT 2 OFFSET 1;

-- A comparison between two columns (or a column and an arithmetic expression)
-- is supported; it evaluates per row rather than via an index.
SELECT * FROM orders WHERE shipped_qty < ordered_qty;
SELECT * FROM products WHERE list_price > cost * 1.5;

-- Computed expressions in the SELECT list / ORDER BY: arithmetic, ||, and the
-- common scalar functions evaluate per row.
SELECT name, price * qty AS total, upper(name) AS shout
FROM items
ORDER BY price * qty DESC;
SELECT coalesce(nickname, name) || ' (' || length(name) || ')' AS label FROM users;

-- Non-correlated subqueries in WHERE: IN / NOT IN over a single column, and a
-- scalar `= (SELECT ...)`. The inner query runs first (it may aggregate/filter).
SELECT name FROM customers WHERE id IN (SELECT cust_id FROM orders WHERE total > 100);
SELECT name FROM customers WHERE id = (SELECT max(cust_id) FROM orders);

-- EXISTS / NOT EXISTS and correlated subqueries (the inner query references the
-- outer row) are evaluated per row: each candidate row is tested against the
-- inner query, whose outer-row references resolve to that row. IN and scalar
-- `OP (SELECT ...)` may both be correlated; an aggregate inner projection
-- (`max`/`min`/`sum`/`avg`/`count`) reduces the matching inner rows.
SELECT name FROM customers c WHERE EXISTS (SELECT 1 FROM orders o WHERE o.cust_id = c.id);
SELECT name FROM customers c WHERE NOT EXISTS (SELECT 1 FROM orders o WHERE o.cust_id = c.id);
SELECT name FROM customers c
WHERE c.id = (SELECT max(o.cust_id) FROM orders o WHERE o.region = c.region);
```

Correlated subqueries are limited to a **single-table** outer SELECT (no outer
JOIN / GROUP BY), and the inner query is a simple `SELECT … FROM one_table
[WHERE …]` (no inner join / GROUP BY). The per-row evaluation is a full scan of
the outer table, so it's `O(outer × inner)` — fine for the ephemeral test data
SecantusDB targets, not a query planner.

## Aggregates, GROUP BY, HAVING

`COUNT` / `SUM` / `AVG` / `MIN` / `MAX` compile to an aggregation pipeline
(`$group`).

```sql
SELECT region, COUNT(*) AS n, SUM(amount) AS total, AVG(amount) AS mean
FROM sales
GROUP BY region
HAVING SUM(amount) > 100
ORDER BY total DESC;

SELECT COUNT(*), SUM(amount) FROM sales;          -- whole-table aggregate
SELECT COUNT(id) FROM sales;                      -- COUNT(col) excludes NULLs
```

## Joins

An `INNER` or `LEFT JOIN` with an equality `ON` compiles to a `$lookup`.
Multiple joins chain — each table joins the base or an already-joined table:

```sql
SELECT o.id, o.total, c.name
FROM orders o
JOIN customers c ON o.cust_id = c.id
WHERE c.region = 'east'
ORDER BY o.id;

-- LEFT JOIN keeps unmatched left rows with NULLs on the right:
SELECT o.id, c.name
FROM orders o
LEFT JOIN customers c ON o.cust_id = c.id;

-- Three (or more) tables — products joins via orders.prod_id:
SELECT c.name, p.pname
FROM orders o
JOIN customers c ON o.cust_id = c.id
JOIN products  p ON o.prod_id = p.id
ORDER BY c.name;

-- JOIN combined with GROUP BY / aggregates / HAVING — the canonical analytics
-- query. WHERE filters joined rows before grouping; HAVING filters after:
SELECT c.region, SUM(o.total) AS revenue
FROM orders o
JOIN customers c ON o.cust_id = c.id
WHERE o.total > 0
GROUP BY c.region
HAVING SUM(o.total) > 1000
ORDER BY c.region;
```

## SELECT DISTINCT

`SELECT DISTINCT` dedups on the projected columns (single-table or over a join):

```sql
SELECT DISTINCT region FROM sales ORDER BY region;
SELECT DISTINCT region, status FROM orders;
SELECT DISTINCT c.name FROM orders o JOIN customers c ON o.cust_id = c.id;
```

## Reflected tables and jsonb (the dual-protocol payoff)

A collection with **no `CREATE TABLE`** is still queryable. SecantusDB samples
the documents, infers a column and type per top-level field, and presents a
read-only, schema-on-read view. Nested documents and arrays surface as `jsonb`,
and missing fields read as `NULL`.

```python
# Written through MongoDB — no SQL DDL at all:
MongoClient(mongo.uri)["shop"]["people"].insert_many([
    {"_id": 1, "name": "alice", "profile": {"city": "NYC", "tags": ["a", "b"]}},
    {"_id": 2, "name": "bob",   "profile": {"city": "LA"}},
    {"_id": 3, "name": "carol"},                       # no profile
])
```

```sql
-- Read through SQL (connected to database "shop"):
SELECT * FROM people ORDER BY _id;
--  _id | name  | profile
-- -----+-------+----------------------------------
--    1 | alice | {"city": "NYC", "tags": ["a","b"]}
--    2 | bob   | {"city": "LA"}
--    3 | carol | NULL

-- jsonb navigation: -> (returns jsonb), ->> (returns text), #> (path)
SELECT name, profile->>'city' AS city FROM people ORDER BY _id;
SELECT name FROM people WHERE profile->>'city' = 'LA';
SELECT profile->'tags' AS tags FROM people WHERE _id = 1;   -- ["a", "b"]
SELECT profile #> '{city}'   AS c FROM people WHERE _id = 2; -- LA
```

`->`/`->>`/`#>` also work on a declared `jsonb` column. A declared table always
shadows reflection.

### jsonb containment, existence, and functions

The containment and key-existence operators are supported in `WHERE` (they
compile to Mongo filters), along with the common `jsonb_*` functions:

```sql
-- containment (@>): object keys, array membership, scalars
SELECT _id FROM docs WHERE data @> '{"a": 1}';
SELECT _id FROM docs WHERE data @> '{"tags": ["y"]}';   -- array contains "y"

-- key / element existence
SELECT _id FROM docs WHERE data ? 'c';                  -- has top-level key "c"
SELECT _id FROM docs WHERE data ?| array['b', 'c'];     -- any of these keys
SELECT _id FROM docs WHERE data ?& array['a', 'b'];     -- all of these keys

-- builders, length, type, and set-returning functions
SELECT jsonb_build_object('k', 5) AS o;
SELECT jsonb_build_array(1, 2, 3) AS a;
SELECT jsonb_array_length(data #> '{tags}') FROM docs WHERE _id = 1;
SELECT jsonb_typeof(data) FROM docs WHERE _id = 1;       -- 'object'
SELECT jsonb_array_elements((data->'tags')) FROM docs;   -- one row per element
SELECT jsonb_object_keys(data) FROM docs;                -- one row per key
```

Two caveats. `<@` (contained-by) is **not supported** — "this field is a subset
of a constant" can't be pushed down as a filter; rewrite it as `'<const>' @>
field` where possible. And because sqlglot reads a bare `->` inside a function
call as a lambda arrow, a *navigated function argument* must be parenthesised
(`jsonb_array_length((data->'tags'))`) or use the `#>` form
(`jsonb_array_length(data #> '{tags}')`); bare `->` in `WHERE`/projection is
unaffected.

Reflected collections aren't limited to plain `SELECT` — **`GROUP BY`,
aggregates, `HAVING`, and `JOIN` all work over `pymongo`-written data** with no
DDL, so you can run SQL analytics directly against documents:

```sql
-- "sales" and "people" were written through MongoDB, never declared:
SELECT region, SUM(amount) AS total
FROM sales
GROUP BY region
ORDER BY region;

-- A reflected collection exposes the Mongo field names, so a join keys off
-- "_id" (there is no DDL-declared "id" column):
SELECT p.item, c.name
FROM purchases p
JOIN people c ON p.buyer = c._id;
```

One caveat: in a join, qualify references to fields that may not appear in the
sampled rows (`c.name`, not a bare `name`) so the planner can route them to the
right reflected table.

### Writing to reflected collections

Reflected tables are **read-write**: `INSERT`, `UPDATE`, and `DELETE` reach a
`pymongo`-written collection with no `CREATE TABLE`. The change is a genuine
MongoDB document mutation — visible immediately through `pymongo` — which is the
other half of the dual-protocol payoff:

```sql
-- "people" exists only as a Mongo collection, never declared:
INSERT INTO people (_id, name, age) VALUES (3, 'dave', 40);
UPDATE people SET age = 41 WHERE name = 'dave';
DELETE FROM people WHERE age < 18;
```

A field that wasn't in the sampled rows is still a valid write target (it stores
as-is). The reflected primary key is the Mongo `_id`: it's `NOT NULL` (an
`INSERT` must supply it — there's no server-side auto-id through SQL) and
immutable (`SET _id = …` is rejected). Writing to a collection that doesn't
exist yet returns `undefined_table` — `CREATE TABLE` it first, or create it
through `pymongo`.

### RETURNING

`INSERT`, `UPDATE`, and `DELETE` accept a `RETURNING` clause that projects the
affected rows back as a result set — the same projection vocabulary as a SELECT
list (`*`, columns, aliases, jsonb navigation). `INSERT` returns the inserted
rows, `UPDATE` the **post-image** of the updated rows, and `DELETE` the deleted
rows. Works on declared and reflected tables alike:

```sql
INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b') RETURNING id, name;
UPDATE t SET n = n + 1 WHERE id = 1 RETURNING id, n;     -- the new n
DELETE FROM t WHERE n > 100 RETURNING *;
```

`RETURNING` is limited to the projection vocabulary above (no computed
expressions); the rows reflect the values actually stored.

## Indexes

`CREATE INDEX` (optionally `UNIQUE`) maps to a real Mongo secondary index on the
underlying collection; the query planner then accelerates matching `WHERE` /
`ORDER BY` exactly as it does for indexes created through the MongoDB API. The
primary-key column maps to the `_id` index. `DROP INDEX` removes it.

```sql
CREATE INDEX ix_age ON users (age);
CREATE UNIQUE INDEX ux_email ON users (email);
CREATE INDEX ix_name_desc ON users (name DESC);
DROP INDEX ix_age;
```

## Transactions

`BEGIN` / `COMMIT` / `ROLLBACK` open a real storage transaction: statements in
the block run atomically, `ROLLBACK` undoes them (DDL included), and an error
poisons the block until it ends (Postgres' aborted-transaction semantics).

```sql
BEGIN;
INSERT INTO accounts (id, balance) VALUES (1, 100);
UPDATE accounts SET balance = balance WHERE id = 1;
ROLLBACK;            -- the INSERT is undone

BEGIN;
INSERT INTO accounts (id, balance) VALUES (2, 50);
COMMIT;              -- persisted
```

```python
conn.autocommit = False
cur.execute("INSERT INTO accounts (id, balance) VALUES (3, 10)")
conn.rollback()      # discarded
cur.execute("INSERT INTO accounts (id, balance) VALUES (4, 20)")
conn.commit()        # kept
```

After a failed statement inside a block, every command except `COMMIT` /
`ROLLBACK` returns SQLSTATE `25P02` until the block ends; a `COMMIT` of an
aborted block rolls back.

`SET TRANSACTION ISOLATION LEVEL …` / `… READ ONLY` / `… READ WRITE`,
`SET SESSION CHARACTERISTICS AS TRANSACTION …`, and `BEGIN ISOLATION LEVEL …`
are accepted but are no-ops: SecantusDB is single-node, so isolation level and
read-only mode don't change behaviour. `SAVEPOINT` / `RELEASE` /
`ROLLBACK TO SAVEPOINT` are not yet supported.

## Authentication and TLS

By default the server trusts every connection (matching the Mongo server's
`require_auth=False` default). Turn on **SCRAM-SHA-256** by supplying users:

```python
server = SecantusPGServer(port=5432, require_auth=True, users={"alice": "s3cret"})
```

```python
pg8000.dbapi.connect(user="alice", password="s3cret", host="127.0.0.1", port=5432, database="db")
```

**TLS** is enabled by passing a certificate and key; the server answers the
client's `SSLRequest` and wraps the socket:

```python
server = SecantusPGServer(
    port=5432,
    tls_cert_file="server.pem",
    tls_key_file="server.key",
)
```

```python
import ssl
ctx = ssl.create_default_context(cafile="ca.pem")
pg8000.dbapi.connect(user="alice", host="127.0.0.1", port=5432, database="db", ssl_context=ctx)
```

## Session and catalog introspection

Common session functions and settings resolve against the connection:

```sql
SELECT version();
SELECT current_database();
SELECT current_user;
SELECT current_setting('search_path');
SHOW search_path;
SET search_path TO myschema;
```

Programmatic schema discovery works through `information_schema` and `pg_catalog`,
including joins across the catalogs (so SQLAlchemy's `get_table_names()` /
`has_table()` and `psql \dt` work):

```sql
SELECT table_name FROM information_schema.tables WHERE table_schema = 'public';
SELECT column_name, data_type, is_nullable
FROM information_schema.columns WHERE table_name = 'users';
SELECT relname FROM pg_catalog.pg_class;

-- pg_catalog column metadata via a join (relid lines up across catalogs):
SELECT a.attname, a.atttypid, a.attnotnull
FROM pg_catalog.pg_attribute a
JOIN pg_catalog.pg_class c ON a.attrelid = c.oid
WHERE c.relname = 'users'
ORDER BY a.attnum;
```

`pg_attribute` / `pg_attrdef` / `pg_description` (and `pg_sequence` /
`pg_collation`) back column-level introspection. The catalog query SQLAlchemy
and `psql \d` emit for columns — a multi-table outer join with a compound `ON`,
`format_type(...)` in the SELECT list, correlated scalar subqueries, and `CASE`
— runs end to end:

```sql
SELECT a.attname,
       format_type(a.atttypid, a.atttypmod) AS type,
       (SELECT d.adbin FROM pg_catalog.pg_attrdef d
        WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum) AS default,
       a.attnotnull AS not_null
FROM pg_catalog.pg_class c
LEFT OUTER JOIN pg_catalog.pg_attribute a
  ON c.oid = a.attrelid AND a.attnum > 0 AND NOT a.attisdropped
WHERE c.relname = 'users'
ORDER BY a.attnum;
```

Scalar SELECT-list functions (`format_type`, `pg_get_expr`, `coalesce`),
`CASE`, comparisons, and correlated scalar subqueries are evaluated per row;
compound join `ON`s (multi-key joins and residual predicates) compile to a
`$lookup` sub-pipeline; and a `(SELECT … GROUP BY …) AS alias` derived table in
the `FROM` clause is materialized into an ephemeral collection. With those,
**SQLAlchemy's `inspect().get_columns()` works end to end** and returns typed
column metadata:

**Full SQLAlchemy reflection works end to end**, including primary keys and
indexes (`get_pk_constraint` / `get_indexes` use `unnest` / `generate_subscripts`
set-returning functions plus `array_agg` over a derived table — all supported):

```python
insp = sqlalchemy.inspect(engine)
insp.get_table_names()          # ['users', ...]
insp.has_table('users')         # True
insp.get_columns('users')       # [{'name': 'id', 'type': BIGINT(), 'nullable': False, ...}, ...]
insp.get_pk_constraint('users') # {'constrained_columns': ['id'], 'name': 'users_pkey', ...}
insp.get_indexes('users')       # [{'name': 'ix_name', 'column_names': ['name'], 'unique': False, ...}]

# Whole-table autoload reflects columns, the primary key, and indexes:
users = sqlalchemy.Table('users', sqlalchemy.MetaData(), autoload_with=engine)
```

`get_foreign_keys()` reflects empty, since SecantusDB models no foreign-key
constraints. Column comments aren't stored, so they reflect as `None`.

## Supported SQL

| Area | Supported | Not yet |
|---|---|---|
| DML | `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `RETURNING` | `MERGE`, `INSERT ... SELECT` |
| `WHERE` | `=` `<>` `<` `<=` `>` `>=`, `IN`, `BETWEEN`, `LIKE`/`ILIKE`, `IS [NOT] NULL`, `AND`/`OR`/`NOT`, jsonb `@>`/`?`/`?\|`/`?&`, column-to-column + arithmetic, `IN`/`NOT IN`/scalar `OP (SELECT …)` subqueries (correlated or not), `EXISTS`/`NOT EXISTS` | correlated subqueries with an outer JOIN/GROUP BY, function calls in a comparison, jsonb `<@` |
| Projection | columns, `*`, aliases, `jsonb` paths, `jsonb_*` functions, `DISTINCT`, computed expressions (arithmetic, `\|\|`, `upper`/`lower`/`length`/`substring`/`round`/`coalesce`/`greatest`/...) | computed GROUP BY keys, expressions over an aggregate, window functions |
| Aggregates | `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`, `GROUP BY`, `HAVING` | window functions, `GROUPING SETS` |
| Joins | multi-table `INNER`/`LEFT JOIN`, equality `ON`, JOIN + GROUP BY / aggregates / HAVING | `RIGHT`/`FULL`/`CROSS`, non-equi / `OR` `ON` |
| DDL | `CREATE TABLE`, `DROP TABLE`, `CREATE`/`DROP INDEX` (incl. `UNIQUE`) | `ALTER TABLE`, views, constraints (enforced) |
| Transactions | `BEGIN`/`COMMIT`/`ROLLBACK`, `SET TRANSACTION` / `BEGIN ISOLATION LEVEL`, `SAVEPOINT`/`RELEASE`/`ROLLBACK TO` (accepted, single-node no-op) | true nested savepoint rollback, `DECLARE CURSOR` |
| Protocol | simple + extended query, `$1` params (text + binary), prepared statements, portals, binary result format | `COPY`, `DECLARE CURSOR` |
| Auth | trust, SCRAM-SHA-256, TLS | channel binding, mTLS, SQL `CREATE ROLE` |
| Catalog | `information_schema`, `pg_catalog` (`pg_index`/`pg_constraint`/`pg_am`/...), catalog *joins*, full SQLAlchemy reflection (`get_table_names`/`has_table`/`get_columns`/`get_pk_constraint`/`get_indexes`/`get_foreign_keys`, `Table(autoload_with=...)`) | column comments, FK reflection (no FKs modeled) |

Anything outside the supported set returns a faithful SQLSTATE error rather than
a wrong answer — the same "honest *not supported* over a half-feature" discipline
the [compatibility](compatibility.md) page describes for the MongoDB side.
