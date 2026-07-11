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

Or run it straight from the command line — `pip install "secantus[sql]"` puts a
`secantusd-py-pg` script on your `PATH` (the SQL sibling of the Mongo
`secantusd-py` daemon):

```console
$ secantusd-py-pg --host 127.0.0.1 --port 5432 --storage-path ./secantus-data
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

`CREATE TABLE` records a typed schema in a per-database catalog. A single
`PRIMARY KEY` column maps to the document `_id`, so PK uniqueness rides the
storage layer's `_id` index. A **composite** `PRIMARY KEY (a, b)` maps to a
subdocument `_id: {a, b}` — uniqueness still rides the `_id` index, and the
subdocument's key order is fixed to the PK declaration order so equality is
independent of the column order you insert with:

```sql
CREATE TABLE membership (
    org_id  bigint,
    user_id bigint,
    role    text,
    PRIMARY KEY (org_id, user_id)
);
INSERT INTO membership VALUES (1, 100, 'admin');
INSERT INTO membership VALUES (1, 100, 'member');  -- error 23505 (duplicate key)
SELECT role FROM membership WHERE org_id = 1 AND user_id = 100;
```

Both PK columns reflect through `pg_index` / `pg_constraint` / SQLAlchemy's
`get_pk_constraint`. Updating a PK column (single or a composite subfield) re-keys
the row — the old `_id` is deleted and the row re-inserted under the new one, with
the same statement-atomic uniqueness check (a collision with an existing key errors
`23505` and leaves the table unchanged). A *reflected* collection's `_id` is still
immutable and can't be updated (`0A000`), as in a real MongoDB deployment.

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

An `UPDATE`'s `SET col = …` right-hand side may be a literal or a **per-row
expression** — arithmetic, a column reference, `||`, or a function call — and every
assignment sees the row's *old* values (so a two-column swap `SET a = b, b = a`
works). A computed primary-key target (`SET id = id + 1`) re-keys the row:

```sql
UPDATE t SET n = n + 1 WHERE id = 1;      -- increment
UPDATE t SET total = price * qty;         -- from other columns
UPDATE t SET tag = upper(tag);            -- function call
```

Both `UPDATE` and `DELETE` accept **join forms** that bring in other tables.
`UPDATE t SET … FROM src … WHERE <join>` lets a `SET` right-hand side read the
joined source, and `DELETE FROM t USING src … WHERE <join>` is a semi-join —
each target row matched by *any* source combination is deleted exactly once (a
target matched by many source rows is still deleted a single time). The source
list may name several tables or a sub-`SELECT`, and either form takes a
`RETURNING` clause:

```sql
-- copy a per-id bonus from another table onto the target
UPDATE accounts a SET balance = a.balance + d.amount FROM deltas d WHERE a.id = d.id;

-- delete every account that has a matching row in closed
DELETE FROM accounts a USING closed c WHERE a.id = c.id RETURNING a.id;

-- a sub-SELECT is a valid source
UPDATE a SET n = s.v FROM (SELECT 1 AS id, 99 AS v) s WHERE a.id = s.id;
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
| `<type>[]` (e.g. `text[]`, `int[]`) | BSON array of the element type | `list` |

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

A **non-literal** default — `now()`, `CURRENT_TIMESTAMP`, `gen_random_uuid()`,
or an arithmetic / function expression — is evaluated **per omitted row** at
`INSERT` (so `gen_random_uuid()` yields a fresh value for each row). It works
both from `CREATE TABLE` and `ALTER COLUMN … SET DEFAULT`, and flows through
`INSERT … SELECT`:

```sql
CREATE TABLE ev (id uuid DEFAULT gen_random_uuid() PRIMARY KEY,
                 at timestamptz DEFAULT now(), n int DEFAULT 3 + 4);
INSERT INTO ev DEFAULT VALUES;    -- id: fresh uuid, at: now, n: 7
```

`information_schema.columns.column_default` reflects the default's text. A
`DEFAULT nextval('seq')` draws from a sequence instead (see below). A default
that references another column is rejected (`0A000`), matching Postgres.

### Sequences and SERIAL

`SERIAL` / `BIGSERIAL` / `SMALLSERIAL` columns are auto-incrementing integers. A
SERIAL column is an integer column (`int4` / `int8` / `int2`), implicitly
`NOT NULL`, backed by an owned sequence named `<table>_<column>_seq`; an `INSERT`
that omits it fills in the sequence's next value:

```sql
CREATE TABLE users (id serial PRIMARY KEY, name text);
INSERT INTO users (name) VALUES ('a'), ('b');    -- id -> 1, 2
INSERT INTO users (id, name) VALUES (100, 'c');  -- explicit id; sequence untouched
SELECT currval('users_id_seq');                  -- 2 (last value drawn this session)
```

Standalone sequences work too, with `START WITH` / `INCREMENT BY` / `MINVALUE` /
`MAXVALUE` / `CYCLE`, and the `nextval` / `currval` / `setval` / `lastval`
functions:

```sql
CREATE SEQUENCE order_seq START WITH 1000 INCREMENT BY 10;
SELECT nextval('order_seq');            -- 1000
SELECT nextval('order_seq');            -- 1010
SELECT setval('order_seq', 5000);       -- next nextval -> 5010
CREATE TABLE orders (id bigint DEFAULT nextval('order_seq') PRIMARY KEY, total int);
```

`nextval` advances and returns; `currval` / `lastval` read the last value drawn
**in the current session** (error `55000` before the first `nextval`); `setval`
sets the current value (`setval(seq, v, false)` makes the next `nextval` return
`v` itself). A non-cycling sequence raises `2200H` when it passes `MAXVALUE`;
`CYCLE` wraps to the other bound. Sequences reflect through
`information_schema.sequences`, `pg_catalog.pg_sequence`, and `pg_class`
(`relkind = 'S'`). A SERIAL column's owned sequence is dropped with the table.

`ALTER SEQUENCE` adjusts a sequence's parameters — `RESTART [WITH n]` (the next
`nextval` returns `n`, or the sequence's `START` when bare), `INCREMENT BY n`,
`MINVALUE` / `MAXVALUE`, `START WITH n`, and `[NO] CYCLE`:

```sql
ALTER SEQUENCE order_seq RESTART WITH 1;
ALTER SEQUENCE order_seq INCREMENT BY 100 MAXVALUE 1000000 CYCLE;
```

**Identity columns** (the SQL-standard alternative to SERIAL) also work, backed
by the same machinery:

```sql
CREATE TABLE a (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, v text);
CREATE TABLE b (id bigint GENERATED BY DEFAULT AS IDENTITY (START WITH 100 INCREMENT BY 5), v text);
INSERT INTO a (v) VALUES ('x');            -- id -> 1
INSERT INTO a (id, v) VALUES (99, 'y');    -- error 428C9: GENERATED ALWAYS
INSERT INTO a (id, v) VALUES (DEFAULT, 'y'); -- OK — DEFAULT draws from the sequence
INSERT INTO b (id, v) VALUES (50, 'z');    -- BY DEFAULT accepts an explicit value
```

`GENERATED ALWAYS` rejects a user-supplied value (`428C9`) but accepts the
`DEFAULT` keyword; `GENERATED BY DEFAULT` behaves like SERIAL. Both reflect their
identity kind through `pg_attribute.attidentity` (`'a'` / `'d'`).

### Enum types (`CREATE TYPE … AS ENUM`)

An enum type is a named set of allowed string labels. A column declared with an
enum type stores text but rejects any value outside the enum's labels (`22P02`):

```sql
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
CREATE TABLE survey (id int PRIMARY KEY, feeling mood);
INSERT INTO survey (id, feeling) VALUES (1, 'happy');   -- OK
INSERT INTO survey (id, feeling) VALUES (2, 'furious'); -- error 22P02
DROP TYPE mood;
```

A `NULL` is always allowed. Referencing an undeclared type in a column raises
`42704`. Enum types reflect through `pg_catalog.pg_type` (`typtype = 'e'`) and
`pg_catalog.pg_enum` (one row per label, `enumsortorder` giving label order), and
an enum column's `pg_attribute.atttypid` points at its enum type's oid — so
SQLAlchemy and `psql`'s `\dT` reflect them. Only the `ENUM` form of `CREATE TYPE`
is supported (composite / range / base types raise `0A000`).

An enum can be extended with `ALTER TYPE … ADD VALUE`, optionally positioning the
new label relative to an existing one. **`ORDER BY` on an enum column follows the
enum's declared label order, not lexical text order** — so a label added in the
middle sorts in its declared position:

```sql
ALTER TYPE mood ADD VALUE 'ecstatic';              -- appended
ALTER TYPE mood ADD VALUE 'meh' AFTER 'ok';        -- positioned
ALTER TYPE mood ADD VALUE IF NOT EXISTS 'ok';      -- no-op if present

SELECT feeling FROM survey ORDER BY feeling;
-- sad, ok, meh, happy, ecstatic  (declared order, not alphabetical)
```

Adding a label that already exists raises `42710` (unless `IF NOT EXISTS`); a
missing type or `BEFORE`/`AFTER` neighbour raises `42704`. Other `ALTER TYPE`
forms (e.g. `RENAME VALUE`) raise `0A000`. Enum-aware ordering applies everywhere
an enum column is an `ORDER BY` key — single-table, `GROUP BY`, `DISTINCT`, JOIN,
JOIN + GROUP BY, and the evaluated (computed-column) path.

### Domain types (`CREATE DOMAIN`)

A domain is a named base type carrying its own constraints. A column declared with
a domain type stores as the domain's **base type** and enforces the domain's
`NOT NULL` and `CHECK` constraints on every write (`INSERT` / `UPDATE` / upsert /
`MERGE`). The `CHECK` predicate refers to the value under test as `VALUE`:

```sql
CREATE DOMAIN posint   AS integer CHECK (VALUE > 0);
CREATE DOMAIN nonblank AS text NOT NULL CHECK (length(VALUE) > 0);
CREATE DOMAIN email    AS varchar(255) CONSTRAINT email_chk CHECK (VALUE LIKE '%@%');

CREATE TABLE parts (id int PRIMARY KEY, qty posint, label nonblank, contact email);

INSERT INTO parts VALUES (1, 5, 'bolt', 'a@b.com');   -- OK
INSERT INTO parts VALUES (2, -1, 'nut',  'a@b.com');  -- error 23514 (CHECK)
INSERT INTO parts VALUES (3, 5,  NULL,   'a@b.com');  -- error 23502 (domain NOT NULL)
```

A `CHECK` that fails raises `23514`; a `NULL` into a `NOT NULL` domain raises
`23502` (`domain <name> does not allow null values`). A domain `CHECK` is *not*
evaluated for a `NULL` value (Postgres' three-valued logic), so a domain without
`NOT NULL` accepts `NULL`. A domain may carry a `DEFAULT`, which a column of that
type inherits when it declares no default of its own:

```sql
CREATE DOMAIN score AS int DEFAULT 100 CHECK (VALUE >= 0);
CREATE TABLE game (id int PRIMARY KEY, s score);
INSERT INTO game (id) VALUES (1);          -- s defaults to 100
```

Domains reflect through `pg_catalog.pg_type` (`typtype = 'd'`, `typbasetype`
pointing at the base type's oid, `typnotnull` set for a `NOT NULL` domain); a
domain column's `pg_attribute.atttypid` points at the domain's oid, and each
domain `CHECK` is a `pg_constraint` row (`contype = 'c'` with `contypid` = the
domain oid) — so SQLAlchemy and `psql`'s `\dD` reflect them. `DROP DOMAIN [IF
EXISTS] name` removes one; a missing domain raises `42704` (silenced by
`IF EXISTS`). A domain name that clashes with an existing type raises `42710`, and
a domain built on an unknown base type raises `42704`. The `CHECK` predicate is
evaluated by the scalar engine, so it supports the same operators as a table
`CHECK` (comparisons, `LIKE`, the `~` / `~*` regex-match operators, `length()`,
arithmetic).

### Composite types (`CREATE TYPE … AS (…)`)

A composite type is an ordered list of named, typed fields. A column declared with
a composite type stores its value as a subdocument keyed by the field names; you
write it with the `ROW(…)` constructor (positional, mapped onto the type's fields)
and read a field with the `(col).field` accessor, which returns the field's
declared type:

```sql
CREATE TYPE addr AS (street text, zip int);
CREATE TABLE people (id int PRIMARY KEY, home addr);

INSERT INTO people VALUES (1, ROW('Main St', 90210));

SELECT (home).street, (home).zip FROM people;   -- 'Main St', 90210 (int, not text)
SELECT home FROM people;                          -- ("Main St",90210)  (record literal)
```

The `(col).field` accessor also works in a `WHERE` predicate (it lowers to a
dotted Mongo path, so it can drive an equality / range filter) and as an `UPDATE`
target — `UPDATE t SET col.field = v` rewrites a single subfield, while
`UPDATE t SET col = ROW(...)` replaces the whole value:

```sql
SELECT id FROM people WHERE (home).zip = 90210;
UPDATE people SET home.zip = 55555 WHERE id = 1;             -- one subfield
UPDATE people SET home = ROW('New Rd', 12345) WHERE id = 1;  -- whole value
```

Selecting the whole column renders the Postgres record text literal
`(field1,field2)` (a field is double-quoted when empty or containing a comma /
paren / quote / backslash / whitespace; a `NULL` field is empty) and reports the
generic `RECORD` type oid, so a driver decodes it as a tuple of text fields.
Composite types reflect through `pg_catalog.pg_type` (`typtype = 'c'`), and each
type's fields reflect via the `pg_type.typrelid` → `pg_class` (`relkind = 'c'`) →
`pg_attribute` chain, so `psql \dT+` and SQLAlchemy see the field names and types.
`DROP TYPE [IF EXISTS] name` removes one; a missing type raises `42704` (silenced
by `IF EXISTS`) and a name clash raises `42710`.

A composite field may itself be a composite type (**nested composites**). The
referenced type's fields are embedded at `CREATE TYPE` time; you build the value
with nested `ROW(...)`, walk in with chained accessors, and the whole value
renders as a nested Postgres record:

```sql
CREATE TYPE addr AS (street text, zip int);
CREATE TYPE person AS (name text, home addr);          -- home is itself a composite
CREATE TABLE t (id int PRIMARY KEY, p person);

INSERT INTO t VALUES (1, ROW('Bob', ROW('Main St', 90210)));

SELECT (p).home FROM t;              -- ("Main St",90210)  (the addr record)
SELECT ((p).home).street FROM t;     -- 'Main St'  (deep access, typed text)
SELECT ((p).home).zip FROM t;        -- 90210      (typed int)
SELECT p FROM t;                     -- (Bob,"(""Main St"",90210)")  (nested record)
UPDATE t SET p.home = ROW('Elm St', 11111) WHERE id = 1;
```

Nesting is arbitrary-depth (`(((p).home).at).lat`), works in `WHERE`
(`WHERE ((p).home).zip = 90210` lowers to a dotted Mongo path), and a composite
field reflects at its own type's oid in `pg_attribute`. A composite type cannot
contain itself (a direct cycle raises `0A000`).

`ALTER DOMAIN` evolves a domain in place:

```sql
ALTER DOMAIN posint ADD CONSTRAINT lt100 CHECK (VALUE < 100);  -- re-validates rows
ALTER DOMAIN posint ADD CHECK (VALUE <> 42) NOT VALID;         -- skip re-validation
ALTER DOMAIN posint DROP CONSTRAINT lt100;                     -- IF EXISTS supported
ALTER DOMAIN posint SET DEFAULT 1;
ALTER DOMAIN posint DROP DEFAULT;
ALTER DOMAIN posint SET NOT NULL;                              -- re-validates rows
ALTER DOMAIN posint DROP NOT NULL;
ALTER DOMAIN posint RENAME TO posnum;                          -- repoints columns
```

`ADD CONSTRAINT … CHECK` and `SET NOT NULL` **re-validate every existing row** of
every column typed with the domain: a row that would violate the new constraint
rejects the `ALTER` (`23514` / `23502`) and leaves the domain unchanged — add
`NOT VALID` to skip the re-check (it still applies to new writes). An unnamed
`ADD … CHECK` gets an auto-generated `<domain>_check[N]` name; a duplicate
explicit name raises `42710`. `RENAME TO` re-keys the domain and repoints every
column that references it (columns track the domain by name), rejecting a name
that clashes with an existing type (`42710`). Not modeled: `VALIDATE CONSTRAINT`
(accepted as a no-op, since we validate eagerly) and `RENAME CONSTRAINT`.

### Range types (`int4range`, `numrange`, `daterange`, …)

A range column stores an interval of element values. Five built-in range types
are supported: `int4range` / `int8range` (discrete integers), `numrange`
(numeric), `tsrange` (timestamp), and `daterange` (dates). A range value is
stored as a subdocument `{"lower", "upper", "lower_inc", "upper_inc"}` (or
`{"empty": true}`); discrete types canonicalise to the half-open `[)` form, so
`int4range(1,10)`, `'[1,10)'`, `'(0,10]'` and `'[1,9]'` all normalise to the
same interval.

```sql
CREATE TABLE reservations (id int PRIMARY KEY, during int4range);
INSERT INTO reservations VALUES (1, int4range(1, 10));   -- constructor
INSERT INTO reservations VALUES (2, '[5,20)');           -- text literal
INSERT INTO reservations VALUES (3, '(0,10]');           -- -> canonical [1,11)

-- Constructors and casts
SELECT int4range(1, 5);              -- [1,5)
SELECT '[1,10)'::int4range;          -- [1,10)
SELECT numrange(1.5, 3.5);           -- continuous, keeps its bound flags

-- Accessors
SELECT lower(during), upper(during), isempty(during) FROM reservations;

-- Operators: @> (contains value or range), <@ (contained by), && (overlaps)
SELECT * FROM reservations WHERE during @> 7;                -- value in range
SELECT * FROM reservations WHERE during @> int4range(6, 8);  -- range in range
SELECT * FROM reservations WHERE during && int4range(15, 150);
SELECT * FROM reservations WHERE int4range(6, 8) <@ during;
```

The `@>` / `<@` / `&&` operators work in both the `SELECT` list (yielding a
`bool`) and in `WHERE`. `lower(r)` / `upper(r)` return the range's element type
(`lower(int4range)` → `int4`); an unbounded side is `NULL`. Ranges reflect
through `pg_type` with `typtype = 'r'`.

**Range algebra + multiranges.** Ranges support the set operators and the
`range_merge` function, and `range_agg` coalesces a group's ranges into a
**multirange** (`int4multirange` / `nummultirange` / …), stored as an ordered,
non-overlapping list of ranges:

```sql
SELECT int4range(1,10) * int4range(5,20);   -- [5,10)   (intersection)
SELECT int4range(1,10) + int4range(5,20);   -- [1,20)   (union; errors if disjoint)
SELECT int4range(5,20) - int4range(1,10);   -- [10,20)  (difference; errors if it splits)
SELECT int4range(1,5) -|- int4range(5,9);   -- true     (adjacency)
SELECT range_merge(int4range(1,5), int4range(10,15));  -- [1,15) (smallest covering range)

SELECT int4multirange(int4range(1,5), int4range(10,15));  -- {[1,5), [10,15)}
SELECT '{[1,5), [10,20)}'::int4multirange;

SELECT g, range_agg(r) FROM t GROUP BY g;   -- coalesced multirange per group
```

The containment / overlap operators work on multiranges too — `@>` / `<@` / `&&`
where either operand is a multirange (or a mix of range and multirange, or a
multirange and a scalar element). A multirange's members are disjoint and
non-adjacent, so a range is contained iff a single member covers it:

```sql
SELECT int4multirange(int4range(1,5), int4range(10,20)) @> 12;   -- t
SELECT int4multirange(int4range(1,5), int4range(10,20)) @> int4range(4,12);  -- f (spans the gap)
SELECT int4range(1,20) @> int4multirange(int4range(2,5), int4range(10,15));  -- t
```

Not yet supported: `range_intersect_agg`, `multirange()` extraction functions,
and range GiST indexes.

### Full-text search (`tsvector` / `tsquery`)

`tsvector` and `tsquery` columns support the standard full-text search surface:
`to_tsvector` builds a document vector (lower-cased lexemes with positions,
English stop-words dropped), `to_tsquery` / `plainto_tsquery` build queries, the
`@@` operator matches, and `ts_rank` scores relevance:

```sql
CREATE TABLE docs (id int PRIMARY KEY, body tsvector);
INSERT INTO docs VALUES (1, to_tsvector('the quick brown fox'));
INSERT INTO docs VALUES (2, to_tsvector('the quick dog runs quick'));

SELECT to_tsvector('a cat sat') @@ to_tsquery('cat');    -- true

-- match: & (and), | (or), ! (not), and parentheses
SELECT id FROM docs WHERE body @@ to_tsquery('quick & dog');
SELECT id FROM docs WHERE body @@ plainto_tsquery('quick fox');  -- ANDs the words

-- rank the matches (higher term frequency ranks higher)
SELECT id FROM docs
WHERE body @@ to_tsquery('quick')
ORDER BY ts_rank(body, to_tsquery('quick')) DESC;
```

A `tsvector` renders as the Postgres text form `'brown':3 'fox':4 'quick':2`; a
`tsquery` as `'quick' & 'dog'`. Both accept text-literal casts
(`'a cat'::tsvector`, `'cat & dog'::tsquery`).

Prefix, phrase, `phraseto_tsquery`, and `ts_headline` are supported too:

```sql
-- prefix: cat:* matches any lexeme starting with "cat"
SELECT to_tsvector('a category') @@ to_tsquery('cat:*');       -- true

-- phrase: <-> requires adjacency, <N> requires distance N
SELECT to_tsvector('the quick brown fox') @@ to_tsquery('quick <-> brown');  -- true
SELECT to_tsvector('brown quick fox') @@ to_tsquery('quick <-> brown');      -- false

-- phraseto_tsquery keeps word order (chains the words with <->)
SELECT to_tsvector('a quick brown fox') @@ phraseto_tsquery('quick brown');  -- true

-- ts_headline highlights the matched terms
SELECT ts_headline('The quick brown fox', to_tsquery('quick | fox'));
--     The <b>quick</b> brown <b>fox</b>
```

`websearch_to_tsquery` parses a web-search-style string — bare words are AND'd,
`"quoted phrases"` become adjacency queries, the bare word `or` is an OR, and a
leading `-` negates — and a *ranked search* orders by the `ts_rank` score, which
can be referenced by its output alias (`ORDER BY rank`):

```sql
SELECT id FROM docs WHERE body @@ websearch_to_tsquery('quick -dog');   -- quick, not dog
SELECT id FROM docs WHERE body @@ websearch_to_tsquery('fox or "brown dog"');

SELECT id, ts_rank(body, websearch_to_tsquery('quick')) AS rank
FROM docs WHERE body @@ websearch_to_tsquery('quick')
ORDER BY rank DESC;                    -- ORDER BY resolves the SELECT-list alias
```

Simplifications vs real Postgres: the text-search configuration is fixed
(English stop-words, **no stemming** — `cats` and `cat` stay distinct), `ts_rank`
is a monotonic match-count score rather than the cover-density algorithm,
`ts_headline` returns the whole document (no fragment windowing), and lexeme
weights (`:A` / `setweight` / weighted `ts_rank`) and GIN/GiST indexes are out
of scope.

### Network address types (`inet` / `cidr` / `macaddr`)

`inet` (a host address with an optional netmask), `cidr` (a network, host bits
zero), and `macaddr` columns store canonical text and support the subnet
operators and accessor functions:

```sql
CREATE TABLE hosts (id int PRIMARY KEY, addr inet, mac macaddr);
INSERT INTO hosts VALUES (1, '10.1.2.3', '08:00:2b:01:02:03');
INSERT INTO hosts VALUES (2, '172.16.0.1/16', 'aabb.ccdd.eeff');

-- subnet containment: << (is contained by), >> (contains), && (overlaps)
SELECT id FROM hosts WHERE addr << '10.0.0.0/8'::cidr;    -- 1
SELECT '10.0.0.0/8'::cidr >> '10.1.2.3'::inet;            -- true
SELECT '10.0.0.0/8'::cidr && '10.1.0.0/16'::cidr;         -- true

-- accessors
SELECT host(addr), masklen(addr), family(addr) FROM hosts WHERE id = 2;
--     172.16.0.1 |      16 |      4
SELECT network(addr), netmask(addr), broadcast(addr) FROM hosts WHERE id = 2;
--     172.16.0.0/16 | 255.255.0.0 | 172.16.255.255/16
```

An `inet` with a full-host mask renders without the redundant `/32` (or `/128`
for IPv6); `macaddr` normalises to the lower-case colon form regardless of the
input separator. `host` and `abbrev` return `text`; `masklen` and `family`
return `int4`; `network` returns `cidr`; `netmask` / `broadcast` / `hostmask`
return `inet`.

Simplifications vs real Postgres: the `<<=` / `>>=` (contain-or-equal)
operators aren't parsed by sqlglot, `inet ± int` arithmetic, `macaddr8`, and
GiST network indexes are out of scope.

### Bit-string types (`bit(n)` / `varbit`)

`bit(n)` (fixed-length) and `bit varying` / `varbit` columns store a canonical
`'0'`/`'1'` string. `B'…'` literals, the bitwise operators, and the accessor
functions all work:

```sql
CREATE TABLE t (id int PRIMARY KEY, flags bit(8), mask varbit);
INSERT INTO t VALUES (1, '10101010', '111');

SELECT b'1010' & b'0110';        -- 0010  (AND)
SELECT b'1010' | b'0110';        -- 1110  (OR)
SELECT b'1010' # b'0110';        -- 1100  (XOR)
SELECT ~ b'1010';                -- 0101  (NOT)
SELECT b'1010' << 1;             -- 0100  (shift, width preserved)
SELECT b'1010' || b'11';         -- 101011  (concat)

SELECT 10::bit(8);               -- 00001010  (int -> bit)
SELECT b'1010'::int;             -- 10        (bit -> int)

SELECT length(flags), get_bit(flags, 0) FROM t WHERE id = 1;   -- 8 | 1
SELECT set_bit(flags, 0, 0) FROM t WHERE id = 1;               -- 00101010

-- a bitmask test in WHERE routes through the per-row scalar path
SELECT id FROM t WHERE flags & b'00001111' = b'00001010';
```

An explicit `::bit(n)` cast zero-pads or truncates on the right to exactly `n`
bits; a `::varbit(n)` truncates but never pads. `get_bit` / `set_bit` count from
the left (the most significant bit is index 0). The integer bitwise operators
(`5 & 3`) keep working — the operand type selects bit-string vs integer
semantics.

Simplifications vs real Postgres: a `bit(n)` *column* isn't padded to `n` on
insert (the declared length isn't tracked at the storage layer — only explicit
casts pad); a stored bit column can't be re-read as an integer with `::int`
(only a `B'…'` literal or a `::bit` cast is treated as a bit source); and bit
indexes are out of scope.

### Interval type (`interval`)

`interval` columns store a `{months, days, micros}` value and render in the
Postgres output style. Interval literals, arithmetic, and the standard functions
all work:

```sql
SELECT interval '1 year 2 months 3 days';     -- 1 year 2 mons 3 days
SELECT interval '90 minutes';                 -- 01:30:00

SELECT interval '1 day' + interval '2 hours'; -- 1 day 02:00:00
SELECT interval '1 hour' * 3;                 -- 03:00:00
SELECT - interval '1 day';                    -- -1 day

-- date/time arithmetic
SELECT timestamp '2020-01-31' + interval '1 month';        -- 2020-02-29 00:00:00
SELECT timestamp '2020-03-15' - timestamp '2020-01-01';    -- 74 days  (an interval)

-- functions
SELECT make_interval(1, 2, 0, 3, 4, 5, 6);    -- 1 year 2 mons 3 days 04:05:06
SELECT justify_hours(interval '25 hours');    -- 1 day 01:00:00
SELECT justify_days(interval '35 days');      -- 1 mon 5 days
SELECT age(timestamp '2021-03-15', timestamp '2020-01-20');  -- 1 year 1 mon 23 days
SELECT extract(day from interval '3 days 4 hours');          -- 3
```

`months`, `days`, and `micros` stay independent (a month is not a fixed number
of days), so `interval '1 month'` added to Jan 31 clamps to Feb 29 in a leap
year. `justify_hours` / `justify_days` / `justify_interval` roll the
fixed-duration parts up into larger units.

Simplifications vs real Postgres: days are treated as 24 hours (no DST-aware
arithmetic), the `@` / verbose input grammar beyond a trailing `ago` isn't
parsed, and interval indexes are out of scope.

### UUID type (`uuid`)

`uuid` columns store the canonical lower-case hyphenated string, and
`gen_random_uuid()` / `uuid_generate_v4()` mint fresh values:

```sql
CREATE TABLE people (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), name text);
INSERT INTO people (name) VALUES ('alice');
INSERT INTO people VALUES ('550e8400-e29b-41d4-a716-446655440000', 'bob');

-- casts normalise: uppercase, bare-hex, and {braced} forms all canonicalise
SELECT '550E8400E29B41D4A716446655440000'::uuid;   -- 550e8400-e29b-41d4-a716-446655440000

-- equality lowers to a Mongo filter (a non-canonical literal is normalised first)
SELECT name FROM people WHERE id = '550E8400-E29B-41D4-A716-446655440000';
```

Because the value is stored as its canonical string, equality and `ORDER BY`
work as ordinary comparisons and push down to the storage layer (no per-row
evaluation). `gen_random_uuid` and `uuid_generate_v4` return version-4
(random) UUIDs.

### Date-only / time-only types (`date` / `time` / `timetz`)

`date`, `time`, and `time with time zone` (`timetz`) are distinct types (a
`timestamp` / `timestamptz` column still maps to a full timestamp). They store
canonical text (`date` as `YYYY-MM-DD`, `time` as `HH:MM:SS`, `timetz` with an
offset), so equality and `ORDER BY` push down to storage:

```sql
CREATE TABLE ev (id int PRIMARY KEY, d date, t time, ttz timetz);
INSERT INTO ev VALUES (1, '2020-06-15', '09:00', '09:00:00+02');

SELECT date '2020-01-15';                 -- 2020-01-15
SELECT time '14:30';                      -- 14:30:00
SELECT current_date, current_time;

-- date arithmetic
SELECT date '2020-03-15' - date '2020-01-01';   -- 74      (integer days)
SELECT date '2020-01-31' + 1;                   -- 2020-02-01  (date)
SELECT date '2020-01-15' + interval '2 hours';  -- 2020-01-15 02:00:00  (timestamp)
SELECT time '15:00' - time '13:30';             -- 01:30:00  (interval)
```

These report the correct wire OIDs (`date` 1082, `time` 1083, `timetz` 1266), so
driver / ORM reflection sees them as the right types rather than a timestamp.

Simplifications vs real Postgres: `time(p)` precision isn't rounded to a
declared scale, `timetz` preserves the literal's offset without converting, and
mixing a bare `timestamp` with a `date` in one arithmetic expression isn't
supported (cast one side explicitly).

### Money type + `to_char` numeric formatting

`money` columns store a `Decimal` and render as `$1,234.56`. `to_char(numeric,
fmt)` formats a number with the common Postgres template patterns:

```sql
CREATE TABLE items (id int PRIMARY KEY, price money);
INSERT INTO items VALUES (1, '19.99'), (2, '$1,250.00');

SELECT price FROM items WHERE id = 2;      -- $1,250.00
SELECT price + price FROM items WHERE id = 1;   -- $39.98   (money arithmetic)

SELECT to_char(1234.5, 'FM999,999.99');    -- 1,234.50
SELECT to_char(1234, 'FM$9,999.99');       -- $1,234.00
SELECT to_char(-1234.5, 'FM9999.99PR');    -- <1234.50>
SELECT to_char(1234.56, '999999.99');      --    1234.56   (padded, non-FM)
```

Supported `to_char` patterns: `9` / `0` (digit positions), `.` (decimal point),
`,` (group separator), `$` / `L` (currency), `S` (anchored sign), `MI` (trailing
minus), `PR` (angle-bracket negatives), and the `FM` prefix (suppress padding).

Simplifications vs real Postgres: money is stored with 2-decimal scale and a `$`
symbol (no locale currency), `to_char` doesn't implement `EEEE` / `RN` / `V` /
`TH` or non-ASCII locale patterns, and — as for `numeric` — `ORDER BY` on a
money/decimal *column* relies on the storage engine's sort (the in-memory test
store can't sort raw `Decimal128`).

### Geometric types (`point` / `box` / `circle` / `polygon` / `lseg`)

The core Postgres geometric types are stored as their canonical text (`point`
`(1,2)`, `box` `(2,2),(0,0)`, `circle` `<(0,0),5>`, `polygon`
`((0,0),(1,0),(1,1))`, `lseg` `[(0,0),(1,1)]`) and support the distance and
overlap operators:

```sql
CREATE TABLE shapes (id int PRIMARY KEY, loc point, area polygon);
INSERT INTO shapes VALUES (1, '(1,1)', '((0,0),(4,0),(4,4),(0,4))');
INSERT INTO shapes VALUES (2, '(9,9)', '((5,5),(6,5),(6,6),(5,6))');

SELECT '(1, 2)'::point;                     -- (1,2)       (canonicalised)
SELECT point '(0,0)' <-> point '(3,4)';     -- 5           (<-> distance, float8)
SELECT '((0,0),(2,2))'::box @> point '(1,1)';   -- t        (@> contains)
SELECT '<(0,0),5>'::circle @> point '(3,3)';    -- t        (circle contains)

-- ORDER BY distance
SELECT id FROM shapes ORDER BY loc <-> point '(0,0)';   -- 1, 2
-- WHERE containment (per-row)
SELECT id FROM shapes WHERE area @> point '(2,2)';      -- 1
```

Operators: `<->` (distance), `@>` (contains) / `<@` (contained by), `&&`
(overlaps). Geometry math delegates to Shapely — a `circle` is modelled as its
centre point buffered by the radius. `<->` yields `float8`; `@>` / `<@` / `&&`
yield `bool`. The containment/overlap operators in a `WHERE` clause route through
the per-row scalar path (they can't lower to a Mongo `$match`).

Out of scope: the infinite `line` type and open/closed `path` distinction for the
operators (both spellings are accepted and stored), the `#` / `##` / `?-` / `?|`
positional operators, and geometric indexes.

### Binary data (`bytea`)

`bytea` columns store raw bytes (as a BSON Binary) and render as the `\x…` hex
form Postgres emits under the default `bytea_output = hex`. Both input literal
forms are accepted:

```sql
CREATE TABLE blobs (id int PRIMARY KEY, data bytea);
INSERT INTO blobs VALUES (1, '\xcafe');       -- hex form
INSERT INTO blobs VALUES (2, 'ab\001c');      -- escape form (a, b, 0x01, c)

SELECT encode(data, 'hex')    FROM blobs WHERE id = 1;   -- cafe
SELECT encode(data, 'base64') FROM blobs WHERE id = 1;   -- yv4=
SELECT decode('deadbeef', 'hex');                        -- \xdeadbeef

SELECT get_byte('\xdeadbeef'::bytea, 1);        -- 173
SELECT set_byte('\xdeadbeef'::bytea, 0, 0);     -- \x00adbeef
SELECT length('\xdeadbeef'::bytea);             -- 4  (byte count)
SELECT bit_length('\xdeadbeef'::bytea);         -- 32
SELECT '\xdead'::bytea || '\xbeef'::bytea;      -- \xdeadbeef
```

`encode` / `decode` convert between bytes and the `hex` / `base64` / `escape`
text formats; `get_byte` / `set_byte` read and replace a single 0-based byte;
`length` / `octet_length` return the byte count and `bit_length` returns 8× that;
`||` concatenates two `bytea` values. Out of scope: the `bytea_output = escape`
server setting (output is always hex) and the digest functions (`md5` / `sha256`,
which belong to the crypto extensions rather than core `bytea`).

### Key/value pairs (`hstore`)

The `hstore` contrib type stores a flat string → string map (values may be NULL)
and renders as the canonical `"a"=>"1", "b"=>"2"` text:

```sql
CREATE TABLE items (id int PRIMARY KEY, attrs hstore);
INSERT INTO items VALUES (1, 'color=>red, size=>big');
INSERT INTO items VALUES (2, 'color=>blue, size=>small');

SELECT attrs -> 'color' FROM items WHERE id = 1;      -- red   (-> lookup, text)
SELECT id FROM items WHERE attrs @> 'color=>red';     -- 1     (@> contains)
SELECT id FROM items WHERE attrs ? 'size';            -- 1, 2  (? key exists)
SELECT id FROM items WHERE attrs -> 'color' = 'blue'; -- 2     (pushes down)

SELECT akeys('a=>1,b=>2'::hstore);        -- {a,b}
SELECT avals('a=>1,b=>2'::hstore);        -- {1,2}
SELECT hstore_to_json('a=>1'::hstore);    -- {"a": "1"}
SELECT 'a=>1'::hstore || 'b=>2'::hstore;  -- "a"=>"1", "b"=>"2"   (merge)
SELECT defined('a=>1,b=>NULL'::hstore, 'b');  -- f   (present but NULL)
```

Operators: `->` (value lookup → text, NULL if absent), `@>` / `<@` (contains /
contained-by), `?` / `?&` / `?|` (key exists / all keys / any keys), `||` (merge,
right wins). Functions: `akeys` / `avals` (→ `text[]`), `hstore_to_json`,
`hstore(k, v)` / `hstore(keys[], vals[])`, `delete(h, key)`, `defined(h, key)`.

The containment and key-exists operators route through the per-row scalar path
(they can't lower to a Mongo `$match`); a `->` lookup *does* push down (it maps to
the stored key path). Stored as a tagged subdocument so it stays distinct from a
`jsonb` object even though the operators are spelled the same. Out of scope: the
set-returning `each` / `skeys` / `svals` forms (use the `akeys` / `avals` arrays),
GiST/GIN indexing, and the `#=` / `%%` record operators.

### Case-insensitive text (`citext`)

`citext` columns store text with its original case preserved (for storage and
display) but compare and sort **case-insensitively**:

```sql
CREATE TABLE u (id int PRIMARY KEY, name citext);
INSERT INTO u VALUES (1, 'Alice'), (2, 'BOB'), (3, 'carol');

SELECT name FROM u WHERE id = 1;             -- Alice   (case preserved)
SELECT id FROM u WHERE name = 'alice';       -- 1       (= folds case)
SELECT id FROM u WHERE name IN ('ALICE','bob');  -- 1, 2
SELECT id FROM u WHERE name < 'c';           -- 1, 2    (range folds case)
SELECT id FROM u WHERE name LIKE 'a%';       -- 1       (LIKE folds case)
SELECT id FROM u ORDER BY name;              -- 1, 2, 3 (a, b, c order)
```

The comparison operators (`=`, `<>`, `<`, `<=`, `>`, `>=`), `IN`, `BETWEEN`, and
`LIKE` all fold case (`LIKE` on citext is equivalent to `ILIKE`), and `ORDER BY`
on a citext column sorts case-insensitively. The value is stored verbatim, so the
case is preserved on read.

**Simplification:** `GROUP BY` / `SELECT DISTINCT` on a citext column currently
group case-*sensitively* (`Alice` and `alice` are distinct groups) — unlike real
Postgres, which folds them. The dominant citext uses (case-insensitive lookups,
uniqueness, and sorted listings) are faithful; case-folding aggregation grouping
is a known limitation. citext indexing is also out of scope.

### XML (`xml`)

`xml` columns store XML text (validated well-formed on write) and support the
core constructor / extraction functions:

```sql
CREATE TABLE docs (id int PRIMARY KEY, body xml);
INSERT INTO docs VALUES (1, '<doc><title>Hi</title></doc>');

SELECT xmlelement(name foo, 'bar');                     -- <foo>bar</foo>
SELECT xmlelement(name item, xmlattributes('7' as id), 'hi');  -- <item id="7">hi</item>
SELECT xmlforest('x' as a, 'y' as b);                   -- <a>x</a><b>y</b>
SELECT xml_is_well_formed('<a/>');                      -- t
SELECT xpath('/doc/title/text()', body) FROM docs;      -- {Hi}
SELECT xmlconcat('<a/>', '<b/>');                       -- <a/><b/>
```

`xmlelement(NAME tag [, xmlattributes(v AS k, …)], content…)` and
`xmlforest(value AS name, …)` build elements (content and attribute values are
XML-escaped); `xml_is_well_formed(text)` returns a boolean; `xpath(expr, xml)`
returns a `text[]` of matched nodes; `xmlconcat(…)` concatenates fragments. A
malformed `'<a>'::xml` cast is rejected.

XML parsing goes through the stdlib `xml.etree.ElementTree` (no external
dependency, and external entities are disabled — no XXE). **Simplifications:** the
`xpath` support is a pragmatic subset — absolute child paths (`/a/b/c`), a
trailing `text()` / `@attr` step, and a leading `//tag` descendant search — not
full XPath 1.0 (no namespaces, predicates, or functions). The `xmltable` table
function, the `xmlagg` aggregate, and the document/content-node distinction are
out of scope.

### Generated columns (`GENERATED ALWAYS AS (…) STORED`)

A generated column's value is computed from the row's other columns on every
write; you can't insert or update one directly (`428C9`), only recompute it via
`DEFAULT`:

```sql
CREATE TABLE box (
    id   int PRIMARY KEY,
    w    int,
    h    int,
    area int GENERATED ALWAYS AS (w * h) STORED
);
INSERT INTO box (id, w, h) VALUES (1, 3, 4);   -- area -> 12
UPDATE box SET w = 10 WHERE id = 1;            -- area recomputed to 40
INSERT INTO box (id, w, h, area) VALUES (2, 1, 1, 9);  -- error 428C9
```

The expression is evaluated with the aggregation/scalar engine (arithmetic,
`||`, functions), so a string column like
`full text GENERATED ALWAYS AS (first || ' ' || last) STORED` works too. If the
expression yields NULL (e.g. a NULL input), the column is NULL. Generated columns
reflect as `pg_attribute.attgenerated = 's'`. Only `STORED` is supported (which is
all Postgres itself offers).

### Array columns (`text[]`, `int[]`, …)

An array column (`<type>[]`) is stored as a native BSON array — the same
representation a MongoDB array field uses — so both protocols see one list.
Insert with either an `ARRAY[…]` constructor or a `'{…}'` string literal; results
render as Postgres array text (`{a,"b,c",NULL}`, quoting only elements that need
it) and a real driver decodes them back into a list via the array type OID:

```sql
CREATE TABLE post (id int PRIMARY KEY, tags text[], scores int[]);
INSERT INTO post VALUES (1, ARRAY['py', 'db'], ARRAY[10, 20]);
INSERT INTO post VALUES (2, '{go}', '{5}');

SELECT id FROM post WHERE 'py' = ANY(tags);   -- membership -> 1
SELECT id FROM post WHERE scores @> ARRAY[5]; -- containment -> 2
SELECT id, array_length(tags, 1) FROM post;   -- 1 -> 2, 2 -> 1
```

`= ANY(col)` is array membership (the value is contained in the array); the three
array containment / overlap operators are all supported:

```sql
SELECT id FROM post WHERE scores @> ARRAY[5];       -- contains: every RHS elem in LHS
SELECT id FROM post WHERE scores <@ ARRAY[5,10,20]; -- contained by: every LHS elem in RHS
SELECT id FROM post WHERE scores && ARRAY[20,99];   -- overlaps: share ≥1 element
```

`field @> ARRAY[…]` and `field && ARRAY[…]` against an array **column** with an
index use that index (a single-element `@>` and any `&&` report `IXSCAN` in
`EXPLAIN`); a multi-element `@>`, the `<@` (subset) form, and `field @> '{}'`
(empty, true for every row) are evaluated per row.

`array_length(col, dim)` gives the length along a dimension, `cardinality(col)`
the total element count, and `array_ndims` / `array_dims` / `array_upper` /
`array_lower` round out the introspection. **Multi-dimensional arrays** work:
`ARRAY[[1,2,3],[4,5,6]]` (or an `int[][]` column) stores, subscripts (`g[2][3]`),
and reports per-dimension lengths (`array_length(g, 2)` → `3`, `array_ndims(g)` →
`2`, `array_dims(g)` → `[1:2][1:3]`, `cardinality(g)` → `6`). Array columns reflect
as `information_schema.columns.data_type = 'ARRAY'` with the Postgres array type
OID in `pg_attribute`.

Arrays of the richer element types work the same way, and each reports its proper
Postgres array-type OID so a driver decodes the elements natively (a `uuid[]`
column comes back as a list of `UUID` objects, an `inet[]` as network objects,
etc.):

```sql
CREATE TABLE ev (id int, ids uuid[], hosts inet[], days date[], spans interval[]);
INSERT INTO ev VALUES (1,
  ARRAY['11111111-1111-1111-1111-111111111111'::uuid],
  ARRAY['10.0.0.1'::inet], ARRAY['2026-01-01'::date], ARRAY['1 day'::interval]);
SELECT id FROM ev WHERE ids @> ARRAY['11111111-1111-1111-1111-111111111111'::uuid];
```

`uuid[]`, `inet[]`/`cidr[]`/`macaddr[]`, `date[]`/`time[]`/`timetz[]`,
`interval[]`, `bit[]`/`varbit[]`, `money[]`, `xml[]`, `json[]`, the geometric
array types (`point[]`, `box[]`, …), and the range array types all carry their
real array OID.

Subscripting is 1-based, and `unnest(col)` expands an array to one row per element:

```sql
SELECT tags[1] FROM post;              -- first element ('py')
SELECT tags[2:3] FROM post;            -- 1-based inclusive slice -> {db}
SELECT tags[99] FROM post;            -- out of range -> NULL (no wraparound)
SELECT id, unnest(tags) FROM post;    -- one row per element
```

`arr[i]` returns the *i*-th element (NULL for an out-of-range or zero/negative
index — Postgres arrays don't wrap), and `arr[lo:hi]` returns the 1-based
inclusive slice (clamped to the array bounds). Both work in the SELECT list and in
`WHERE` (`WHERE tags[1] = 'py'`). `unnest(array_col)` in the SELECT list expands
the array; the FROM-clause table form (`FROM unnest(col)`) is not yet supported.

The array manipulation functions are available too:

```sql
SELECT array_append(tags, 'rust');        -- {py,db,rust}
SELECT array_prepend('rust', tags);       -- {rust,py,db}
SELECT array_cat(scores, ARRAY[99]);      -- {10,20,99}
SELECT array_position(tags, 'db');        -- 2   (1-based; NULL if absent)
SELECT array_remove(tags, 'py');          -- {db}
SELECT array_to_string(tags, ', ');       -- 'py, db'   (NULLs dropped)
SELECT array_to_string(tags, ',', 'n/a'); -- NULL elements become 'n/a'
```

And `array_agg` can populate a declared array column via `INSERT … SELECT`:

```sql
INSERT INTO groups (grp, members) SELECT grp, array_agg(user_id) FROM m GROUP BY grp;
```

`unnest(array_col)` also works as a FROM-clause table function — each outer row is
paired with one row per array element, exposed under the alias:

```sql
SELECT id, tag FROM post, unnest(tags) AS tag;     -- one row per (post, tag)
SELECT id, count(*) FROM post, unnest(tags) AS t GROUP BY id;
```

An inner (comma / `CROSS JOIN`) form drops a row whose array is empty; a
`LEFT JOIN unnest(tags) AS t ON true` keeps it with a NULL element. The
column-alias form (`unnest(tags) AS x(v)`) names the element column.

### `generate_series` and base-less table functions

A set-returning function can be the *whole* row source — no other table:

```sql
SELECT * FROM generate_series(1, 5);              -- 1,2,3,4,5 (one per row)
SELECT generate_series(1, 5);                     -- same, SELECT-list form
SELECT * FROM generate_series(1, 10, 2);          -- with a step -> 1,3,5,7,9
SELECT n FROM generate_series(1, 100) AS g(n) WHERE n % 7 = 0;
SELECT * FROM generate_series(1, 3) WITH ORDINALITY;   -- adds a 1-based ordinal column

-- date / timestamp ranges with an interval step
SELECT * FROM generate_series(timestamp '2024-01-01', timestamp '2024-01-03', interval '1 day');
SELECT generate_series(timestamp '2024-01-01', timestamp '2024-01-02', interval '12 hours');
```

The base-less `FROM` form also covers `unnest(ARRAY[…])`, `jsonb_array_elements`,
`jsonb_object_keys`, `regexp_split_to_table`, `regexp_matches`, and the two-column
record SRFs `jsonb_each` / `jsonb_each_text`:

```sql
SELECT * FROM unnest(ARRAY[10, 20, 30]) AS x;         -- 10,20,30
SELECT * FROM regexp_split_to_table('a,b,c', ',') AS p;
SELECT * FROM jsonb_array_elements('[1,2,3]'::jsonb) AS e;

-- regexp_matches is set-returning: one row per match, each a text[] of the
-- capture groups (or the whole match when there are none). The 'g' flag emits
-- every match; without it, at most the first.
SELECT * FROM regexp_matches('foobarbaz', 'ba.', 'g') AS m;   -- {bar}, {baz}
SELECT regexp_matches('a1b2', '([a-z])([0-9])', 'g');         -- {a,1}, {b,2}

-- jsonb_each / jsonb_each_text expand an object into (key, value) rows.
-- jsonb_each's value is jsonb; jsonb_each_text renders each value as text.
SELECT * FROM jsonb_each('{"a":1,"b":"x"}'::jsonb);            -- (a,1), (b,x)
SELECT * FROM jsonb_each_text('{"a":1}'::jsonb) AS t(k, v);   -- (a,'1')
```

`jsonb_each` / `jsonb_each_text` also work in the base-less `FROM` form above, and
`jsonb_each` works in the **lateral-join** form — one `(key, value)` row per member
of each outer row's object:

```sql
SELECT id, key, value FROM d, jsonb_each(doc);          -- expand each row's object
SELECT id, k, v FROM d, jsonb_each(doc) AS e(k, v);     -- renamed columns
```

The lateral `jsonb_each_text` form and the base-less `SELECT jsonb_each(x)`
composite form are not yet modeled.

A single-column function's column takes the table alias — `generate_series(1,5) AS g`
names the column `g` — or an explicit column alias (`AS g(n)`), or the function
name by default. `WITH ORDINALITY [AS t(v, ord)]` appends the ordinal column.
Projection, `WHERE`, `ORDER BY`, `LIMIT`, and `count(*)` all work over the
generated rows.

`generate_series` covers both integer / numeric ranges and date / timestamp
ranges with an `interval` step (the step's sign chooses the walk direction; a
zero step errors; month stepping is calendar-aware). The result column types as
`timestamp` or `timestamptz` by the bound's tz-ness.

**Simplifications:** a non-`count(*)` aggregate or `GROUP BY` directly over a
base-less SRF isn't supported yet — wrap it in a subquery / CTE, or generate into
a table first.

### Foreign keys

Column-level `REFERENCES` and table-level `FOREIGN KEY` — named or unnamed — are
recorded in the catalog, enforced on write (see "Constraint enforcement" above),
and surfaced through reflection so ORMs and migration tools see the
relationships.

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
    CONSTRAINT items_order_fk FOREIGN KEY (order_id)          -- table-level, named
        REFERENCES orders(id)
);
```

A `CONSTRAINT name` before a column-level `REFERENCES` or a table-level
`FOREIGN KEY` sets the constraint's name (used for reflection and
`SET CONSTRAINTS`); without one it defaults to `<table>_<col>_fkey`.

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

`ON DELETE` / `ON UPDATE` actions (`NO ACTION` / `RESTRICT` / `CASCADE` /
`SET NULL` / `SET DEFAULT`) fire on a parent `DELETE` / `UPDATE` (see "Constraint
enforcement" above). `REFERENCES t` with no column list targets `t`'s primary
key. A foreign key can
also be added after the fact with `ALTER TABLE … ADD [CONSTRAINT name] FOREIGN
KEY (…) REFERENCES …`.

### CHECK and UNIQUE constraints

`CHECK` and `UNIQUE` constraints — column-level, table-level, named or unnamed —
are recorded in the catalog, reflected, and **enforced** on write (see below).

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

Enforcement applies to **every** write path: plain `INSERT` / `UPDATE` /
`DELETE`, `INSERT … SELECT`, `INSERT … ON CONFLICT` (including a constraint other
than its arbiter target), and `MERGE`'s `INSERT` / `UPDATE` / `DELETE` actions.

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

#### Deferred constraints (`DEFERRABLE` / `INITIALLY DEFERRED`)

A `UNIQUE` or `FOREIGN KEY` constraint declared `DEFERRABLE` can have its check
postponed to the end of the transaction, so a block may hold a
transiently-inconsistent state and still commit — as long as the constraint holds
by the time it's checked. `INITIALLY DEFERRED` defers by default; `INITIALLY
IMMEDIATE` (the default) checks on each statement unless `SET CONSTRAINTS` defers
it. A deferred violation is re-checked at `COMMIT`; a violation that survives
raises (`23505` / `23503`) and rolls the transaction back.

```sql
CREATE TABLE orders (id bigint PRIMARY KEY,
                     uid bigint REFERENCES users(id) DEFERRABLE INITIALLY DEFERRED);

BEGIN;
INSERT INTO orders (id, uid) VALUES (1, 5);   -- user 5 doesn't exist yet — OK, deferred
INSERT INTO users  (id, name) VALUES (5, 'e'); -- now it does
COMMIT;                                        -- FK re-checked here: passes
```

`SET CONSTRAINTS { ALL | name [, …] } { DEFERRED | IMMEDIATE }` overrides the
deferral mode for the current transaction. Switching a pending constraint to
`IMMEDIATE` re-checks it right away (a surviving violation raises there, not at
`COMMIT`). Deferrability reflects through `pg_catalog.pg_constraint`
(`condeferrable` / `condeferred`) and
`information_schema.table_constraints` (`is_deferrable` / `initially_deferred`).

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
`get_view_definition()` see them. Views are not materialized — each query
re-reads the underlying tables.

`INSERT` / `UPDATE` / `DELETE` work through an **automatically-updatable** view —
one over a single base table (no join / set-op), with no `DISTINCT` / `GROUP BY` /
`HAVING` / window / `LIMIT`, whose output columns are plain base columns (or `*`).
The DML rewrites onto the base table, and the view's `WHERE` bounds which rows a
statement may touch. A view that isn't automatically-updatable (aggregate, join,
aliased/computed columns) raises `0A000` — Postgres would require an `INSTEAD OF`
trigger, which isn't modeled.

`WITH [LOCAL | CASCADED] CHECK OPTION` **is** enforced: an `INSERT` or `UPDATE`
through the view whose resulting row would not satisfy the view's `WHERE` (i.e.
would be invisible through the view) is rejected with SQLSTATE `44000`. As with
row visibility, a predicate that is not TRUE — FALSE *or* NULL — violates. A
`DELETE` is unaffected (it can't create a row). The option is reflected in
`information_schema.views.check_option` (`LOCAL` / `CASCADED` / `NONE`).

```sql
CREATE VIEW active_pos AS SELECT * FROM t WHERE n > 0 WITH CHECK OPTION;
INSERT INTO active_pos VALUES (1, -5);   -- ERROR: 44000 (would be invisible)
UPDATE active_pos SET n = -1 WHERE id = 2;  -- ERROR: 44000
```

The check is enforced one level deep — a `CASCADED` option over a view that is
itself defined over another `CHECK OPTION` view doesn't cascade the inner view's
condition (rare; auto-updatable views over a plain base table are the common
case).

### Materialized views

A materialized view stores a **snapshot** of its `SELECT`'s rows, queried like a
table. Unlike a plain view it does not track the base tables — `REFRESH
MATERIALIZED VIEW` recomputes the snapshot:

```sql
CREATE MATERIALIZED VIEW active AS SELECT id, name FROM users WHERE age >= 18;
SELECT count(*) FROM active;              -- reads the snapshot, not `users`
REFRESH MATERIALIZED VIEW active;         -- recompute after base data changes
DROP MATERIALIZED VIEW active;
DROP MATERIALIZED VIEW IF EXISTS active;
```

`CREATE MATERIALIZED VIEW … WITH NO DATA` registers the view **unpopulated** — it
is not scannable (querying it errors `55000`) until its first `REFRESH`. `WITH
DATA` is the default. `REFRESH MATERIALIZED VIEW CONCURRENTLY` recomputes the
snapshot but, like Postgres, requires the view to be populated and to carry a
unique index — otherwise it errors `0A000` with the "create a unique index" hint.
`ALTER MATERIALIZED VIEW … RENAME TO` moves the view, its catalog shape, and its
backing collection:

```sql
CREATE MATERIALIZED VIEW active AS SELECT id FROM users WHERE age >= 18 WITH NO DATA;
SELECT * FROM active;                      -- 55000: has not been populated
REFRESH MATERIALIZED VIEW active;          -- now scannable
ALTER MATERIALIZED VIEW active RENAME TO adults;
```

Materialized views reflect through `pg_class` (`relkind = 'm'`) and
`pg_get_viewdef()` — SQLAlchemy's `get_materialized_view_names()` sees them, and
they are excluded from `get_table_names()` / `information_schema.tables` (matching
Postgres). Refreshing is always a full recompute (there is no incremental
refresh — Postgres has none either); `CONCURRENTLY` enforces the unique-index +
populated prerequisites but applies the same full recompute rather than a true
diff-based concurrent swap.

## User-defined functions (`CREATE FUNCTION`)

Define a `LANGUAGE sql` function and call it anywhere an expression is allowed:

```postgresql
CREATE FUNCTION add(a int, b int) RETURNS int AS $$ SELECT a + b $$ LANGUAGE sql;
SELECT add(2, 3);                          -- 5
SELECT id, add(v, 100) FROM t;             -- called per row
SELECT id FROM t WHERE add(v, 0) > 10;     -- in WHERE

-- positional $1/$2 params and a single-quoted body work too
CREATE FUNCTION mul(int, int) RETURNS int LANGUAGE sql AS 'SELECT $1 * $2';

-- the body may query tables / aggregate
CREATE FUNCTION total() RETURNS int AS $$ SELECT sum(v) FROM t $$ LANGUAGE sql;

CREATE OR REPLACE FUNCTION add(a int, b int) RETURNS int AS $$ SELECT a + b + 1 $$ LANGUAGE sql;
DROP FUNCTION add(int, int);               -- or DROP FUNCTION add / DROP FUNCTION IF EXISTS
```

The body is a single SQL statement whose result is the return value. Parameters
resolve by name (the `a`, `b` columns in the body) and/or by position (`$1`,
`$2`). Functions may call other functions (nesting), and `EXECUTE`/prepared
statements can call them. Overloading by arity is supported — `f(int)` and
`f(int, int)` coexist. Calls are resolved when no built-in function matches, so a
built-in name always wins.

Errors mirror Postgres: redefining the same `(name, arity)` without `OR REPLACE`
raises `42723`; `DROP FUNCTION` of an unknown function raises `42883` (silenced by
`IF EXISTS`). A non-`sql` language (`plpgsql`, …) raises `0A000`.

User functions are reflected like Postgres', so `psql`'s `\df` and SQLAlchemy see
them: they appear in `pg_catalog.pg_proc` (with `proname` / `pronargs` /
`proargtypes` / `proargnames` / `prorettype` / `prosrc`),
`information_schema.routines` (one row per function) and
`information_schema.parameters` (one row per argument), and
`pg_get_functiondef(oid)` / `pg_get_function_arguments(oid)` /
`pg_get_function_result(oid)` reconstruct the definition, argument list, and
return type.

```sql
SELECT proname, pg_get_function_arguments(oid), pg_get_function_result(oid)
FROM pg_catalog.pg_proc WHERE proname = 'add';   -- add | a integer, b integer | integer
```

**Simplifications:** only `LANGUAGE sql` with a single-statement body; a
set-returning (`RETURNS SETOF` / `TABLE`) function returns only its first row in a
scalar context (use it as a scalar); and `pg_proc` lists only user-defined
functions (built-ins aren't enumerated there).

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
SELECT * FROM users WHERE name ~ '^a';         -- POSIX regex match (~* case-insensitive)
SELECT * FROM users WHERE name !~* 'test$';    -- negated, case-insensitive
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

-- Regex / string functions evaluate per row:
SELECT regexp_replace(path, '/+', '/', 'g'),   -- collapse runs of slashes (g = global)
       split_part(email, '@', 2) AS domain,    -- 2nd field (1-based; -1 counts from the end)
       translate(code, 'O-', '0'),             -- map 'O'->'0', delete '-'
       regexp_count(text, '[0-9]') AS digits,  -- number of matches
       regexp_matches(text, '(\w+)@(\w+)')     -- first match's capture groups -> text[]
FROM t;

-- More string functions: lpad/rpad pad (or truncate) to a length; left/right take
-- a prefix/suffix (negative counts drop from the far end); position/strpos give a
-- 1-based index (0 if absent); overlay replaces a span.
SELECT lpad(code, 8, '0'),               -- '000abcde'  (rpad pads on the right)
       left(name, 3), right(name, 2),    -- prefix / suffix (left(x,-2) drops last 2)
       repeat('=', 10), reverse(name),
       initcap(title),                   -- 'hello world' -> 'Hello World'
       ascii(name), chr(65),             -- code point of 1st char / char from code
       position('@' in email),           -- 1-based index (strpos(email,'@') is the same)
       overlay(sku placing 'XY' from 2 for 3)
FROM t;

-- Math / numeric functions evaluate per row. trunc/sign/factorial stay exact
-- numeric; sqrt/cbrt/ln/log/exp/pi/degrees/radians produce double precision.
SELECT trunc(x),          -- truncate toward zero (trunc(x, n) keeps n decimals)
       sqrt(x), cbrt(x),  -- square / (real) cube root
       sign(x),           -- -1 / 0 / 1
       ln(x), log(x),     -- natural log; log(x) is base-10 (log(b, x) is base b)
       log10(x), exp(x),  -- base-10 log; e^x
       pi(), degrees(x), radians(x),
       factorial(n),      -- n!
       gcd(a, b), lcm(a, b),
       mod(a, b), power(a, b), abs(x), ceil(x), floor(x), round(x, 2)
FROM t;

-- Date / time functions evaluate per row. extract / date_part return a numeric
-- field; date_trunc returns a timestamp; to_char returns text; ts ± interval
-- returns a timestamp (calendar-aware for month / year, with day clamping).
SELECT extract(year FROM at),        -- also month/day/hour/minute/second/quarter/
       extract(dow FROM at),         --   dow (Sun=0)/isodow (Mon=1)/doy/week/epoch
       date_part('hour', at),        -- date_part is the function-call spelling
       date_trunc('month', at),      -- zero everything below the unit (week -> Monday);
                                     --   also truncates an interval (returns an interval)
       to_char(at, 'YYYY-MM-DD HH24:MI:SS'),   -- Mon/Day month/weekday names too
       at + interval '1 day',        -- interval arithmetic (fixed + month/year units)
       at - interval '2 months 3 days',
       now(), current_timestamp, current_date
FROM events;

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
targets, not a query planner. A correlated WHERE also composes with a GROUP BY
**and** a window function in one SELECT — single-table or over a JOIN (the WHERE
filters the joined rows before the `$group`, then the window runs over the grouped
rows). A subquery in `HAVING` (`HAVING sum(x) > (SELECT … WHERE t.k = g.k)`) is
likewise evaluated per grouped row, so it may correlate on the group key.

## Aggregates, GROUP BY, HAVING

`COUNT` / `SUM` / `AVG` / `MIN` / `MAX` compile to an aggregation pipeline
(`$group`), along with `array_agg`, `string_agg`, the boolean aggregates
`bool_and` / `bool_or` (and their `every` spelling), and the statistical /
bitwise aggregates (`stddev*` / `variance` / `var_pop` / `bit_and` / `bit_or` /
`bit_xor`).

```sql
SELECT region, COUNT(*) AS n, SUM(amount) AS total, AVG(amount) AS mean
FROM sales
GROUP BY region
HAVING SUM(amount) > 100
ORDER BY total DESC;

SELECT COUNT(*), SUM(amount) FROM sales;          -- whole-table aggregate
SELECT COUNT(id) FROM sales;                      -- COUNT(col) excludes NULLs

SELECT region, string_agg(name, ', ') FROM sales GROUP BY region;  -- NULLs skipped
SELECT region, bool_and(active), bool_or(active) FROM sales GROUP BY region;
```

`string_agg(expr, sep)` joins the non-NULL values in each group with the
separator (returning NULL when every value was NULL). `bool_and` / `every` are
true only when every input is true; `bool_or` is true when any is.

Statistical and bitwise aggregates round out the set:

```sql
SELECT stddev(x), stddev_pop(x), stddev_samp(x),      -- sample / population stddev
       variance(x), var_pop(x),                        -- and their variances
       bit_and(n), bit_or(n), bit_xor(n)               -- bitwise fold over an int column
FROM t GROUP BY g;
```

`stddev` / `stddev_samp` and `variance` / `var_samp` are the **sample** forms
(NULL for a single row); `stddev_pop` / `var_pop` are the **population** forms.
They lower to Mongo's native `$stdDevPop` / `$stdDevSamp` accumulators (variance
is the square). `bit_and` / `bit_or` / `bit_xor` fold the non-NULL integer values
of a group (NULL for an all-NULL / empty group). All ignore NULL inputs. These —
`variance` / `var_pop` / `bit_and` / `bit_or` / `bit_xor` and `bool_and` /
`bool_or` / `every` — also work **over a `JOIN`**, resolving the aggregate's
argument through the join resolver.

`array_agg` and `string_agg` accept an **in-call `ORDER BY`** that orders the
aggregated values (multiple keys, `ASC`/`DESC`, and Postgres NULL placement):

```sql
SELECT dept, array_agg(name ORDER BY hired) FROM emp GROUP BY dept;
SELECT string_agg(name, ', ' ORDER BY name DESC) FROM emp;
```

The value + sort-key pair is collected per row and sorted in Python before the
array is built / the string joined. Supported grouped, whole-table, and **over a
`JOIN`** (the value / sort-key expressions lower through the join resolver).

The **ordered-set aggregates** `percentile_cont(f)` / `percentile_disc(f)` /
`mode()` are supported via `WITHIN GROUP (ORDER BY expr)`:

```sql
SELECT dept,
       percentile_cont(0.5) WITHIN GROUP (ORDER BY salary) AS median,   -- interpolated
       percentile_disc(0.9) WITHIN GROUP (ORDER BY salary) AS p90,      -- an actual value
       mode()               WITHIN GROUP (ORDER BY salary) AS commonest
FROM emp GROUP BY dept;
```

`percentile_cont(f)` interpolates linearly between the two nearest ranks
(returning `float8`); `percentile_disc(f)` returns the first value whose
cumulative fraction ≥ `f` (keeping the value's type); `mode()` returns the most
frequent value (the smallest on a tie). NULLs are ignored; an all-NULL / empty
group yields NULL. `f` must be in `[0, 1]` (else `2202E`). They collect the
ordered values and compute in Python, so they work grouped and whole-table (not
yet over a JOIN).

An aggregate can carry a `FILTER (WHERE cond)` clause — only rows satisfying
`cond` contribute to that aggregate:

```sql
SELECT region,
       count(*) FILTER (WHERE active)          AS active_n,
       sum(amount) FILTER (WHERE amount > 100) AS big_total,
       avg(amount) FILTER (WHERE active)       AS active_mean
FROM sales GROUP BY region;

SELECT count(*) FILTER (WHERE status = 'paid') FROM orders;   -- whole-table
SELECT region FROM sales GROUP BY region
HAVING count(*) FILTER (WHERE active) >= 1;                    -- in HAVING
```

`FILTER` works on `count` / `sum` / `avg` / `min` / `max` / `bool_and` / `bool_or`
in the SELECT list (grouped, whole-table, and over a JOIN) and in `HAVING`. It
lowers to a `$cond` inside the accumulator (a non-matching row donates the neutral
element — `0` for sum/count, NULL for avg/min/max). It also works on the
collecting aggregates `array_agg` / `string_agg` / `jsonb_object_agg` (a
non-matching row is dropped from the collected array/string/object; a *matching*
`NULL` is still kept by `array_agg`, per Postgres):

```sql
SELECT dept, array_agg(sal) FILTER (WHERE active) FROM emp GROUP BY dept;
SELECT string_agg(dept, ',') FILTER (WHERE active) FROM emp;
```

`FILTER` also combines with `DISTINCT` — `count(DISTINCT x) FILTER (WHERE cond)`
counts the distinct `x`-values drawn only from matching rows (likewise
`sum`/`avg`), in the SELECT list and in `HAVING`:

```sql
SELECT dept, count(DISTINCT sal) FILTER (WHERE active) FROM emp GROUP BY dept;
SELECT dept FROM emp GROUP BY dept HAVING count(DISTINCT sal) FILTER (WHERE active) > 1;
```

The condition supports comparisons, `AND` / `OR` / `NOT`, and `IS [NOT] NULL`. Not
supported (`0A000`): `FILTER` with an in-call `ORDER BY`
(`array_agg(x ORDER BY y) FILTER (…)`), and `DISTINCT` aggregates under
`GROUPING SETS` / `ROLLUP` / `CUBE`.

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

A leading plain `GROUP BY a, ROLLUP(b)` keeps `a` in every grouping set.
`HAVING` and `DISTINCT` aggregates (`count(DISTINCT x)`) work per grouping set,
and the whole construct runs **over a `JOIN`** too — each grouping set replays the
`$lookup`/`$unwind` join prefix before its own `$group`, so `GROUP BY ROLLUP(d.label)`
across joined tables produces the same subtotal hierarchy:

```sql
SELECT d.label, SUM(t.amt), GROUPING(d.label) AS g
FROM t JOIN d ON t.region = d.region
GROUP BY ROLLUP(d.label);
```

A **window function** may run over a GROUPING SETS / ROLLUP / CUBE query
(single-table): the grouping-sets union produces the grouped rows and each window
is then computed over them, so a rolled-up row (a group column reads NULL) still
participates. `GROUPING()` is available inside the window's `ORDER BY` /
`PARTITION BY` as well:

```sql
SELECT region, SUM(amt),
       ROW_NUMBER() OVER (ORDER BY GROUPING(region), SUM(amt) DESC)
FROM t GROUP BY ROLLUP(region);
```

The window may even run over a GROUPING SETS query that *also* sits over a
`JOIN` — the join prefix is replayed per grouping set, and the window then runs
over the union's grouped rows (`GROUPING()` in the window ORDER BY / PARTITION BY
still works):

```sql
SELECT d.label, SUM(t.amt),
       ROW_NUMBER() OVER (ORDER BY SUM(t.amt) DESC)
FROM t JOIN d ON t.region = d.region GROUP BY ROLLUP(d.label);
```

A *computed* grouping key over a JOIN (`GROUP BY ROLLUP(lower(d.label))`), a
correlated `WHERE` with GROUPING SETS, and a subquery in `HAVING` alongside a
window over GROUPING SETS are still rejected (`0A000`).

The `GROUPING(a, …)` super-aggregate helper is supported: it returns a bitmask
that is 1 for each argument rolled up (absent from that row's grouping set) and 0
otherwise, most-significant bit first — so `GROUPING(region)` distinguishes a
subtotal row's NULL from a real NULL, and `GROUPING(region, dept)` yields 0/1/2/3
across a `CUBE`. In a plain `GROUP BY` every argument is grouped, so it is 0.

```sql
SELECT region, GROUPING(region) AS g, SUM(amount)
FROM sales GROUP BY ROLLUP(region);   -- g = 1 on the grand-total row
```

### Computed GROUP BY keys

`GROUP BY` accepts an *expression* in place of a bare column — arithmetic, `%`,
`||`, and the common functions (`lower` / `upper` / `length` / `abs` / `round` /
`floor` / `ceil` / `coalesce`). The same expression may also appear in the
`SELECT` list, `HAVING`, and `ORDER BY`:

```sql
-- fold case, then group
SELECT lower(name) AS g, SUM(x) FROM t GROUP BY lower(name) ORDER BY g;

-- bucket by a derived value
SELECT x % 2 AS parity, COUNT(*) FROM t GROUP BY x % 2;

-- a computed key alongside a bare column and a HAVING on the aggregate
SELECT city, lower(name) AS g, SUM(x)
FROM t GROUP BY city, lower(name) HAVING SUM(x) > 5;
```

Each computed key is lowered to an equivalent aggregation expression and
materialised into a synthetic field before the group, so the grouping runs on the
derived value exactly as Postgres would. This works over a **`JOIN`**
(`GROUP BY lower(c.region)` across joined tables — the key lowers through the join
resolver) and over **`GROUPING SETS` / `ROLLUP` / `CUBE`** (`GROUP BY
ROLLUP(lower(region))`; `GROUPING(lower(region))` reports the rollup bitmask). A
key using a function the engine can't evaluate (e.g. `substr`) raises `0A000`.

The same function lowering applies inside a `WHERE` comparison against a constant —
`WHERE upper(name) = 'X'`, `WHERE abs(x) = 3`, `WHERE length(name) > 3` all
evaluate via `$expr`.

## Joins

An `INNER` or `LEFT JOIN` compiles to a `$lookup`. The `ON` may be an equality
(index-accelerated), a multi-condition `AND`, or a non-equi / `OR` predicate
(evaluated per candidate pair). `CROSS JOIN` and the implicit comma form
(`FROM a, b`) produce a cartesian product. Multiple joins chain — each table
joins the base or an already-joined table. `RIGHT` and `FULL OUTER` joins are
supported between **two** tables, and a **pure-`RIGHT` chain of 3+ tables**
(every join `RIGHT`, each `ON` joining a table to the immediately-prior one) is
reversed into a `LEFT` chain driven from the last table. A chain mixing `LEFT`
and `RIGHT`, a `RIGHT` `ON` that reaches a non-adjacent table, and any
multi-table `FULL` stay `0A000`:

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
FROM cte`, an `UPDATE` / `DELETE` whose `WHERE` has a subquery over a CTE, and a
`MERGE` whose `USING` source is a CTE (`WITH c AS (…) MERGE INTO t USING c …`).

```sql
WITH recent AS (SELECT id FROM events WHERE ts > '2024-01-01')
DELETE FROM events WHERE id IN (SELECT id FROM recent);

WITH totals AS (SELECT cust_id, sum(total) AS spent FROM orders GROUP BY cust_id)
INSERT INTO summary (cust_id, spent) SELECT cust_id, spent FROM totals;
```

A CTE body may itself be a **data-modifying** statement — `INSERT` / `UPDATE` /
`DELETE`, optionally with `RETURNING`. The write executes for its side effects,
and its `RETURNING` rows materialize as the CTE, so you can move rows between
tables in a single statement:

```sql
WITH moved AS (DELETE FROM events WHERE ts < '2024-01-01' RETURNING *)
INSERT INTO events_archive SELECT * FROM moved;
```

`WITH RECURSIVE` may also precede a write body (the recursive CTE materializes
first, then the `INSERT` / `UPDATE` / `DELETE` runs). Two things aren't modeled:
statement-level snapshot semantics — each data-modifying CTE observes the effects
of earlier ones rather than one pre-statement snapshot — and `WITH CHECK OPTION`.

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
frames with `UNBOUNDED` / `CURRENT ROW` bounds *and* a numeric `n PRECEDING` /
`n FOLLOWING` offset (a row is in-frame when its `ORDER BY` key is within `n` of
the current row's key — a value window, not a row count; Postgres requires exactly
one `ORDER BY` column for an offset `RANGE` frame, and a non-numeric/interval
offset is not supported):

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

-- manipulation: set / insert / strip / delete (each returns a modified copy)
SELECT jsonb_set(data, '{a}', '5') FROM docs;            -- set data.a = 5 (creates if absent)
SELECT jsonb_set(data, '{b,c}', '{"k":1}') FROM docs;    -- set a nested path to any json
SELECT jsonb_insert(data, '{d}', '9') FROM docs;         -- insert only if the key is absent
SELECT jsonb_strip_nulls(data) FROM docs;                -- drop object members whose value is null
SELECT data #- '{a}' FROM docs;                          -- delete data.a
SELECT data #- '{b,c}' FROM docs;                        -- delete a nested path
SELECT jsonb_pretty(data) FROM docs;                     -- indented text rendering

-- aggregates: collect rows into a json array / object
SELECT jsonb_agg(v) FROM t;                              -- [v1, v2, …]
SELECT jsonb_agg(v ORDER BY v DESC) FROM t;              -- in-call ORDER BY honoured
SELECT g, jsonb_object_agg(k, v) FROM t GROUP BY g;      -- {k1: v1, k2: v2, …} per group
SELECT json_agg(v), json_object_agg(k, v) FROM t;        -- the json_* spellings too

-- builders: value / row -> json
SELECT to_jsonb(5), to_json('hi');                       -- scalar -> json
SELECT to_jsonb(p), row_to_json(p) FROM composites;      -- a composite -> a json object
```

`jsonb_agg` / `json_agg` build a json array from the group's values (an in-call
`ORDER BY` sorts them, like `array_agg`); `jsonb_object_agg` / `json_object_agg`
build a json object, with each key coerced to text (Postgres object keys are
text). `to_jsonb` / `to_json` / `row_to_json` convert a value or a composite /
`ROW(...)` into json (a composite becomes an object keyed by its field names).
All are typed `json` on the wire.

The path argument to `jsonb_set` / `jsonb_insert` / `#-` is a Postgres `text[]`
(`'{a,b}'`); the value argument is parsed as JSON (`'5'` → 5, `'{"k":1}'` → an
object) the way an implicit `::jsonb` cast would. These functions return a
modified copy — the stored row is untouched (use them in an `UPDATE … SET`).

SQL/JSON **path** queries navigate a jsonb value with a `jsonpath` expression:

```sql
-- jsonb_path_query returns the matched value (first match in this scalar
-- context); jsonb_path_query_array collects all matches into a jsonb array.
SELECT jsonb_path_query(data, '$.a.b') FROM docs;            -- a nested member
SELECT jsonb_path_query_array(data, '$.items[*].x') FROM docs;

-- jsonb_path_exists / @? test whether a path matches anything; jsonb_path_match
-- / @@ evaluate a boolean predicate path. Both return a real boolean.
SELECT jsonb_path_exists(data, '$.a.b') FROM docs;
SELECT data @? '$.items[*] ? (@.x == 2)' FROM docs;          -- filter expression
SELECT data @@ '$.a.b == 5' FROM docs;                       -- predicate
```

The supported `jsonpath` subset is `$` (root), `.key` / `."key"` member access,
`[n]` array index (negative counts from the end), `[*]` all array elements, `.*`
all members, and a `? (<predicate>)` filter whose predicate compares `@` / `@.path`
(`== != < <= > >=`) to a literal, combined with `&&` / `||`. Arithmetic, functions
(`.size()`), recursive `**`, and `like_regex` are out of scope (they raise a
faithful "not supported" error). `jsonb_path_query` is set-returning in Postgres;
here it yields the **first** match in a scalar `SELECT` (use `jsonb_path_query_array`
for the full set).

One caveat. Both `<@` directions work: `'<const>' <@ field` (equivalently
`field @> '<const>'`) pushes down to a Mongo filter, while `field <@ '<const>'`
("this stored value is a subset of the constant") and `'<const>' @> field` run as
a collection scan with a per-row containment check (they can't lower to a filter).
`<@` / `@>` also work in a scalar `SELECT` (`'{"a":1}'::jsonb <@ '{"a":1,"b":2}'::jsonb`
→ `t`). And because sqlglot reads a bare `->` inside a function
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

### TRUNCATE

`TRUNCATE [TABLE] t [, …]` empties one or more tables in one statement — faster
than `DELETE` and the usual way test suites reset state between cases:

```sql
TRUNCATE orders;
TRUNCATE TABLE orders, order_items;
TRUNCATE orders RESTART IDENTITY;        -- also reset the serial / IDENTITY sequence
TRUNCATE orders CASCADE;                 -- also empty tables that reference orders
```

`RESTART IDENTITY` resets each truncated table's owned `SERIAL` / `IDENTITY`
sequences (so the next insert starts over); `CONTINUE IDENTITY` (the default)
leaves them. By default (`RESTRICT`), truncating a table that a **foreign key
from another table** points at is an error (SQLSTATE `0A000`) unless that
referencing table is truncated in the same statement; `CASCADE` instead empties
the referencing tables too (transitively). `TRUNCATE IF EXISTS` skips a missing
table; an unknown table otherwise errors `42P01`.

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
rows (not the skipped ones). `ON CONFLICT ON CONSTRAINT <name>` names the arbiter
by a declared `UNIQUE` constraint or the primary key (default name
`<table>_pkey`); an unknown name errors. `DO UPDATE` requires an explicit conflict
target (a column list or a constraint name).

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
statement's start and each target row is affected at most once — if a target row
is matched by more than one source row, the statement errors `21000` ("MERGE
command cannot affect row a second time"), matching Postgres. (A single source
row matching several target rows is fine — each is acted on once.)

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

`RETURNING` resolves target columns and computed expressions over them, plus
`merge_action()` — which yields `'INSERT'`, `'UPDATE'`, or `'DELETE'` for each
returned row — and **source-column references** (`s.col`):

```sql
MERGE INTO inventory i USING shipment s ON i.sku = s.sku
WHEN MATCHED THEN UPDATE SET qty = i.qty + s.qty
WHEN NOT MATCHED THEN INSERT (sku, qty) VALUES (s.sku, s.qty)
RETURNING merge_action(), i.sku, s.qty AS shipped;
```

## Bulk load / dump (`COPY`)

`COPY … FROM STDIN` bulk-loads rows and `COPY … TO STDOUT` streams them out — the
sub-protocol `psql`'s `\copy` and `pg_dump` use. Both the default *text* format
and *CSV* are supported:

```sql
COPY users (id, name, active) FROM STDIN;      -- then stream tab-separated rows
COPY users FROM STDIN WITH CSV HEADER;         -- CSV, first line is column names
COPY users TO STDOUT;                          -- stream every row back, text format
COPY users (id, name) TO STDOUT WITH CSV;      -- selected columns, CSV format

-- Dump an arbitrary query's result (query-form COPY, TO only):
COPY (SELECT id, name FROM users WHERE active ORDER BY id) TO STDOUT;
COPY (SELECT grp, count(*) FROM users GROUP BY grp) TO STDOUT WITH CSV HEADER;
```

From `psql`:

```
\copy users FROM 'users.csv' WITH CSV HEADER
\copy users TO 'out.tsv'
```

Rows loaded via `COPY FROM` go through the same coercion and constraint
enforcement as `INSERT` (NOT NULL / CHECK / UNIQUE / FK, sequence defaults,
generated + enum columns). In *text* format a field of `\N` is NULL and `\t` /
`\n` / `\\` are escaped; in CSV an unquoted empty field is NULL while a quoted
empty field (`""`) is the empty string, and `HEADER` skips / emits a column-name
line. `DELIMITER` and `NULL` options are honoured. Only `STDIN` / `STDOUT` are
supported (no server-side file paths — the client streams the data, exactly as
`\copy` does). A generated or `GENERATED ALWAYS AS IDENTITY` column is excluded
from a no-column-list `COPY FROM`.

`COPY (query) TO STDOUT` runs an arbitrary `SELECT` (including joins, aggregates,
`WITH`, and set operations) and dumps its result; the CSV `HEADER` uses the
query's output column names. It is dump-only — `COPY (query) FROM STDIN` is a
syntax error (`42601`).

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

**Partial indexes** — `CREATE INDEX … WHERE <predicate>` — index only the rows
matching the predicate. The predicate lowers to the same `partialFilterExpression`
a MongoDB-side partial index uses, so the query planner accelerates matching
queries and `explain` reports an `IXSCAN` with `isPartial: true`:

```sql
CREATE INDEX ix_active ON orders (user_id) WHERE status = 'active';
CREATE UNIQUE INDEX ux_email ON users (email) WHERE email IS NOT NULL;
```

**Expression indexes** — `CREATE INDEX … ((<expr>))` — index a computed
expression (a function call like `lower(name)`, or an arithmetic expression like
`a + b`). SecantusDB materialises the expression into a hidden per-row field,
recomputes it on every write, and builds an ordinary secondary index over it; a
`WHERE` clause containing that exact expression is rewritten to ride the index, so
`explain` reports an **Index Scan**. The hidden field never appears in `SELECT *`
or catalog reflection, and `DROP INDEX` removes both the index and the field.

```sql
CREATE INDEX ix_lower_name ON users ((lower(name)));
SELECT * FROM users WHERE lower(name) = 'bob';   -- Index Scan using ix_lower_name

CREATE INDEX ix_total ON line_items ((qty * price));
SELECT * FROM line_items WHERE qty * price = 100;
```

The expression must be one SecantusDB's expression engine can evaluate (the same
functions and operators available in the `SELECT` list); an unsupported construct
raises a `0A000` not-supported. Only single-expression indexes are supported —
mixing an expression with plain columns in one index (`CREATE INDEX … (a, (b +
c))`) is rejected. `ORDER BY` on the indexed expression still returns correct
results, but sorts via per-row evaluation rather than index order.

### `EXPLAIN`

`EXPLAIN <statement>` returns a `QUERY PLAN` text column describing how the query
runs. Because SecantusDB executes SQL against the Mongo storage, the plan mirrors
the storage's own routing decision — a query that lands on a covering index shows
an **Index Scan**, everything else a **Seq Scan**:

```sql
EXPLAIN SELECT * FROM users WHERE age = 30;
--                       QUERY PLAN
-- ----------------------------------------------------------
--  Seq Scan on users  (cost=0.00..0.00 rows=100 width=0)
--    Filter: (age = 30)

CREATE INDEX ix_age ON users (age);
EXPLAIN SELECT * FROM users WHERE age = 30;
--  Index Scan using ix_age on users  (cost=0.00..0.00 rows=100 width=0)
--    Index Cond: (age = 30)
```

The Index Scan / Seq Scan call is the authoritative one from the same planner
`find_matching` uses, so `EXPLAIN` never claims an index the real query wouldn't
touch. `UPDATE` / `DELETE` show a modify node over the scan; `INSERT` an *Insert*
node; JOIN / GROUP BY / aggregate queries a coarser node tree (the top operation
over the base-collection scans, since Mongo runs them as an aggregation pipeline).

Options:

```sql
EXPLAIN ANALYZE SELECT ...;              -- runs the statement, reports actual rows
EXPLAIN (FORMAT JSON) SELECT ...;        -- Postgres' JSON plan shape
EXPLAIN (ANALYZE, VERBOSE) SELECT ...;   -- parenthesised option list
```

`ANALYZE` actually executes the statement (as Postgres does — an `EXPLAIN ANALYZE`
of a write performs the write) and annotates the top node with `actual rows`.
`FORMAT JSON` / `FORMAT TEXT` are supported; other formats raise `0A000`.

**Simplifications:** cost figures are placeholders (`cost=0.00..0.00`) — there is
no statistics engine; `ANALYZE` reports actual rows but no per-node timing; and
pipeline-query plans name the top operation coarsely rather than reproducing
Postgres' full plan-node tree.

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

### Row-locking clauses (`FOR UPDATE` / `FOR SHARE`)

`SELECT … FOR UPDATE` / `FOR SHARE` / `FOR NO KEY UPDATE` / `FOR KEY SHARE` — with
the `NOWAIT`, `SKIP LOCKED`, and `OF <table>` modifiers — are accepted so ORM
locking (SQLAlchemy's `with_for_update()`) works:

```sql
SELECT * FROM accounts WHERE id = 1 FOR UPDATE;
SELECT * FROM jobs WHERE done = false FOR UPDATE SKIP LOCKED;
SELECT a.* FROM accounts a JOIN owners o ON o.id = a.owner FOR SHARE OF a NOWAIT;
```

SecantusDB is single-node, so the lock itself is a **no-op** — the statement
just returns its rows (a connection already serializes against the shared
storage). One real check is applied: an `OF <table>` target that isn't a relation
in the query's `FROM` errors with SQLSTATE `42P01`, exactly as Postgres reports
it (and, as in Postgres, a table's alias masks its real name in `OF`).

### Index / constraint reflection for `\d`

`pg_catalog.pg_indexes` lists one row per index with a rendered `indexdef`, and
`pg_get_indexdef(oid)` reconstructs the same `CREATE INDEX` text — what psql's
`\d` and SQLAlchemy read to list a table's indexes:

```sql
SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'orders';
-- orders_pkey  | CREATE UNIQUE INDEX orders_pkey ON public.orders USING btree (id)
-- idx_created  | CREATE INDEX idx_created ON public.orders USING btree (created_at DESC)
```

The primary key surfaces as `<table>_pkey` (never WiredTiger's internal `_id_`
index), a descending index column renders with `DESC`, and a `CREATE UNIQUE
INDEX` / unique-constraint index renders `UNIQUE`.

`pg_get_constraintdef(oid)` renders every constraint the way Postgres does, so
SQLAlchemy's inspector can reflect it:

```sql
SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint;
-- orders_pkey  | PRIMARY KEY (id)
-- orders_c_fkey| FOREIGN KEY (customer) REFERENCES customers(id)
-- orders_n_key | UNIQUE (n)
-- orders_check | CHECK ((total > 0))
```

### Session settings (`SET LOCAL` / `SHOW ALL` / `pg_settings`)

`SET LOCAL name = value` applies a GUC only for the rest of the current
transaction — it reverts to the prior value at `COMMIT` or `ROLLBACK`:

```sql
BEGIN;
SET LOCAL statement_timeout = '5s';   -- active only in this transaction
-- ...
COMMIT;                               -- statement_timeout reverts
```

Outside a transaction block, `SET LOCAL` has no lasting effect (as in Postgres).
A plain `SET` is session-scoped and persists past `COMMIT`.

`SHOW ALL` lists every GUC as a three-column table (`name`, `setting`,
`description`), and `pg_catalog.pg_settings` exposes the same values with the
metadata psql's `\dconfig` and ORMs read:

```sql
SHOW ALL;
SELECT name, setting, vartype, source FROM pg_settings WHERE name = 'TimeZone';
-- TimeZone | UTC | string | default        (source becomes 'session' after a SET)
```

### Role membership (`GRANT <role> TO <member>`)

Roles can be granted to other roles/users, building a membership hierarchy that
`pg_catalog.pg_auth_members` reflects (psql's `\du` and SQLAlchemy read it):

```sql
CREATE ROLE readers;
GRANT readers TO alice;                 -- alice is now a member of readers
GRANT readers TO bob WITH ADMIN OPTION; -- bob may grant readers onward
REVOKE readers FROM alice;

SELECT r.rolname AS role, m.rolname AS member, am.admin_option
FROM pg_auth_members am
JOIN pg_roles r ON r.oid = am.roleid
JOIN pg_roles m ON m.oid = am.member;
-- readers | bob | t
```

`WITH ADMIN OPTION` is tracked (and a plain re-grant keeps an existing one, as in
Postgres); `REVOKE ADMIN OPTION FOR <role> FROM <member>` clears just the admin
option and keeps the membership. Membership is recorded and reflected but not
enforced (a member doesn't automatically inherit the group role's table grants).

### Monitoring views (`pg_stat_activity`)

`pg_catalog.pg_stat_activity` reflects the server's live backends — one row per
open connection — so admin UIs and monitoring tools can introspect the server:

```sql
SELECT pid, datname, usename, application_name, state, query
FROM pg_stat_activity;
-- 12881 | app | alice | myapp | active | SELECT ... FROM pg_stat_activity
```

Each connection gets a distinct `pid`, a `backend_start` timestamp, its
`client_addr` / `application_name`, and a live `state` — `active` while a query
runs (a client sees its own row as `active` with that query), else `idle` with
its last query. `pg_stat_database` gives a per-database live backend count
(`numbackends`); its cumulative counters (`xact_commit`, `blks_hit`, …) are
single-node dev stubs reporting `0`.

### Advisory locks

The `pg_advisory_lock` family is accepted so application-level locking works:

```sql
SELECT pg_advisory_lock(42);              -- session-level exclusive lock
SELECT pg_try_advisory_lock(1, 2);        -- two-key form; returns true
SELECT pg_advisory_lock_shared(42);       -- shared mode
SELECT pg_advisory_xact_lock(42);         -- transaction-scoped
SELECT pg_advisory_unlock(42);            -- true if a session lock was held
SELECT pg_advisory_unlock_all();          -- release all session locks
```

All eleven functions are supported — `pg_advisory_lock` / `pg_advisory_unlock` /
`pg_advisory_unlock_all` and the `_shared`, `_xact_`, and `pg_try_*` variants.
SecantusDB is single-node, so a lock is **always granted immediately** — the
functions never block. What SecantusDB does track is *which* locks the
connection holds, so:

- `pg_try_advisory_lock*` always return `true` (nothing to contend with);
- `pg_advisory_unlock*` return `true` only if a matching session-level lock was
  held (and `false` otherwise, as Postgres does);
- advisory locks are **re-entrant** — locking the same key twice needs two
  unlocks;
- `pg_advisory_xact_lock*` locks are released automatically at `COMMIT` /
  `ROLLBACK` (and can't be released manually);
- the held locks are reflected through **`pg_catalog.pg_locks`** (`locktype =
  'advisory'`, one row per key+mode, always `granted`).

```sql
SELECT locktype, classid, objid, objsubid, mode, granted
FROM pg_locks WHERE locktype = 'advisory';
```

A single `bigint` key splits into `(classid, objid)` signed 32-bit halves with
`objsubid = 1`; a two-`int4` key maps straight through with `objsubid = 2` —
matching Postgres. `pg_locks` reflects **this connection's** locks (single-node
dev surface); cross-backend visibility and non-advisory lock types aren't
modelled.

### Two-phase commit (`PREPARE TRANSACTION`)

The two-phase-commit statements are supported so a client (or a transaction
manager) can prepare a transaction on one connection and resolve it later,
possibly from another:

```sql
BEGIN;
INSERT INTO accounts VALUES (1, 100);
PREPARE TRANSACTION 'txn-42';   -- the block's writes are now staged, uncommitted

-- ... later, on this or any other connection:
COMMIT PREPARED 'txn-42';       -- make the staged writes durable
-- or:
ROLLBACK PREPARED 'txn-42';     -- discard them
```

`PREPARE TRANSACTION 'gid'` detaches the open block's storage transaction into a
**server-wide registry** keyed by the global transaction id (`gid`), leaving the
issuing session with no active transaction. The writes stay staged (invisible to
other transactions) until a matching `COMMIT PREPARED 'gid'` commits them or
`ROLLBACK PREPARED 'gid'` aborts them — and because the registry is shared across
connections, the commit/rollback can run on a **different** backend from the one
that prepared it. Any deferred constraints are re-checked at `PREPARE` time (as
they would be at `COMMIT`), so a violation fails the `PREPARE`.

Open prepared transactions are reflected through **`pg_catalog.pg_prepared_xacts`**:

```sql
SELECT gid, prepared, owner, database FROM pg_prepared_xacts;
-- txn-42 | 2026-07-08 12:00:00+00 | alice | mydb
```

Error handling matches Postgres: `PREPARE TRANSACTION` outside a transaction
block raises `25P01`; a duplicate `gid` raises `42710`; `COMMIT PREPARED` /
`ROLLBACK PREPARED` for an unknown `gid` raises `42704`, and running either
inside a transaction block raises `25001`.

Two limitations: prepared transactions are held **in memory only**, so — unlike
real Postgres, which persists them to `pg_twophase` — they do **not** survive a
server restart; and the statements must be sent over the **simple query
protocol** (the wire server can't route them through the extended Parse/Bind
path). Both are fine for the single-node test-surrogate use case.

## LISTEN / NOTIFY

Asynchronous pub/sub works across connections to the same server:

```sql
-- connection A
LISTEN events;

-- connection B
NOTIFY events, 'something happened';
SELECT pg_notify('events', 'or via the function');
```

Connection A receives a `NotificationResponse` carrying the notifying backend's
pid, the channel, and the payload — surfaced by the driver's notification API
(e.g. pg8000's `connection.notifications`). `UNLISTEN channel` stops listening on
one channel; `UNLISTEN *` stops all. Channel names fold to lower case unless
double-quoted (standard identifier rules).

Semantics: a `NOTIFY` issued inside a transaction block is buffered and delivered
at `COMMIT` (and discarded on `ROLLBACK`); an autocommit `NOTIFY` delivers
immediately. A connection listening on a channel receives its own notifications.

Notifications are delivered **inline with the listener's query cycle** — a queued
notification rides back to the listener just before the `ReadyForQuery` of its
next query (as Postgres does when a backend is idle-in-command). So a listener
that keeps issuing queries (or a heartbeat `SELECT 1`) sees notifications
promptly; a listener that goes completely silent won't observe them until its
next round-trip.

**Simplifications:** duplicate `(channel, payload)` notifications within one
transaction are not collapsed (Postgres collapses them); `LISTEN` / `UNLISTEN`
take effect immediately rather than at commit; and there is no out-of-band push
to a fully-idle connection (notifications are attached to the next query
response, not delivered asynchronously mid-idle).

## Prepared statements (`PREPARE` / `EXECUTE` / `DEALLOCATE`)

The SQL-level prepared-statement commands parse a query once and rerun it with
different argument values:

```postgresql
PREPARE by_id (int) AS SELECT name FROM users WHERE id = $1;
EXECUTE by_id (42);
EXECUTE by_id (99);
DEALLOCATE by_id;      -- forget it; DEALLOCATE ALL forgets every one
```

The `$1`, `$2`, … placeholders in the prepared query are filled by the
positional `EXECUTE` arguments. Any statement kind works — `SELECT`, `INSERT`,
`UPDATE`, `DELETE` — and `EXECUTE` returns the underlying statement's result and
command tag (an `EXECUTE` of a `SELECT` yields rows; of an `INSERT`, an
`INSERT 0 N` tag). The optional `(argtypes)` list after the name is accepted and
ignored — argument values are coerced by the target column's type, as with any
literal.

Prepared statements are **per-connection** and live until `DEALLOCATE` or the
connection closes. `PREPARE` of an already-used name errors (`42P05`); `EXECUTE`
of an unknown name errors (`26000`); an argument-count mismatch errors
(`08P01`). `DEALLOCATE` of a name that was never prepared is tolerated as a
no-op (libpq/psycopg fire speculative `DEALLOCATE`s during cleanup).

This is distinct from the extended wire protocol's Parse/Bind portals — a
driver's own client-side parameter binding (pg8000's `%s`, psycopg's `%s`) uses
that path and never touches these commands.

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

### Roles (`CREATE ROLE` / `CREATE USER`)

SQL-level roles are recorded in the catalog and surfaced through `pg_catalog.pg_roles`,
so `psql`'s `\du` and role-aware tooling see them:

```sql
CREATE ROLE analyst;
CREATE USER app WITH PASSWORD 'secret' CREATEDB;   -- USER implies LOGIN
ALTER ROLE analyst WITH LOGIN;
GRANT SELECT ON orders TO analyst;                 -- recorded + enforced (see below)
DROP ROLE analyst;
```

`CREATE ROLE` / `CREATE USER` (with `LOGIN` / `SUPERUSER` / `CREATEDB` / `CREATEROLE` /
`INHERIT` / `REPLICATION` and their `NO…` negations, `PASSWORD`, `CONNECTION LIMIT`),
`ALTER ROLE`, and `DROP ROLE` are stored and reflected. Role-membership `GRANT` /
`REVOKE` (`GRANT admin TO alice`) is accepted but not enforced. Table-privilege
`GRANT` / `REVOKE` (`GRANT SELECT ON t TO alice`) **is** recorded and enforced — see
[Table privileges](#table-privileges-grant--revoke) below. The connecting user
always appears in `pg_roles` as a superuser login role, like Postgres' bootstrap
superuser.

These SQL roles are a schema-shape / reflection record, **distinct from the wire
server's SCRAM auth users** (the `users={...}` constructor argument above): creating
a SQL role does not by itself add a login credential, and vice versa.

### Authorization (RBAC)

By default the SQL surface is **unrestricted**: in trust mode (no `require_auth`)
and for the embedded `run_sql` API, any statement runs against any database. To
gate statements, start the server with `require_auth=True` **and** per-user role
bindings via `user_roles`. Authorization then reuses the **same role model as the
Mongo server** (`secantus.rbac`), so a SQL client and a Mongo client on the same
`Storage` are held to the same roles:

```python
server = SecantusPGServer(
    port=5432,
    require_auth=True,
    users={"analyst": "s3cret", "editor": "hunter2"},
    user_roles={
        "analyst": [{"role": "read", "db": "shop"}],       # SELECT only
        "editor":  [{"role": "readWrite", "db": "shop"}],  # + INSERT/UPDATE/DELETE/DDL
    },
)
```

Each statement maps to one RBAC action on the connection's database and is
checked with `rbac.check_privilege`: reads (`SELECT`, `DECLARE … CURSOR`, and a
plain `WITH`) need `find`; `INSERT` / `UPDATE` / `DELETE` / `MERGE` (and any
data-modifying CTE) need the matching write action; DDL needs
`createCollection` / `dropCollection` / `createIndex`; `CREATE/DROP/ALTER ROLE`
and `GRANT` / `REVOKE` need user-admin actions. Transaction control
(`BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT`), `SET` / `SHOW`, cursor
navigation (`FETCH` / `MOVE` / `CLOSE`), and session-info queries need no
privilege. A denied statement returns SQLSTATE **`42501`**
(`insufficient_privilege`) and the connection survives.

Built-in roles (`read`, `readWrite`, `dbAdmin`, `dbOwner`, `root`, the
`*AnyDatabase` variants) resolve directly; custom roles resolve through the
shared roles table (`Storage.get_role` / the Mongo `createRole` command), so a
role defined once governs both protocols. An authenticated user with no role
binding can connect and run session-only statements but touches no data.

Without `user_roles`, authorization stays off — pass it only when you want the
per-database gate.

### Table privileges (`GRANT` / `REVOKE`)

`GRANT`/`REVOKE` of `SELECT` / `INSERT` / `UPDATE` / `DELETE` (or `ALL`) on a table
are persisted and enforced as an **additive** layer over the Mongo RBAC roles
above:

```sql
GRANT SELECT, INSERT ON orders TO analyst;   -- analyst can now read + insert orders
GRANT SELECT ON orders TO PUBLIC;            -- everyone can read orders
REVOKE INSERT ON orders FROM analyst;        -- take the insert back
```

A data operation is authorized when the connection's Mongo role covers it **or**
a table grant does — so a grant *extends* access to a user whose role wouldn't
otherwise allow the operation (including a user with no role at all, or via a
`PUBLIC` grant), and a `REVOKE` takes that granted access back. Because the two
axes are additive, a table grant never *shrinks* what a broader Mongo role
(e.g. `readWrite`) already permits; use the role bindings for that. Enforcement
follows the same opt-in as the RBAC gate: with authorization off (trust mode /
embedded `run_sql`) grants are recorded and reflected but not enforced.

Grants surface through `information_schema.role_table_grants` /
`information_schema.table_privileges` and the `has_table_privilege([user,] table,
privilege)` function:

```sql
SELECT grantee, privilege_type FROM information_schema.role_table_grants;
SELECT has_table_privilege('analyst', 'orders', 'SELECT');   -- t
```

`ALL` expands to Postgres' seven table privileges for reflection fidelity, but
only `SELECT` / `INSERT` / `UPDATE` / `DELETE` are enforced (the other operations
don't exist in SecantusDB). Role-membership grants and grants on schemas /
databases / sequences remain accepted no-ops.

Grants can also be **column-scoped** for a finer grain than the whole table:

```sql
GRANT SELECT (id, title) ON articles TO editor;  -- editor reads only these columns
GRANT UPDATE (title) ON articles TO editor;      -- and updates only this one
```

A column grant authorizes exactly the named columns. A statement is allowed on
the column-grant path only when **every** column it touches is granted — a
`SELECT` of an ungranted column (including a `SELECT *`, or a column referenced
only in the `WHERE`) or an `UPDATE`/`INSERT` of an ungranted column is denied with
`42501`. A whole-table grant (or a broader Mongo role) still covers every column,
so column grants only *add* access — they never shrink it. Column grants surface
through `information_schema.column_privileges` and
`has_column_privilege([user,] table, column, privilege)`.

### Switching identity (`SET ROLE` / `SET SESSION AUTHORIZATION`)

A session can change its **current role** with `SET ROLE`, leaving the **session
user** (the login identity) unchanged, and change both with `SET SESSION
AUTHORIZATION`:

```sql
SET ROLE analyst;                 -- current_user is now analyst; session_user unchanged
SELECT current_user, session_user;  -- ('analyst', 'joe')
RESET ROLE;                       -- back to the login role (SET ROLE NONE is equivalent)

SET SESSION AUTHORIZATION alice;  -- both current_user and session_user become alice
RESET SESSION AUTHORIZATION;      -- back to the login identity
```

The current role (`current_user` / `current_role` / `user`) is what the table
grants above are matched against, so a session that holds the `analyst` role can
`SET ROLE analyst` to pick up whatever was granted to `analyst`. `session_user`
always reports the login identity. `SHOW role` / `current_setting('role')` report
the current role (`none` when it tracks the session user).

When authorization is active, a session may only assume an identity it already
holds — its own login, a role in its bindings, or anything if it is a superuser
(`root`); assuming another identity to borrow its grants is denied with SQLSTATE
`42501`. In trust mode (and the embedded API) the switch is unrestricted. Role
*membership* is not otherwise tracked, so `SET ROLE` doesn't consult a membership
graph beyond the session's own role bindings.

### Row-level security (`CREATE POLICY`)

Enable RLS on a table and attach policies to restrict which rows a role can see
or write:

```sql
ALTER TABLE doc ENABLE ROW LEVEL SECURITY;
CREATE POLICY owner_rows ON doc
    FOR ALL TO public
    USING (owner = current_user)         -- rows this role may read / target
    WITH CHECK (owner = current_user);   -- rows this role may write
```

A policy's `USING` predicate is AND'd into the `WHERE` of a `SELECT` / `UPDATE` /
`DELETE`, so only permitted rows are returned or affected; its `WITH CHECK`
predicate is validated against each row an `INSERT` / `UPDATE` writes (a violation
raises SQLSTATE `42501`). Identity functions (`current_user` / `session_user`) in
a policy resolve to the session's identity — combined with `SET ROLE`, one
connection can switch which rows it sees. `AS RESTRICTIVE` policies are AND'd with
the OR of the permissive ones; a `FOR` clause scopes a policy to one command and
`TO` scopes it to roles (default `public`). With RLS enabled and no applicable
permissive policy, the table is default-deny (no rows).

Enforcement follows the same opt-in as the rest of the RBAC surface: it bites
only when authorization is active and a superuser (`root`) bypasses it, so trust
mode and the embedded `run_sql` API record policies (visible in
`pg_catalog.pg_policies`) but don't enforce them. **Limitations:** `USING`
injection covers single-table `SELECT` / `UPDATE` / `DELETE` (a join doesn't get
the base table's policy applied); there's no table-owner concept (RLS applies to
every non-`root` role under active authorization, not "everyone except the
owner"); and `FORCE ROW LEVEL SECURITY` is recorded but behaves like `ENABLE`.

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
| DML | `SELECT`, `INSERT` (`VALUES` / `… SELECT`), `INSERT … ON CONFLICT` (`DO NOTHING` / `DO UPDATE`; target by column list or `ON CONSTRAINT <name>`), `UPDATE`, `DELETE`, `RETURNING` (columns + computed expressions) | — |
| Set ops | `UNION`/`UNION ALL`, `INTERSECT`/`INTERSECT ALL`, `EXCEPT`/`EXCEPT ALL` (chained; trailing `ORDER BY`/`LIMIT`) | corresponding-column-name reconciliation, `ORDER BY` over an expression |
| CTEs | `WITH name AS (...)` (multiple, chained) + `WITH RECURSIVE` (anchor `UNION`/`UNION ALL` recursive term, column aliases) on `SELECT` / set-op queries and on `INSERT`/`UPDATE`/`DELETE`/`MERGE` (incl. `WITH RECURSIVE` before a write); data-modifying CTEs (`WITH x AS (INSERT/UPDATE/DELETE … RETURNING …)`) | statement-level snapshot semantics; `WITH CHECK OPTION` |
| `WHERE` | `=` `<>` `<` `<=` `>` `>=`, `IN`, `BETWEEN`, `LIKE`/`ILIKE`, `~`/`~*`/`!~`/`!~*` (POSIX regex), `IS [NOT] NULL`, `AND`/`OR`/`NOT`, jsonb `@>`/`<@` (both directions; `field <@ const` runs residual)/`?`/`?\|`/`?&`, column-to-column + arithmetic, function calls in a comparison (`amt = abs(target)`, single-table + GROUP BY / JOIN pipelines), `IN`/`NOT IN`/scalar `OP (SELECT …)` subqueries (correlated or not), `EXISTS`/`NOT EXISTS` | a comparison function the aggregation engine can't lower (e.g. `substr`) |
| Projection | columns, `*`, aliases, `jsonb` paths, `jsonb_*` functions, `DISTINCT`, `DISTINCT ON (…)`, computed expressions (arithmetic, `\|\|`, `upper`/`lower`/`length`/`substring`/`round`/`coalesce`/`greatest`/...) | computed GROUP BY keys, expressions over an aggregate |
| Aggregates | `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`, `COUNT`/`SUM`/`AVG`(`DISTINCT`) (select list *and* `HAVING`, single-table + JOIN), an **expression over an aggregate** (`sum(x)+1`, `round(avg(x),2)`), a **computed GROUP BY key** (`GROUP BY lower(name)`, `GROUP BY a+b`), `GROUP BY`, `HAVING`, `GROUP BY ROLLUP`/`CUBE`/`GROUPING SETS` (single-table **and over a JOIN**, incl. `HAVING`, `COUNT`/`SUM`/`AVG`(`DISTINCT`) per set, **and a window over it — single-table or over a JOIN**) + the `GROUPING()` super-aggregate helper, **`ORDER BY <position>`** (`ORDER BY 1`) and **`ORDER BY <aggregate>`** (`ORDER BY count(*) DESC`, when selected) | a *computed* grouping key over a JOIN, a correlated WHERE over a JOIN, or a subquery in HAVING alongside a window over GROUPING SETS |
| Window | `ROW_NUMBER`/`RANK`/`DENSE_RANK`/`NTILE`, `FIRST_VALUE`/`LAST_VALUE`/`NTH_VALUE`, `SUM`/`COUNT`/`AVG`/`MIN`/`MAX` `OVER`, `LAG`/`LEAD`, `PARTITION BY`, `ORDER BY`, `ROWS` frames + `RANGE` frames (`UNBOUNDED`/`CURRENT ROW` **and** numeric `n PRECEDING`/`n FOLLOWING` offsets) | non-numeric/interval `RANGE` offset |
| Joins | multi-table `INNER`/`LEFT JOIN`, two-table `RIGHT`/`FULL OUTER JOIN`, a **pure-`RIGHT` adjacent chain of 3+ tables**, `CROSS JOIN` / comma-join, `[LEFT/CROSS] JOIN LATERAL` (single-table subquery, correlate in its `WHERE`), equality + non-equi / `OR` `ON`, JOIN + GROUP BY / aggregates / HAVING | a mixed `LEFT`/`RIGHT` 3+ chain, a non-adjacent `RIGHT` `ON`, `FULL` in a 3+ table chain, `LATERAL` over a join / aggregate subquery |
| DDL | `CREATE TABLE` (incl. `REFERENCES` / `FOREIGN KEY` named or unnamed, `CHECK` / `UNIQUE` — all enforced, literal column `DEFAULT`, `SERIAL`/`BIGSERIAL`/`SMALLSERIAL`, `GENERATED … AS IDENTITY`, `GENERATED ALWAYS AS (…) STORED`, enum-typed columns), `DROP TABLE`, `ALTER TABLE` (`ADD`/`DROP`/`RENAME COLUMN`, `RENAME TO`, `SET`/`DROP NOT NULL`, `ALTER COLUMN TYPE`, `SET`/`DROP DEFAULT`, `ADD [CONSTRAINT] { FOREIGN KEY \| CHECK \| UNIQUE }`, `DROP CONSTRAINT`, multi-action lists `ADD …, DROP …`), `CREATE`/`DROP INDEX` (incl. `UNIQUE`, partial `… WHERE …`), `CREATE`/`DROP`/`ALTER SEQUENCE`, `CREATE TYPE … AS ENUM` / `DROP TYPE`, `CREATE`/`DROP VIEW`, `CREATE MATERIALIZED VIEW` / `REFRESH`, `CREATE [OR REPLACE]`/`DROP FUNCTION` (`LANGUAGE sql`, single-statement body), `COMMENT ON TABLE`/`COLUMN`, **expression column `DEFAULT`** (`now()` / `gen_random_uuid()` / arithmetic, evaluated per row) | a column `DEFAULT` that references another column, `LANGUAGE plpgsql` / multi-statement functions |
| Transactions | `BEGIN`/`COMMIT`/`ROLLBACK`, `SET TRANSACTION` / `BEGIN ISOLATION LEVEL`, `SAVEPOINT`/`RELEASE`/`ROLLBACK TO` (real nested rollback), two-phase commit `PREPARE TRANSACTION` / `COMMIT`/`ROLLBACK PREPARED` (cross-connection, `pg_prepared_xacts`) | prepared xacts surviving a restart, two-phase over the extended protocol |
| Sessions | `LISTEN`/`NOTIFY`/`UNLISTEN` + `pg_notify()` (cross-connection pub/sub), `PREPARE`/`EXECUTE`/`DEALLOCATE` (SQL-level prepared statements), `DECLARE`/`FETCH`/`MOVE`/`CLOSE` (server-side cursors), `EXPLAIN [ANALYZE]` (`FORMAT TEXT`/`JSON`, faithful Index/Seq Scan) | async push to a fully-idle connection, cursor `SCROLL` past materialized rows, per-node `EXPLAIN` costs / timing |
| Protocol | simple + extended query, `$1` params (text + binary), prepared statements, portals, binary result format, `COPY … FROM/TO STDIN/STDOUT` (text + CSV) | binary-format `COPY`, `COPY` from/to a server-side file |
| Auth | trust, SCRAM-SHA-256, TLS, SQL `CREATE`/`ALTER`/`DROP ROLE`/`USER` (reflected via `pg_roles`), `GRANT`/`REVOKE` (accepted) | channel binding, mTLS, enforced privileges, SQL roles wired to SCRAM login |
| Catalog | `information_schema`, `pg_catalog` (`pg_index`/`pg_constraint`/`pg_am`/...), catalog *joins*, full SQLAlchemy reflection (`get_table_names`/`has_table`/`get_columns`/`get_pk_constraint`/`get_indexes`/`get_foreign_keys`, `Table(autoload_with=...)`, `get_foreign_keys`, `get_table_comment` + column comments) | `get_check_constraints`, `get_unique_constraints` |

Anything outside the supported set returns a faithful SQLSTATE error rather than
a wrong answer — the same "honest *not supported* over a half-feature" discipline
the [compatibility](compatibility.md) page describes for the MongoDB side.
