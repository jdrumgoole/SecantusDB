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

:::{note}
The bundled conformance gauge runs **pg8000** and a **SQLAlchemy** Core
round-trip (they are pure-Python, so they run in CI). libpq-based clients
(`psql`, `psycopg`) use the same wire protocol but aren't exercised in CI.
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
```

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

A two-table `INNER` or `LEFT JOIN` with an equality `ON` compiles to a
`$lookup`.

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

`->`/`->>`/`#>` also work on a declared `jsonb` column. A reflected table is
**read-only**; `CREATE TABLE` a collection to write to it through SQL, and a
declared table always shadows reflection.

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

Programmatic schema discovery works through `information_schema` and a subset of
`pg_catalog` (no-join queries):

```sql
SELECT table_name FROM information_schema.tables WHERE table_schema = 'public';
SELECT column_name, data_type, is_nullable
FROM information_schema.columns WHERE table_name = 'users';
SELECT relname FROM pg_catalog.pg_class;
```

## Supported SQL

| Area | Supported | Not yet |
|---|---|---|
| DML | `SELECT`, `INSERT`, `UPDATE`, `DELETE` | `MERGE`, `INSERT ... SELECT`, `RETURNING` |
| `WHERE` | `=` `<>` `<` `<=` `>` `>=`, `IN`, `BETWEEN`, `LIKE`/`ILIKE`, `IS [NOT] NULL`, `AND`/`OR`/`NOT` | subqueries, scalar expressions, column-to-column predicates |
| Projection | columns, `*`, aliases, `jsonb` paths | computed expressions, `DISTINCT` |
| Aggregates | `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`, `GROUP BY`, `HAVING` | window functions, `GROUPING SETS` |
| Joins | single two-table `INNER`/`LEFT JOIN`, equality `ON` | 3+ tables, `RIGHT`/`FULL`/`CROSS`, non-equi, JOIN + GROUP BY |
| DDL | `CREATE TABLE`, `DROP TABLE`, `CREATE INDEX` | `ALTER TABLE`, views, constraints (enforced) |
| Transactions | `BEGIN`/`COMMIT`/`ROLLBACK` | `SAVEPOINT`, isolation levels, `DECLARE CURSOR` |
| Protocol | simple + extended query, `$1` params, prepared statements, portals | binary result format, `COPY` |
| Auth | trust, SCRAM-SHA-256, TLS | channel binding, mTLS, SQL `CREATE ROLE` |
| Catalog | `information_schema`, `pg_catalog` (no-join) | catalog *joins* (interactive `psql \d`, full ORM reflection) |

Anything outside the supported set returns a faithful SQLSTATE error rather than
a wrong answer — the same "honest *not supported* over a half-feature" discipline
the [compatibility](compatibility.md) page describes for the MongoDB side.
