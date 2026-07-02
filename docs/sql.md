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

`INSERT` also accepts a query as its source — `INSERT INTO target [(cols)]
SELECT …`. The source runs first (it may filter, join, aggregate, or be a set
operation / CTE) and its result columns map positionally onto the target
columns, coerced to each target column's type:

```sql
INSERT INTO archived_users (id, name) SELECT id, name FROM users WHERE active = false;
INSERT INTO region_totals (region, total) SELECT region, SUM(amount) FROM sales GROUP BY region;
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

### Evolving a table (`ALTER TABLE`)

`ALTER TABLE` rewrites the catalog entry and, where the data must follow, the
backing collection:

```sql
ALTER TABLE users ADD COLUMN email text;            -- new field, reads NULL until set
ALTER TABLE users ADD COLUMN score int NOT NULL;    -- marks the column non-nullable
ALTER TABLE users DROP COLUMN age;                  -- $unsets the field on every doc
ALTER TABLE users RENAME COLUMN name TO full_name;  -- $renames the field
ALTER TABLE users ALTER COLUMN email SET NOT NULL;  -- / DROP NOT NULL
ALTER TABLE users ALTER COLUMN score TYPE bigint;   -- retype in the catalog
ALTER TABLE users ALTER COLUMN score SET DEFAULT 0; -- / DROP DEFAULT
ALTER TABLE users ADD CONSTRAINT uq_email UNIQUE (email);   -- declared, not enforced
ALTER TABLE users ADD CONSTRAINT ck_score CHECK (score >= 0);
ALTER TABLE users DROP CONSTRAINT ck_score;         -- drops any FK / CHECK / UNIQUE by name
ALTER TABLE users RENAME TO members;                -- renames the table + collection
```

Supported actions: `ADD COLUMN [IF NOT EXISTS]`, `DROP COLUMN [IF EXISTS]`,
`RENAME COLUMN`, `RENAME TO`, `ALTER COLUMN … SET/DROP NOT NULL`, `ALTER COLUMN
… TYPE t`, `ALTER COLUMN … SET/DROP DEFAULT`, `ADD [CONSTRAINT name] { FOREIGN
KEY (…) REFERENCES … | CHECK (…) | UNIQUE (…) }` (declared, not enforced — like a
CREATE TABLE constraint), and `DROP CONSTRAINT [IF EXISTS] name` (removes a
declared FK / CHECK / UNIQUE). `ALTER TABLE IF EXISTS` on a
missing table is a no-op. Dropping the `PRIMARY KEY` column is rejected (it maps
to `_id`); renaming it changes only the SQL name — the field stays `_id`. A
`TYPE` change retypes the column in the catalog (new inserts/reads use it;
already-stored values keep their BSON type — no rewrite). Multiple actions in
one statement are not supported (sqlglot parses a comma-separated action list as
an opaque command); issue one action per statement.

### Column DEFAULTs

A literal column `DEFAULT` (a number, string, boolean, or `NULL`) — declared in
`CREATE TABLE` or via `ALTER COLUMN … SET DEFAULT` — is filled in when an
`INSERT` omits the column:

```sql
CREATE TABLE t (id bigint PRIMARY KEY, n int DEFAULT 5, s text DEFAULT 'hi');
INSERT INTO t (id) VALUES (1);        -- n -> 5, s -> 'hi'
```

A non-literal default (e.g. `DEFAULT now()`) is accepted but not applied — the
column reads `NULL` when omitted.

### Foreign keys (declared, not enforced)

Column-level `REFERENCES` and table-level `FOREIGN KEY` are recorded in the
catalog and surfaced through reflection, so ORMs and migration tools see the
relationships. SecantusDB does **not** enforce referential integrity on write —
a foreign key here is a schema-shape record, not a runtime guard.

```sql
CREATE TABLE users (id bigint PRIMARY KEY, name text);

CREATE TABLE orders (
    id      bigint PRIMARY KEY,
    user_id bigint REFERENCES users(id) ON DELETE CASCADE,   -- column-level
    total   int
);

CREATE TABLE items (
    id       bigint PRIMARY KEY,
    order_id bigint,
    FOREIGN KEY (order_id) REFERENCES orders(id)             -- table-level
);
```

Foreign keys reflect through the standard catalogs:
`information_schema.referential_constraints` / `.table_constraints` /
`.key_column_usage` / `.constraint_column_usage`, and `pg_catalog.pg_constraint`
(`contype = 'f'`) with `pg_get_constraintdef()` rendering the `FOREIGN KEY (…)
REFERENCES …` text. SQLAlchemy's inspector reflects them end to end:

```python
insp = sqlalchemy.inspect(engine)
insp.get_foreign_keys("orders")
# [{'name': 'orders_user_id_fkey', 'constrained_columns': ['user_id'],
#   'referred_table': 'users', 'referred_columns': ['id'],
#   'options': {'ondelete': 'CASCADE'}, ...}]
```

`ON DELETE` / `ON UPDATE` actions are recorded and reflected but never acted on.
`REFERENCES t` with no column list targets `t`'s primary key. A foreign key can
also be added after the fact with `ALTER TABLE … ADD [CONSTRAINT name] FOREIGN
KEY (…) REFERENCES …`.

### CHECK and UNIQUE constraints (declared, not enforced)

`CHECK` and `UNIQUE` constraints — column-level, table-level, named or unnamed —
are recorded in the catalog and reflected, but (like foreign keys) **not
enforced**: SecantusDB does not validate a `CHECK` predicate or reject a
duplicate `UNIQUE` value on write.

```sql
CREATE TABLE t (
    id     bigint PRIMARY KEY,
    email  text UNIQUE,                       -- column-level UNIQUE
    age    int CHECK (age >= 0),              -- column-level CHECK
    status text,
    CONSTRAINT uq_es UNIQUE (email, status),  -- named table-level UNIQUE
    CONSTRAINT ck_age CHECK (age < 200),      -- named table-level CHECK
    UNIQUE (status)                           -- unnamed table-level UNIQUE
);
```

`CHECK`, `NOT NULL`, `UNIQUE`, and `FOREIGN KEY` are **enforced** on write. An
`INSERT` or `UPDATE` that would leave a row violating a declared `CHECK`
predicate (`23514`), a `NOT NULL` column (`23502`), a `UNIQUE` constraint
(`23505`), or a `FOREIGN KEY` (`23503`) is rejected and the table is left
unchanged. A `CHECK` whose predicate evaluates to NULL (unknown) passes, and
NULLs are distinct in a `UNIQUE` constraint (multiple NULLs allowed) — both
matching Postgres.

`FOREIGN KEY` enforcement covers both sides: a child `INSERT`/`UPDATE` whose FK
columns are all non-NULL requires a matching parent row (MATCH SIMPLE — a NULL in
any FK column exempts the row), and `DELETE`/`UPDATE` of a referenced parent row
applies the declared referential action — `NO ACTION` / `RESTRICT` reject,
`ON DELETE CASCADE` deletes the children (recursively), `SET NULL` / `SET DEFAULT`
clear the child columns:

```sql
CREATE TABLE users  (id bigint PRIMARY KEY, name text);
CREATE TABLE orders (id bigint PRIMARY KEY,
                     uid bigint REFERENCES users(id) ON DELETE CASCADE);

INSERT INTO orders (id, uid) VALUES (1, 999);  -- 23503: no such user
DELETE FROM users WHERE id = 1;                -- also deletes user 1's orders
```

Unnamed constraints get Postgres' default names (`<table>_<col>_key`,
`<table>_<col>_check`). They reflect through `pg_catalog.pg_constraint`
(`contype = 'u'` / `'c'`, each `UNIQUE` backed by an implicit unique index),
`information_schema.table_constraints` / `.check_constraints` /
`.key_column_usage`, and `pg_get_constraintdef()`. SQLAlchemy's inspector
reflects them end to end:

```python
insp = sqlalchemy.inspect(engine)
insp.get_unique_constraints("t")
# [{'name': 't_email_key', 'column_names': ['email'], ...},
#  {'name': 'uq_es', 'column_names': ['email', 'status'], ...}, ...]
insp.get_check_constraints("t")
# [{'name': 'ck_age', 'sqltext': 'age < 200', ...},
#  {'name': 't_age_check', 'sqltext': 'age >= 0', ...}]
```

### Comments (`COMMENT ON`)

`COMMENT ON TABLE` / `COMMENT ON COLUMN` attach a description that reflects
through `pg_description` — SQLAlchemy's `get_table_comment()` and the `comment`
field of `get_columns()`:

```sql
COMMENT ON TABLE users IS 'application users';
COMMENT ON COLUMN users.email IS 'primary contact address';
COMMENT ON COLUMN users.email IS NULL;   -- remove the comment
```

## Views (`CREATE VIEW`)

A view is a stored `SELECT` that reads like the table it stands for. `CREATE
VIEW` records the query text; any reference to the view in a `FROM` / `JOIN`
expands inline as a subquery, so single-table reads, aggregates, joins against
real tables, and views built on other views all work:

```sql
CREATE VIEW active_users AS SELECT id, name FROM users WHERE age >= 18;
CREATE OR REPLACE VIEW active_users AS SELECT id, name, email FROM users WHERE age >= 21;

SELECT count(*) FROM active_users;                 -- reads through to `users`
SELECT a.name FROM active_users a JOIN orders o ON o.user_id = a.id;

DROP VIEW active_users;
DROP VIEW IF EXISTS active_users;                  -- no error if absent
```

Views reflect through `pg_class` (`relkind = 'v'`), `pg_get_viewdef()`, and
`information_schema.views`, so SQLAlchemy's `get_view_names()` and
`get_view_definition()` see them. Views are read-only (no `INSERT`/`UPDATE`
through a view) and are not materialized — each query re-reads the underlying
tables.

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
-- NULL placement follows Postgres: ASC orders NULLs last, DESC orders them
-- first, and NULLS FIRST / NULLS LAST override (across every query shape).
SELECT name FROM users ORDER BY age NULLS FIRST;

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
-- scalar `OP (SELECT ...)`. The inner query runs first (it may aggregate/filter).
-- These work in every query shape — a plain SELECT, or one that also JOINs /
-- GROUP BYs / has computed columns.
SELECT name FROM customers WHERE id IN (SELECT cust_id FROM orders WHERE total > 100);
SELECT name FROM customers WHERE id = (SELECT max(cust_id) FROM orders);
SELECT c.region, sum(o.total) FROM orders o JOIN customers c ON o.cust_id = c.id
WHERE o.total > (SELECT avg(total) FROM orders) GROUP BY c.region;

-- EXISTS / NOT EXISTS and correlated subqueries (the inner query references the
-- outer row) are evaluated per row: each candidate row is tested against the
-- inner query, whose outer-row references resolve to that row. IN and scalar
-- `OP (SELECT ...)` may both be correlated; an aggregate inner projection
-- (`max`/`min`/`sum`/`avg`/`count`) reduces the matching inner rows.
SELECT name FROM customers c WHERE EXISTS (SELECT 1 FROM orders o WHERE o.cust_id = c.id);
SELECT name FROM customers c WHERE NOT EXISTS (SELECT 1 FROM orders o WHERE o.cust_id = c.id);
SELECT name FROM customers c
WHERE c.id = (SELECT max(o.cust_id) FROM orders o WHERE o.region = c.region);

-- A correlated / EXISTS WHERE also works when the outer query JOINs or GROUP BYs:
SELECT o.id, c.name FROM orders o JOIN customers c ON o.cust_id = c.id
WHERE EXISTS (SELECT 1 FROM shipments s WHERE s.order_id = o.id);
SELECT c.region, count(*) FROM customers c
WHERE EXISTS (SELECT 1 FROM orders o WHERE o.cust_id = c.id) GROUP BY c.region;
```

The correlated WHERE is evaluated per row: in a JOIN it filters the joined rows
after the join; in a GROUP BY it filters the base rows *before* grouping (so
only the survivors are grouped). When a query has **both** a JOIN and a GROUP BY,
the WHERE filters the joined rows after the join and before the `$group` — again,
only the survivors are grouped. The inner query is a simple `SELECT … FROM
one_table [WHERE …]` (no inner join / GROUP BY). The per-row evaluation is a full
scan, so it's `O(outer × inner)` — fine for the ephemeral test data SecantusDB
targets, not a query planner. Combining a correlated WHERE with a JOIN, a
GROUP BY, **and** a window function all in one SELECT is not yet supported.

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

`DISTINCT` inside an aggregate is supported for `COUNT` / `SUM` / `AVG` (and is a
no-op for `MIN` / `MAX`, which are unaffected by duplicates). It deduplicates the
non-NULL values within each group before applying the function:

```sql
SELECT COUNT(DISTINCT customer_id) AS unique_buyers FROM orders;
SELECT region, COUNT(DISTINCT product) AS skus, SUM(DISTINCT price) AS price_sum
FROM sales GROUP BY region;
```

(`DISTINCT` inside an aggregate is not yet supported in a `HAVING` clause.)

### GROUPING SETS / ROLLUP / CUBE

Multi-grouping aggregation produces the union of several groupings in one query;
a column absent from a given grouping reads `NULL` in those rows:

```sql
-- per-region subtotals + a grand total (region NULL)
SELECT region, SUM(amount) FROM sales GROUP BY ROLLUP(region);

-- (region, city), (region), () — a subtotal hierarchy
SELECT region, city, SUM(amount) FROM sales GROUP BY ROLLUP(region, city);

-- every combination: (r,c), (r), (c), ()
SELECT region, city, SUM(amount) FROM sales GROUP BY CUBE(region, city);

-- exactly the listed groupings
SELECT region, city, SUM(amount)
FROM sales GROUP BY GROUPING SETS ((region), (city), ());
```

A leading plain `GROUP BY a, ROLLUP(b)` keeps `a` in every grouping set. These
are single-table only (a `JOIN`, `HAVING`, `DISTINCT` aggregate, or window over
GROUPING SETS is rejected); the `GROUPING()` helper function isn't modeled.

## Joins

An `INNER` or `LEFT JOIN` compiles to a `$lookup`. The `ON` may be an equality
(index-accelerated), a multi-condition `AND`, or a non-equi / `OR` predicate
(evaluated per candidate pair). `CROSS JOIN` and the implicit comma form
(`FROM a, b`) produce a cartesian product. Multiple joins chain — each table
joins the base or an already-joined table. `RIGHT` and `FULL OUTER` joins are
supported between **two** tables:

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

-- CROSS JOIN (and the comma form) is the cartesian product; a non-equi or OR
-- ON condition is evaluated per candidate pair:
SELECT a.x, b.y FROM a CROSS JOIN b;
SELECT a.x, b.y FROM a, b WHERE a.k = b.k;
SELECT o.id, t.bracket FROM orders o JOIN tax t ON o.total BETWEEN t.lo AND t.hi;

-- RIGHT keeps unmatched right rows; FULL OUTER keeps unmatched rows from both
-- sides (two-table only — a chain mixing in a RIGHT/FULL is rejected):
SELECT c.name, o.id
FROM orders o
RIGHT JOIN customers c ON o.cust_id = c.id;

SELECT c.name, o.id
FROM orders o
FULL JOIN customers c ON o.cust_id = c.id;

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

`DISTINCT ON (exprs)` keeps the first row per distinct value of `exprs`, in the
query's `ORDER BY` order — the idiomatic "one row per group" (e.g. the newest
order per customer). The `ORDER BY` should lead with the `DISTINCT ON`
expressions so the surviving row is well-defined:

```sql
-- highest-amount sale per region
SELECT DISTINCT ON (region) region, amount
FROM sales ORDER BY region, amount DESC;

-- across a join
SELECT DISTINCT ON (c.name) c.name, o.total
FROM orders o JOIN customers c ON o.cust_id = c.id
ORDER BY c.name, o.total DESC;
```

## LATERAL joins

A `LATERAL` subquery may reference columns from the FROM items to its left, so
it runs once per outer row — the standard way to expand related rows or take a
top-N per group. Correlate inside the subquery's `WHERE`:

```sql
-- expand each customer into its orders
SELECT c.name, o.total
FROM customers c, LATERAL (SELECT total FROM orders WHERE orders.cust_id = c.id) o;

-- top-3 orders per customer
SELECT c.name, o.total
FROM customers c
CROSS JOIN LATERAL (
    SELECT total FROM orders WHERE orders.cust_id = c.id ORDER BY total DESC LIMIT 3
) o
ORDER BY c.name, o.total DESC;

-- LEFT JOIN LATERAL keeps customers with no orders (lateral columns read NULL)
SELECT c.name, o.total
FROM customers c
LEFT JOIN LATERAL (
    SELECT total FROM orders WHERE orders.cust_id = c.id ORDER BY total DESC LIMIT 1
) o ON true;
```

The subquery is single-table with an optional `WHERE` / `ORDER BY` / `LIMIT`; it
lowers to a correlated `$lookup`. `JOIN LATERAL … ON <cond>` must use `ON true`
(the correlation lives in the subquery's `WHERE`); a `LATERAL` subquery
containing a join, `GROUP BY`, or aggregate is not supported.

## Set operations

`UNION`, `INTERSECT`, and `EXCEPT` combine the rows of two (or more, chained)
queries. The plain forms are `DISTINCT`; the `ALL` forms keep multiplicities
(`INTERSECT ALL` → the min of the two counts, `EXCEPT ALL` → left minus right).
Output column names come from the **first** query, and the arms must have the
same number of columns (a mismatch is a `42601` error). A trailing `ORDER BY`
(by output-column name or ordinal position) and `LIMIT` / `OFFSET` apply to the
combined result:

```sql
SELECT region FROM sales_2023 UNION SELECT region FROM sales_2024 ORDER BY region;
SELECT id FROM active EXCEPT SELECT id FROM banned;
SELECT sku FROM warehouse_a INTERSECT SELECT sku FROM warehouse_b;
SELECT n FROM a UNION ALL SELECT n FROM b ORDER BY 1 LIMIT 10;
```

The combine happens in Python over each arm's result rows, so it composes with
any query the arms can express (joins, aggregates, subqueries).

## Common table expressions (WITH)

A `WITH name AS (...) [, ...] <query>` prefix defines one or more named,
non-recursive CTEs. Each CTE is materialized to rows once and then resolves like
a table in the main query — so a CTE composes with everything: filters, joins,
`GROUP BY`, and set operations. CTEs materialize in order, so a later one may
reference an earlier one. The CTE name is scoped to its statement:

```sql
WITH recent AS (SELECT * FROM orders WHERE created > '2024-01-01')
SELECT region, count(*) FROM recent GROUP BY region;

-- chained, and joined against a real table:
WITH big AS (SELECT cust_id, total FROM orders WHERE total > 100),
     vip AS (SELECT cust_id FROM big GROUP BY cust_id HAVING count(*) > 3)
SELECT c.name FROM vip JOIN customers c ON vip.cust_id = c.id;
```

`WITH RECURSIVE` is supported: a recursive CTE is a `UNION [ALL]` of an anchor
(seed) term and a recursive term that references the CTE. It's evaluated by
semi-naive iteration — run the anchor, then repeatedly run the recursive term
against just the rows the previous step produced until it yields nothing new.
`UNION` dedups against all rows seen (so a cyclic graph terminates); `UNION ALL`
keeps every row and is guarded against runaway recursion. Optional column
aliases (`name(a, b)`) rename the output.

```sql
-- generate a series 1..5
WITH RECURSIVE nums(n) AS (
  SELECT 1
  UNION ALL
  SELECT n + 1 FROM nums WHERE n < 5
)
SELECT n FROM nums;

-- walk an org-chart hierarchy, tracking depth
WITH RECURSIVE chain(id, name, lvl) AS (
  SELECT id, name, 0 FROM emp WHERE id = 1
  UNION ALL
  SELECT e.id, e.name, c.lvl + 1 FROM emp e JOIN chain c ON e.mgr = c.id
)
SELECT id, name, lvl FROM chain ORDER BY id;
```

A `WITH` prefix also works on a write: `WITH cte AS (…) INSERT INTO t SELECT …
FROM cte`, and an `UPDATE` / `DELETE` whose `WHERE` has a subquery over a CTE.

```sql
WITH recent AS (SELECT id FROM events WHERE ts > '2024-01-01')
DELETE FROM events WHERE id IN (SELECT id FROM recent);

WITH totals AS (SELECT cust_id, sum(total) AS spent FROM orders GROUP BY cust_id)
INSERT INTO summary (cust_id, spent) SELECT cust_id, spent FROM totals;
```

## Window functions

`func(...) OVER (PARTITION BY … ORDER BY …)` computes a value per row from its
partition. Supported: `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `NTILE`; the value
functions `FIRST_VALUE` / `LAST_VALUE` / `NTH_VALUE`; the aggregate windows
`SUM` / `COUNT` / `AVG` / `MIN` / `MAX`; and `LAG` / `LEAD`. An aggregate window
with no `ORDER BY` aggregates the whole partition; with an `ORDER BY` it's a
running aggregate under the default `RANGE` frame (rows tied on the order key
share the cumulative value):

```sql
SELECT id, region,
       ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount DESC) AS rank_in_region,
       SUM(amount)  OVER (PARTITION BY region)                      AS region_total,
       amount - LAG(amount) OVER (ORDER BY id)                      AS delta
FROM sales;
```

Explicit frames are supported — `ROWS` frames with any
`UNBOUNDED` / `CURRENT ROW` / `n PRECEDING` / `n FOLLOWING` bound, and `RANGE`
frames with `UNBOUNDED` / `CURRENT ROW` bounds (a numeric `RANGE` offset is
rejected — use `ROWS`):

```sql
SELECT id,
       SUM(amount)  OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS sliding,
       SUM(amount)  OVER (ORDER BY id ROWS UNBOUNDED PRECEDING)                 AS running,
       LAST_VALUE(amount) OVER (PARTITION BY region ORDER BY id
                                ROWS BETWEEN UNBOUNDED PRECEDING
                                         AND UNBOUNDED FOLLOWING)               AS region_last
FROM sales;
```

### Window functions over `GROUP BY`

A window function may be computed **over the aggregated rows** of a `GROUP BY`
(or an implicit whole-table aggregation) in the same SELECT — Postgres evaluates
windows after grouping, so a window's arguments, `PARTITION BY`, and `ORDER BY`
can all reference the group aggregates. The grouping runs first; the window then
ranks / accumulates over the grouped rows:

```sql
SELECT region,
       SUM(amount)                              AS region_total,
       RANK() OVER (ORDER BY SUM(amount) DESC)  AS rank_by_total,
       SUM(SUM(amount)) OVER ()                 AS grand_total
FROM sales
GROUP BY region
ORDER BY rank_by_total;
```

An aggregate may nest inside a window aggregate (`SUM(SUM(amount)) OVER ()` —
the grand total of the per-group sums), and `ORDER BY` can reference a window's
output alias. `HAVING` prunes groups before the window sees them. This also works
when the `GROUP BY` spans a `JOIN` — the window then ranks / accumulates over the
grouped rows of the joined tables:

```sql
SELECT c.region,
       SUM(o.amount)                             AS region_total,
       RANK() OVER (ORDER BY SUM(o.amount) DESC)  AS rank_by_total
FROM orders o JOIN customers c ON o.cust_id = c.id
GROUP BY c.region;
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

Two caveats. `<@` (contained-by) is supported only as `'<const>' <@ field`
(equivalently `field @> '<const>'`) — the `field <@ '<const>'` direction ("this
field is a subset of a constant") is a constraint on the stored shape and can't
be pushed down as a filter. And because sqlglot reads a bare `->` inside a function
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
affected rows back as a result set — `*`, columns, aliases, jsonb navigation,
and **computed expressions** (arithmetic, `||`, function calls, `CASE` …)
evaluated per returned row. `INSERT` returns the inserted rows, `UPDATE` the
**post-image** of the updated rows (so a computed expression sees the new
values), and `DELETE` the deleted rows. Works on declared and reflected tables
alike, and on `INSERT … ON CONFLICT`:

```sql
INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b') RETURNING id, name;
UPDATE t SET n = n + 1 WHERE id = 1 RETURNING id, n;               -- the new n
INSERT INTO items (id, price, qty) VALUES (1, 10, 3)
  RETURNING id, price * qty AS total, upper(name) AS shout;        -- computed
DELETE FROM t WHERE n > 100 RETURNING *;
```

### INSERT … ON CONFLICT (upsert)

`INSERT` accepts an `ON CONFLICT` clause to make a colliding row an upsert
instead of a unique-constraint error. The conflict target names the column(s)
whose existing value the proposed row would duplicate — typically the primary
key:

```sql
-- skip the row if it already exists
INSERT INTO t (id, n) VALUES (1, 5) ON CONFLICT (id) DO NOTHING;

-- update the existing row instead; EXCLUDED is the row proposed for insertion
INSERT INTO t (id, n) VALUES (1, 5)
  ON CONFLICT (id) DO UPDATE SET n = EXCLUDED.n;

-- the SET expressions can mix the existing row and EXCLUDED, with an optional WHERE gate
INSERT INTO t (id, n) VALUES (1, 5)
  ON CONFLICT (id) DO UPDATE SET n = t.n + EXCLUDED.n WHERE t.n < 100;
```

`DO NOTHING` skips a conflicting row (and, with no conflict target, absorbs a
collision on *any* unique index). `DO UPDATE` updates the existing row: bare or
target-qualified columns (`n`, `t.n`) resolve to the existing row, and
`EXCLUDED.<col>` to the value that would have been inserted; an optional `WHERE`
gates the update. The command tag counts rows inserted *or* updated — skipped
rows don't count — and a `RETURNING` clause projects the inserted and updated
rows (not the skipped ones). `ON CONFLICT ON CONSTRAINT <name>` is not supported
(SecantusDB has no named-constraint registry — name the column(s) instead), and
`DO UPDATE` requires an explicit conflict target.

### MERGE

`MERGE` is the SQL-standard multi-action upsert. For each source row it finds the
target rows the `ON` condition matches, then applies the **first** `WHEN` clause
of the right kind whose optional `AND` condition holds — `UPDATE` / `DELETE` /
`DO NOTHING` for a match, `INSERT` / `DO NOTHING` for a non-match:

```sql
MERGE INTO accounts a USING deltas d ON a.id = d.id
WHEN MATCHED AND d.amount = 0 THEN DELETE
WHEN MATCHED THEN UPDATE SET balance = a.balance + d.amount
WHEN NOT MATCHED THEN INSERT (id, balance) VALUES (d.id, d.amount);
```

The source is a table, a reflected collection, or a `(SELECT …) alias`. In `ON`
and the `WHEN` conditions, target and source columns resolve by their alias
(`a.id` / `d.id`); an `UPDATE`'s right-hand sides and an `INSERT`'s `VALUES` may
reference either side. The command tag counts every row inserted, updated, or
deleted (`MERGE n`). Matching is evaluated against the target snapshot at the
statement's start and each target row is affected at most once.

`WHEN NOT MATCHED BY SOURCE` acts on **target** rows that no source row matched
(`UPDATE` / `DELETE` / `DO NOTHING`), and a `RETURNING` clause projects the
affected rows — an updated row's post-image, an inserted row, a deleted row's
pre-image — like a write statement's `RETURNING`:

```sql
MERGE INTO inventory i USING shipment s ON i.sku = s.sku
WHEN MATCHED THEN UPDATE SET qty = i.qty + s.qty
WHEN NOT MATCHED THEN INSERT (sku, qty) VALUES (s.sku, s.qty)
WHEN NOT MATCHED BY SOURCE THEN UPDATE SET qty = 0   -- items absent from the shipment
RETURNING i.sku, i.qty;
```

`RETURNING` resolves target columns (and computed expressions over them); the
`merge_action()` function and source-column references in `RETURNING` aren't
supported.

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

### Savepoints

`SAVEPOINT name` / `ROLLBACK TO SAVEPOINT name` / `RELEASE SAVEPOINT name` give
real nested, partial rollback inside a transaction — the machinery SQLAlchemy's
nested-transaction / unit-of-work blocks lean on. `ROLLBACK TO SAVEPOINT` undoes
every write since the savepoint (keeping earlier ones), leaves the savepoint
open, and un-poisons a block that a prior statement aborted. `RELEASE` forgets a
savepoint but keeps its writes.

```sql
BEGIN;
INSERT INTO accounts (id, balance) VALUES (1, 100);
SAVEPOINT sp1;
INSERT INTO accounts (id, balance) VALUES (2, 50);
ROLLBACK TO SAVEPOINT sp1;   -- id=2 undone; id=1 kept
INSERT INTO accounts (id, balance) VALUES (3, 20);
COMMIT;                      -- persists id=1 and id=3
```

Each savepoint captures a touched table's pre-image the first time it's written
after the savepoint is established, and `ROLLBACK TO` restores those pre-images —
so it undoes `INSERT` / `UPDATE` / `DELETE` (and upserts). A `SAVEPOINT` /
`RELEASE` / `ROLLBACK TO` outside a transaction block errors with `25P01`; an
unknown savepoint name errors with `3B001`. DDL issued inside a savepoint (e.g.
`CREATE TABLE`) is **not** rolled back by `ROLLBACK TO SAVEPOINT` — only DML is.

### Server-side cursors

`DECLARE name [WITH HOLD] CURSOR FOR <query>` runs the query and stores its rows;
`FETCH` / `MOVE` walk a scroll position over them, and `CLOSE` drops the cursor.
The cursor is fully scrollable — forward, backward, and by absolute / relative
position:

```sql
BEGIN;
DECLARE c CURSOR FOR SELECT id, name FROM users ORDER BY id;
FETCH 2 FROM c;            -- first two rows
FETCH NEXT FROM c;         -- the third
FETCH BACKWARD 1 FROM c;   -- back to the second
MOVE 2 FROM c;             -- advance without returning rows
FETCH ALL FROM c;          -- the rest
CLOSE c;
COMMIT;
```

`FETCH` accepts `NEXT` (default), a bare count, `ALL`, `PRIOR`, `FIRST`, `LAST`,
`FORWARD [n | ALL]`, `BACKWARD [n | ALL]`, `ABSOLUTE n`, and `RELATIVE n`; `MOVE`
takes the same directions but returns only a `MOVE n` count, no result set.
`CLOSE name` drops one cursor; `CLOSE ALL` drops them all. A `WITHOUT HOLD`
cursor (the default) closes at `COMMIT` / `ROLLBACK`; a `WITH HOLD` cursor
survives, since its rows are already materialized. Fetching from an unknown or
closed cursor errors with `34000`. The query is materialized once at `DECLARE`,
so a cursor is a snapshot — later writes in the same transaction aren't visible
through it.

`SET TRANSACTION ISOLATION LEVEL …` / `… READ ONLY` / `… READ WRITE`,
`SET SESSION CHARACTERISTICS AS TRANSACTION …`, and `BEGIN ISOLATION LEVEL …`
are accepted but are no-ops: SecantusDB is single-node, so isolation level and
read-only mode don't change behaviour.

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

A FROM-less `SELECT` also evaluates constant expressions (arithmetic, `||`,
function calls) and honours a constant `WHERE` (a false predicate returns zero
rows with the right column shape):

```sql
SELECT 1 + 1 AS two, upper('ab') AS shout;
SELECT 1 WHERE current_setting('server_version') IS NOT NULL;
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

The SQL-standard constraint views are also present, so tooling that reflects
through `information_schema` (rather than `pg_catalog`) resolves too:

```sql
-- the canonical primary-key reflection join
SELECT tc.table_name, kcu.column_name
FROM information_schema.table_constraints tc
JOIN information_schema.key_column_usage kcu
  ON tc.constraint_name = kcu.constraint_name
WHERE tc.constraint_type = 'PRIMARY KEY';
```

`table_constraints`, `key_column_usage`, and `constraint_column_usage` surface
one row per PRIMARY KEY (the only constraint SecantusDB models — a
`CREATE UNIQUE INDEX` is an index, not a constraint). `referential_constraints`
and `sequences` are present but empty (no foreign keys, no sequences), so an
ORM's FK / sequence reflection resolves to "none" instead of erroring.

## Supported SQL

| Area | Supported | Not yet |
|---|---|---|
| DML | `SELECT`, `INSERT` (`VALUES` / `… SELECT`), `INSERT … ON CONFLICT` (`DO NOTHING` / `DO UPDATE`), `UPDATE`, `DELETE`, `RETURNING` (columns + computed expressions) | `MERGE`, `ON CONFLICT ON CONSTRAINT` |
| Set ops | `UNION`/`UNION ALL`, `INTERSECT`/`INTERSECT ALL`, `EXCEPT`/`EXCEPT ALL` (chained; trailing `ORDER BY`/`LIMIT`) | corresponding-column-name reconciliation, `ORDER BY` over an expression |
| CTEs | `WITH name AS (...)` (multiple, chained) + `WITH RECURSIVE` (anchor `UNION`/`UNION ALL` recursive term, column aliases) on `SELECT` / set-op queries and on `INSERT`/`UPDATE`/`DELETE` | `WITH RECURSIVE` on a write body |
| `WHERE` | `=` `<>` `<` `<=` `>` `>=`, `IN`, `BETWEEN`, `LIKE`/`ILIKE`, `IS [NOT] NULL`, `AND`/`OR`/`NOT`, jsonb `@>`/`<@` (`const <@ field`)/`?`/`?\|`/`?&`, column-to-column + arithmetic, `IN`/`NOT IN`/scalar `OP (SELECT …)` subqueries (correlated or not), `EXISTS`/`NOT EXISTS` | correlated subqueries with an outer JOIN/GROUP BY, function calls in a comparison, `field <@ const` |
| Projection | columns, `*`, aliases, `jsonb` paths, `jsonb_*` functions, `DISTINCT`, `DISTINCT ON (…)`, computed expressions (arithmetic, `\|\|`, `upper`/`lower`/`length`/`substring`/`round`/`coalesce`/`greatest`/...) | computed GROUP BY keys, expressions over an aggregate |
| Aggregates | `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`, `COUNT`/`SUM`/`AVG`(`DISTINCT`), `GROUP BY`, `HAVING`, `GROUP BY ROLLUP`/`CUBE`/`GROUPING SETS` (single-table) | `GROUPING SETS` over a JOIN / with HAVING, the `GROUPING()` helper, `DISTINCT` aggregate in `HAVING` |
| Window | `ROW_NUMBER`/`RANK`/`DENSE_RANK`/`NTILE`, `FIRST_VALUE`/`LAST_VALUE`/`NTH_VALUE`, `SUM`/`COUNT`/`AVG`/`MIN`/`MAX` `OVER`, `LAG`/`LEAD`, `PARTITION BY`, `ORDER BY`, `ROWS` frames + `RANGE` (`UNBOUNDED`/`CURRENT ROW`) | numeric `RANGE` offset, window + `GROUP BY` in one SELECT |
| Joins | multi-table `INNER`/`LEFT JOIN`, two-table `RIGHT`/`FULL OUTER JOIN`, `CROSS JOIN` / comma-join, `[LEFT/CROSS] JOIN LATERAL` (single-table subquery, correlate in its `WHERE`), equality + non-equi / `OR` `ON`, JOIN + GROUP BY / aggregates / HAVING | `RIGHT`/`FULL` in a 3+ table chain, `LATERAL` over a join / aggregate subquery |
| DDL | `CREATE TABLE` (incl. `REFERENCES` / `FOREIGN KEY` declared-not-enforced, literal column `DEFAULT`), `DROP TABLE`, `ALTER TABLE` (`ADD`/`DROP`/`RENAME COLUMN`, `RENAME TO`, `SET`/`DROP NOT NULL`, `ALTER COLUMN TYPE`, `SET`/`DROP DEFAULT`, `ADD [CONSTRAINT] FOREIGN KEY`), `CREATE`/`DROP INDEX` (incl. `UNIQUE`), `COMMENT ON TABLE`/`COLUMN` | multi-action `ALTER`, `ADD` CHECK/UNIQUE constraint, non-literal / expression DEFAULT, enforced constraints, views |
| Transactions | `BEGIN`/`COMMIT`/`ROLLBACK`, `SET TRANSACTION` / `BEGIN ISOLATION LEVEL`, `SAVEPOINT`/`RELEASE`/`ROLLBACK TO` (accepted, single-node no-op) | true nested savepoint rollback, `DECLARE CURSOR` |
| Protocol | simple + extended query, `$1` params (text + binary), prepared statements, portals, binary result format | `COPY`, `DECLARE CURSOR` |
| Auth | trust, SCRAM-SHA-256, TLS | channel binding, mTLS, SQL `CREATE ROLE` |
| Catalog | `information_schema`, `pg_catalog` (`pg_index`/`pg_constraint`/`pg_am`/...), catalog *joins*, full SQLAlchemy reflection (`get_table_names`/`has_table`/`get_columns`/`get_pk_constraint`/`get_indexes`/`get_foreign_keys`, `Table(autoload_with=...)`, `get_foreign_keys`, `get_table_comment` + column comments) | `get_check_constraints`, `get_unique_constraints` |

Anything outside the supported set returns a faithful SQLSTATE error rather than
a wrong answer — the same "honest *not supported* over a half-feature" discipline
the [compatibility](compatibility.md) page describes for the MongoDB side.
