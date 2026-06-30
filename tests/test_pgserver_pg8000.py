"""P7 gauge: a real third-party driver (pg8000) + SQLAlchemy against the server.

``pg8000`` is a pure-Python PostgreSQL driver (no libpq), so unlike psql/psycopg
it runs in this environment. It uses the extended query protocol and is strict
about the wire format, so it validates the server far more harshly than the
hand-rolled wire clients in the other test modules — it found the
adjacent-``$1,$2`` tokenizer bug and the ``pg_catalog.version()`` gap these
tests now guard against.
"""

from __future__ import annotations

import datetime as _dt
import socket
import ssl
import struct
from decimal import Decimal

import bson
import pytest
import trustme

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from sqlfake import FakeStorage

pg8000 = pytest.importorskip("pg8000.dbapi")


@pytest.fixture
def server():
    srv = SecantusPGServer(port=0, storage=FakeStorage())
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


def connect(srv, **kw):
    host, port = srv.address
    return pg8000.connect(user="joe", host=host, port=port, database="db", **kw)


# --------------------------------------------------------------------------- #


def test_connect_and_select_one(server):
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT 1")
    assert cur.fetchall() == ([1],)
    conn.close()


def test_crud_with_bound_parameters(server):
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE users (id bigint primary key, name text, age int)")
    # Adjacent params with no spaces — the case that broke sqlglot tokenizing.
    cur.execute("INSERT INTO users (id,name,age) VALUES (%s,%s,%s)", (1, "alice", 30))
    cur.execute("INSERT INTO users (id,name,age) VALUES (%s,%s,%s)", (2, "bob", 17))
    cur.execute("SELECT id, name FROM users WHERE age > %s ORDER BY id", (18,))
    assert cur.fetchall() == ([1, "alice"],)
    cur.execute("UPDATE users SET age = %s WHERE id = %s", (18, 2))
    assert cur.rowcount == 1
    cur.execute("DELETE FROM users WHERE age < %s", (18,))
    cur.execute("SELECT COUNT(*) FROM users")
    assert cur.fetchall() == ([2],)
    conn.close()


def test_returning_via_driver(server):
    # RETURNING makes a DML statement emit a result set, so the driver fetches
    # rows from INSERT / UPDATE / DELETE just like a SELECT.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, name text, n int)")
    cur.execute("INSERT INTO t (id, name, n) VALUES (1, 'a', 10), (2, 'b', 20) RETURNING id, name")
    assert cur.fetchall() == ([1, "a"], [2, "b"])
    cur.execute("UPDATE t SET n = 99 WHERE id = 1 RETURNING id, n")
    assert cur.fetchall() == ([1, 99],)
    cur.execute("DELETE FROM t WHERE n > 50 RETURNING *")
    assert cur.fetchall() == ([1, "a", 99],)
    cur.execute("SELECT id FROM t ORDER BY id")
    assert cur.fetchall() == ([2],)
    conn.close()


def test_window_functions_via_driver(server):
    # Window functions over Mongo-written data, through the real driver.
    server.storage.insert(
        "db",
        "sales",
        [
            {"_id": bson.Int64(i), "region": r, "amount": bson.Int64(a)}
            for i, (r, a) in enumerate([("e", 10), ("e", 30), ("w", 20), ("w", 50)], 1)
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "SELECT _id, ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount) AS rn, "
        "SUM(amount) OVER (PARTITION BY region) AS tot FROM sales ORDER BY _id"
    )
    assert cur.fetchall() == ([1, 1, 40], [2, 2, 40], [3, 1, 70], [4, 2, 70])
    conn.close()


def test_window_frames_via_driver(server):
    # Explicit ROWS frame + value/NTILE functions over the real driver.
    server.storage.insert(
        "db",
        "sales",
        [
            {"_id": bson.Int64(i), "amount": bson.Int64(a)}
            for i, a in enumerate([10, 20, 30, 40], 1)
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "SELECT _id, "
        "SUM(amount) OVER (ORDER BY _id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS run, "
        "FIRST_VALUE(amount) OVER (ORDER BY _id) AS fv, "
        "NTILE(2) OVER (ORDER BY _id) AS nt "
        "FROM sales ORDER BY _id"
    )
    assert cur.fetchall() == ([1, 10, 10, 1], [2, 30, 10, 1], [3, 50, 10, 2], [4, 70, 10, 2])
    conn.close()


def test_insert_select_via_driver(server):
    # INSERT ... SELECT over Mongo-written data, through the real driver.
    server.storage.insert(
        "db",
        "src",
        [{"_id": bson.Int64(i), "n": bson.Int64(v)} for i, v in enumerate([10, 20, 30], 1)],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE dst (id bigint primary key, n int)")
    cur.execute("INSERT INTO dst (id, n) SELECT _id, n FROM src WHERE n >= 20")
    assert cur.rowcount == 2
    cur.execute("SELECT id, n FROM dst ORDER BY id")
    assert cur.fetchall() == ([2, 20], [3, 30])
    conn.close()


def test_on_conflict_via_driver(server):
    # INSERT ... ON CONFLICT DO UPDATE upsert through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, n int)")
    cur.execute("INSERT INTO t (id, n) VALUES (1, 5)")
    cur.execute(
        "INSERT INTO t (id, n) VALUES (1, 10) "
        "ON CONFLICT (id) DO UPDATE SET n = t.n + EXCLUDED.n RETURNING id, n"
    )
    assert cur.fetchall() == ([1, 15],)
    cur.execute("INSERT INTO t (id, n) VALUES (1, 99) ON CONFLICT (id) DO NOTHING")
    assert cur.rowcount == 0
    cur.execute("SELECT id, n FROM t ORDER BY id")
    assert cur.fetchall() == ([1, 15],)
    conn.close()


def test_outer_join_via_driver(server):
    # RIGHT and FULL OUTER joins over Mongo-written data, through the real driver.
    server.storage.insert(
        "db", "a", [{"_id": bson.Int64(1), "av": "a1"}, {"_id": bson.Int64(2), "av": "a2"}]
    )
    server.storage.insert(
        "db",
        "b",
        [
            {"_id": bson.Int64(10), "aid": bson.Int64(1), "bv": "b1"},
            {"_id": bson.Int64(11), "aid": bson.Int64(99), "bv": "b3"},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    # ORDER BY ... NULLS LAST is now honored, so the server returns a stable order.
    cur.execute("SELECT a.av, b.bv FROM a RIGHT JOIN b ON a._id = b.aid ORDER BY a.av NULLS LAST")
    assert cur.fetchall() == (["a1", "b1"], [None, "b3"])
    cur.execute(
        "SELECT a.av, b.bv FROM a FULL JOIN b ON a._id = b.aid ORDER BY a.av NULLS LAST, b.bv"
    )
    assert cur.fetchall() == (["a1", "b1"], ["a2", None], [None, "b3"])
    conn.close()


def test_nulls_ordering_via_driver(server):
    # ORDER BY NULL placement (Postgres default + explicit NULLS FIRST/LAST).
    server.storage.insert(
        "db",
        "t",
        [
            {"_id": bson.Int64(1), "n": bson.Int64(5)},
            {"_id": bson.Int64(2)},
            {"_id": bson.Int64(3), "n": bson.Int64(3)},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT n FROM t ORDER BY n")  # ASC default -> NULLs last
    assert cur.fetchall() == ([3], [5], [None])
    cur.execute("SELECT n FROM t ORDER BY n DESC")  # DESC default -> NULLs first
    assert cur.fetchall() == ([None], [5], [3])
    cur.execute("SELECT n FROM t ORDER BY n NULLS FIRST")
    assert cur.fetchall() == ([None], [3], [5])
    conn.close()


def test_cte_via_driver(server):
    # WITH ... AS (...) over Mongo-written data, through the real driver.
    server.storage.insert(
        "db",
        "sales",
        [
            {"_id": bson.Int64(i), "region": r, "amount": bson.Int64(a)}
            for i, (r, a) in enumerate([("e", 10), ("e", 20), ("w", 30), ("w", 5)], 1)
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "WITH big AS (SELECT region, amount FROM sales WHERE amount >= 20) "
        "SELECT region, amount FROM big ORDER BY amount"
    )
    assert cur.fetchall() == (["e", 20], ["w", 30])
    cur.execute(
        "WITH t AS (SELECT region, amount FROM sales) "
        "SELECT region, SUM(amount) FROM t GROUP BY region ORDER BY region"
    )
    assert cur.fetchall() == (["e", 30], ["w", 35])
    conn.close()


def test_recursive_cte_via_driver(server):
    # WITH RECURSIVE: walk an org hierarchy over Mongo-written data via the driver.
    server.storage.insert(
        "db",
        "emp",
        [
            {"_id": bson.Int64(i), "name": n, "mgr": bson.Int64(m)}
            for i, n, m in [(1, "ceo", 0), (2, "vp", 1), (3, "eng", 2)]
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "WITH RECURSIVE chain(id, name, lvl) AS ("
        "  SELECT _id, name, 0 FROM emp WHERE _id = 1"
        "  UNION ALL"
        "  SELECT e._id, e.name, c.lvl + 1 FROM emp e JOIN chain c ON e.mgr = c.id"
        ") SELECT id, name, lvl FROM chain ORDER BY id"
    )
    assert cur.fetchall() == ([1, "ceo", 0], [2, "vp", 1], [3, "eng", 2])
    conn.close()


def test_count_distinct_via_driver(server):
    # COUNT(DISTINCT) / SUM(DISTINCT) over Mongo-written data, through the driver.
    server.storage.insert(
        "db",
        "sales",
        [
            {"_id": bson.Int64(i), "region": r, "amount": bson.Int64(a)}
            for i, (r, a) in enumerate([("e", 10), ("e", 20), ("e", 20), ("w", 30), ("w", 30)], 1)
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT COUNT(DISTINCT amount) FROM sales")
    assert cur.fetchall() == ([3],)
    cur.execute("SELECT region, COUNT(DISTINCT amount) FROM sales GROUP BY region ORDER BY region")
    assert cur.fetchall() == (["e", 2], ["w", 1])
    conn.close()


def test_set_operations_via_driver(server):
    # UNION / INTERSECT / EXCEPT over Mongo-written data, through the real driver.
    server.storage.insert(
        "db",
        "a",
        [{"_id": bson.Int64(i), "n": bson.Int64(v)} for i, v in enumerate([1, 2, 2, 3], 1)],
    )
    server.storage.insert(
        "db",
        "b",
        [{"_id": bson.Int64(i), "n": bson.Int64(v)} for i, v in enumerate([2, 3, 4], 1)],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT n FROM a UNION SELECT n FROM b ORDER BY n")
    assert cur.fetchall() == ([1], [2], [3], [4])
    cur.execute("SELECT n FROM a INTERSECT SELECT n FROM b ORDER BY n")
    assert cur.fetchall() == ([2], [3])
    cur.execute("SELECT n FROM a EXCEPT SELECT n FROM b ORDER BY n")
    assert cur.fetchall() == ([1],)
    cur.execute("SELECT n FROM a UNION ALL SELECT n FROM b ORDER BY n")
    assert cur.fetchall() == ([1], [2], [2], [2], [3], [3], [4])
    conn.close()


def test_types_roundtrip(server):
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "CREATE TABLE m (id bigint primary key, price numeric, flag boolean, at timestamptz)"
    )
    cur.execute(
        "INSERT INTO m (id, price, flag, at) VALUES (1, 19.99, true, '2020-01-02T03:04:05Z')"
    )
    cur.execute("SELECT id, price, flag, at FROM m")
    (row,) = cur.fetchall()
    assert row[0] == 1  # bigint -> int
    assert row[1] == Decimal("19.99")  # numeric -> Decimal
    assert row[2] is True  # boolean
    at = row[3]  # timestamptz -> datetime at the right instant
    assert isinstance(at, _dt.datetime)
    assert (at.year, at.month, at.day, at.hour, at.minute, at.second) == (2020, 1, 2, 3, 4, 5)
    conn.close()


def test_group_by_and_join(server):
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE sales (id bigint primary key, region text, amount int)")
    for i, (r, a) in enumerate([("e", 10), ("e", 20), ("w", 30)], 1):
        cur.execute("INSERT INTO sales (id,region,amount) VALUES (%s,%s,%s)", (i, r, a))
    cur.execute("SELECT region, SUM(amount) FROM sales GROUP BY region ORDER BY region")
    assert cur.fetchall() == (["e", 30], ["w", 30])

    cur.execute("CREATE TABLE customers (id bigint primary key, name text)")
    cur.execute("CREATE TABLE orders (id bigint primary key, cust_id bigint)")
    cur.execute("INSERT INTO customers (id,name) VALUES (%s,%s)", (1, "alice"))
    cur.execute("INSERT INTO orders (id,cust_id) VALUES (%s,%s)", (10, 1))
    cur.execute(
        "SELECT o.id, c.name FROM orders o JOIN customers c ON o.cust_id = c.id ORDER BY o.id"
    )
    assert cur.fetchall() == ([10, "alice"],)
    conn.close()


def test_three_table_join_and_distinct(server):
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE customers (id bigint primary key, name text)")
    cur.execute("CREATE TABLE products (id bigint primary key, pname text)")
    cur.execute("CREATE TABLE orders (id bigint primary key, cust_id bigint, prod_id bigint)")
    cur.execute("INSERT INTO customers (id,name) VALUES (%s,%s),(%s,%s)", (1, "alice", 2, "bob"))
    cur.execute("INSERT INTO products (id,pname) VALUES (%s,%s),(%s,%s)", (100, "gear", 101, "box"))
    for oid, cid, pid in [(10, 1, 100), (11, 1, 101), (12, 2, 100)]:
        cur.execute("INSERT INTO orders (id,cust_id,prod_id) VALUES (%s,%s,%s)", (oid, cid, pid))
    cur.execute(
        "SELECT c.name, p.pname FROM orders o "
        "JOIN customers c ON o.cust_id = c.id "
        "JOIN products p ON o.prod_id = p.id ORDER BY c.name, p.pname"
    )
    assert cur.fetchall() == (["alice", "box"], ["alice", "gear"], ["bob", "gear"])
    cur.execute("SELECT DISTINCT cust_id FROM orders ORDER BY cust_id")
    assert cur.fetchall() == ([1], [2])
    conn.close()


def test_reflected_table_and_jsonb(server):
    # Mongo-written data (no CREATE TABLE) read via the real driver.
    server.storage.insert(
        "db",
        "people",
        [
            {"_id": bson.Int64(1), "name": "alice", "profile": {"city": "NYC"}},
            {"_id": bson.Int64(2), "name": "bob", "profile": {"city": "LA"}},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT name, profile->>'city' FROM people ORDER BY _id")
    assert cur.fetchall() == (["alice", "NYC"], ["bob", "LA"])
    conn.close()


def test_write_to_reflected_table(server):
    # Dual-protocol writes: INSERT/UPDATE/DELETE on a Mongo-written collection
    # with no CREATE TABLE, then confirm the change is visible as a real document.
    server.storage.insert(
        "db",
        "people",
        [
            {"_id": bson.Int64(1), "name": "alice", "age": bson.Int64(30)},
            {"_id": bson.Int64(2), "name": "bob", "age": bson.Int64(17)},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("INSERT INTO people (_id, name, age) VALUES (3, 'dave', 40)")
    conn.commit()
    cur.execute("UPDATE people SET age = 99 WHERE name = 'alice'")
    conn.commit()
    cur.execute("DELETE FROM people WHERE age < 18")
    conn.commit()
    cur.execute("SELECT _id, name, age FROM people ORDER BY _id")
    assert cur.fetchall() == ([1, "alice", 99], [3, "dave", 40])
    # The inserted row is a genuine Mongo document (what pymongo would read).
    stored = server.storage.find_matching("db", "people", {"_id": bson.Int64(3)})
    assert stored[0]["name"] == "dave" and stored[0]["age"] == 40
    conn.close()


def test_where_subquery_via_driver(server):
    # Non-correlated IN / scalar subqueries over Mongo-written data, through the
    # real driver.
    server.storage.insert(
        "db",
        "customers",
        [
            {"_id": bson.Int64(1), "name": "alice"},
            {"_id": bson.Int64(2), "name": "bob"},
            {"_id": bson.Int64(3), "name": "carol"},
        ],
    )
    server.storage.insert(
        "db",
        "orders",
        [
            {"_id": bson.Int64(10), "cust": bson.Int64(1)},
            {"_id": bson.Int64(11), "cust": bson.Int64(3)},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT name FROM customers WHERE _id IN (SELECT cust FROM orders) ORDER BY name")
    assert cur.fetchall() == (["alice"], ["carol"])
    cur.execute("SELECT name FROM customers WHERE _id = (SELECT max(cust) FROM orders)")
    assert cur.fetchall() == (["carol"],)
    conn.close()


def test_correlated_subquery_via_driver(server):
    # Correlated EXISTS / NOT EXISTS over Mongo-written data, through the real
    # driver: customers with (or without) a matching order.
    server.storage.insert(
        "db",
        "customers",
        [
            {"_id": bson.Int64(1), "name": "alice"},
            {"_id": bson.Int64(2), "name": "bob"},
            {"_id": bson.Int64(3), "name": "carol"},
        ],
    )
    server.storage.insert(
        "db",
        "orders",
        [
            {"_id": bson.Int64(10), "cust": bson.Int64(1)},
            {"_id": bson.Int64(11), "cust": bson.Int64(3)},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "SELECT name FROM customers c WHERE EXISTS "
        "(SELECT 1 FROM orders o WHERE o.cust = c._id) ORDER BY name"
    )
    assert cur.fetchall() == (["alice"], ["carol"])
    cur.execute(
        "SELECT name FROM customers c WHERE NOT EXISTS "
        "(SELECT 1 FROM orders o WHERE o.cust = c._id) ORDER BY name"
    )
    assert cur.fetchall() == (["bob"],)
    conn.close()


def test_computed_expressions_via_driver(server):
    # Arithmetic + scalar functions in the SELECT list, over Mongo-written data,
    # through the real driver.
    server.storage.insert(
        "db",
        "items",
        [
            {"_id": bson.Int64(1), "name": "Apple", "price": bson.Int64(10), "qty": bson.Int64(3)},
            {"_id": bson.Int64(2), "name": "pear", "price": bson.Int64(7), "qty": bson.Int64(2)},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT upper(name), price * qty FROM items ORDER BY _id")
    assert cur.fetchall() == (["APPLE", 30], ["PEAR", 14])
    cur.execute("SELECT name || ' x' AS label FROM items WHERE _id = 1")
    assert cur.fetchall() == (["Apple x"],)
    conn.close()


def test_join_group_by_via_driver(server):
    # JOIN + GROUP BY + aggregate + HAVING over Mongo-written data, through the
    # real driver — the canonical analytics query.
    server.storage.insert(
        "db",
        "customers",
        [
            {"_id": bson.Int64(1), "region": "east"},
            {"_id": bson.Int64(2), "region": "east"},
            {"_id": bson.Int64(3), "region": "west"},
        ],
    )
    server.storage.insert(
        "db",
        "orders",
        [
            {"_id": bson.Int64(10), "cust": bson.Int64(1), "total": bson.Int64(100)},
            {"_id": bson.Int64(11), "cust": bson.Int64(2), "total": bson.Int64(200)},
            {"_id": bson.Int64(12), "cust": bson.Int64(3), "total": bson.Int64(30)},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "SELECT c.region, SUM(o.total) FROM orders o "
        "JOIN customers c ON o.cust = c._id GROUP BY c.region ORDER BY c.region"
    )
    assert cur.fetchall() == (["east", 300], ["west", 30])
    cur.execute(
        "SELECT c.region, SUM(o.total) AS s FROM orders o "
        "JOIN customers c ON o.cust = c._id GROUP BY c.region HAVING SUM(o.total) > 100"
    )
    assert cur.fetchall() == (["east", 300],)
    conn.close()


def test_column_to_column_where_via_driver(server):
    # A WHERE comparing two columns (and a column to an arithmetic expression),
    # over Mongo-written data, through the real driver.
    server.storage.insert(
        "db",
        "orders",
        [
            {"_id": bson.Int64(1), "qty": bson.Int64(5), "shipped": bson.Int64(5)},
            {"_id": bson.Int64(2), "qty": bson.Int64(8), "shipped": bson.Int64(3)},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT _id FROM orders WHERE qty > shipped ORDER BY _id")
    assert cur.fetchall() == ([2],)
    cur.execute("SELECT _id FROM orders WHERE qty = shipped ORDER BY _id")
    assert cur.fetchall() == ([1],)
    conn.close()


def test_jsonb_operators_via_driver(server):
    # jsonb containment/existence operators + a jsonb function over Mongo-written
    # data (no CREATE TABLE), through the real driver.
    server.storage.insert(
        "db",
        "docs",
        [
            {"_id": bson.Int64(1), "data": {"a": 1, "tags": ["x", "y"]}},
            {"_id": bson.Int64(2), "data": {"a": 9, "c": 3, "tags": ["y", "z"]}},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT _id FROM docs WHERE data @> '{\"a\":1}' ORDER BY _id")
    assert cur.fetchall() == ([1],)
    cur.execute("SELECT _id FROM docs WHERE data ? 'c' ORDER BY _id")
    assert cur.fetchall() == ([2],)
    cur.execute("SELECT _id FROM docs WHERE data ?| array['c','z'] ORDER BY _id")
    assert cur.fetchall() == ([2],)
    cur.execute("SELECT jsonb_array_length(data #> '{tags}') FROM docs WHERE _id = 2")
    assert cur.fetchall() == ([2],)
    conn.close()


def test_reflected_aggregate_and_join(server):
    # Mongo-written data (no CREATE TABLE) analysed with GROUP BY + JOIN through
    # the real driver — the dual-protocol analytics path.
    server.storage.insert(
        "db",
        "sales",
        [
            {"_id": bson.Int64(1), "region": "e", "amount": bson.Int64(10)},
            {"_id": bson.Int64(2), "region": "e", "amount": bson.Int64(20)},
            {"_id": bson.Int64(3), "region": "w", "amount": bson.Int64(30)},
        ],
    )
    server.storage.insert("db", "people", [{"_id": bson.Int64(1), "name": "alice"}])
    server.storage.insert(
        "db", "purchases", [{"_id": bson.Int64(9), "buyer": bson.Int64(1), "item": "gear"}]
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT region, SUM(amount) FROM sales GROUP BY region ORDER BY region")
    assert cur.fetchall() == (["e", 30], ["w", 30])
    cur.execute(
        "SELECT p.item, pe.name FROM purchases p JOIN people pe ON p.buyer = pe._id ORDER BY p.item"
    )
    assert cur.fetchall() == (["gear", "alice"],)
    conn.close()


def test_catalog_column_introspection_via_driver(server):
    # A pg_catalog column-metadata query (format_type + correlated subquery +
    # CASE + compound ON) through the real driver — the get_columns building
    # blocks evaluated per row on the wire path.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, label text, n int)")
    cur.execute(
        "SELECT a.attname, format_type(a.atttypid, a.atttypmod) AS typ, "
        "(SELECT d.adbin FROM pg_catalog.pg_attrdef d "
        " WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum) AS deflt "
        "FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "WHERE c.relname = 't' ORDER BY a.attnum"
    )
    assert cur.fetchall() == (
        ["id", "bigint", None],
        ["label", "text", None],
        ["n", "integer", None],
    )
    conn.close()


def test_session_functions(server):
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT version()")
    assert cur.fetchall()[0][0].startswith("PostgreSQL 15.0 (SecantusDB)")
    cur.execute("SELECT current_database()")
    assert cur.fetchall() == (["db"],)
    conn.close()


# -- auth / TLS via the real driver ------------------------------------------ #


def test_scram_auth_success_and_failure():
    srv = SecantusPGServer(
        port=0, storage=FakeStorage(), require_auth=True, users={"joe": "s3cret"}
    )
    srv.start()
    try:
        host, port = srv.address
        conn = pg8000.connect(user="joe", password="s3cret", host=host, port=port, database="db")
        cur = conn.cursor()
        cur.execute("SELECT 1")
        assert cur.fetchall() == ([1],)
        conn.close()
        with pytest.raises(pg8000.DatabaseError):
            pg8000.connect(user="joe", password="wrong", host=host, port=port, database="db")
    finally:
        srv.stop()


def test_tls_connection(tmp_path):
    ca = trustme.CA()
    cert = ca.issue_cert("127.0.0.1")
    cert_file, key_file = tmp_path / "c.pem", tmp_path / "c.key"
    cert.cert_chain_pems[0].write_to_path(cert_file)
    cert.private_key_pem.write_to_path(key_file)
    srv = SecantusPGServer(
        port=0, storage=FakeStorage(), tls_cert_file=str(cert_file), tls_key_file=str(key_file)
    )
    srv.start()
    try:
        host, port = srv.address
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ca.configure_trust(ctx)
        conn = pg8000.connect(user="joe", host=host, port=port, database="db", ssl_context=ctx)
        cur = conn.cursor()
        cur.execute("SELECT 42")
        assert cur.fetchall() == ([42],)
        conn.close()
    finally:
        srv.stop()


# -- SQLAlchemy (uses pg8000 as its driver) ---------------------------------- #


def test_sqlalchemy_core_roundtrip(server):
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.connect() as conn:
            conn.execute(sa.text("CREATE TABLE widgets (id bigint primary key, label text)"))
            conn.execute(sa.text("INSERT INTO widgets (id, label) VALUES (1, 'gear')"))
            rows = conn.execute(sa.text("SELECT id, label FROM widgets")).fetchall()
        assert rows == [(1, "gear")]
    finally:
        engine.dispose()


def test_sqlalchemy_reflection_table_names(server):
    # SQLAlchemy's introspection joins pg_catalog.pg_class ⋈ pg_namespace — the
    # catalog-join path that unblocks reflection / interactive psql's \dt.
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:  # begin() commits — connect() would roll back
            conn.execute(sa.text("CREATE TABLE widgets (id bigint primary key, label text)"))
            conn.execute(sa.text("CREATE TABLE gadgets (id bigint primary key, n int)"))
        insp = sa.inspect(engine)
        assert sorted(insp.get_table_names()) == ["gadgets", "widgets"]
        assert insp.has_table("widgets") is True
        assert insp.has_table("nonexistent") is False
    finally:
        engine.dispose()


def test_catalog_join_via_driver(server):
    # The raw catalog join, exercised through the extended protocol.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE a (id bigint primary key)")
    cur.execute("CREATE TABLE b (id bigint primary key)")
    cur.execute(
        "SELECT c.relname FROM pg_catalog.pg_class c "
        "JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace "
        "WHERE n.nspname = %s AND c.relkind = 'r' ORDER BY c.relname",
        ("public",),
    )
    assert cur.fetchall() == (["a"], ["b"])
    conn.close()


def test_sqlalchemy_get_columns_reflection(server):
    # The headline: SQLAlchemy's inspect().get_columns() runs its full pg_catalog
    # column query (4-table outer join, compound ON, format_type, correlated
    # subqueries, CASE) plus the domain/enum derived-table queries, and returns
    # typed column metadata.
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE users (id bigint primary key, name text, age int)"))
        cols = sa.inspect(engine).get_columns("users")
        assert [c["name"] for c in cols] == ["id", "name", "age"]
        assert [type(c["type"]).__name__ for c in cols] == ["BIGINT", "TEXT", "INTEGER"]
        # The PK column reflects NOT NULL; the others nullable.
        assert [c["nullable"] for c in cols] == [False, True, True]
    finally:
        engine.dispose()


def test_sqlalchemy_full_reflection(server):
    # The headline: full Table(autoload_with=...) reflection — columns, primary
    # key, and indexes — via SQLAlchemy's pg_index/pg_constraint queries (which
    # use unnest / generate_subscripts set-returning functions + array_agg over
    # a derived table).
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE users (id bigint primary key, name text, age int)"))
            conn.execute(sa.text("CREATE INDEX ix_name ON users (name)"))
        insp = sa.inspect(engine)
        assert insp.get_pk_constraint("users")["constrained_columns"] == ["id"]
        idx = insp.get_indexes("users")
        assert [(i["name"], i["column_names"], i["unique"]) for i in idx] == [
            ("ix_name", ["name"], False)
        ]
        t = sa.Table("users", sa.MetaData(), autoload_with=engine)
        assert [c.name for c in t.columns] == ["id", "name", "age"]
        assert [c.name for c in t.primary_key.columns] == ["id"]
        assert {ix.name for ix in t.indexes} == {"ix_name"}
    finally:
        engine.dispose()


def test_sqlalchemy_get_foreign_keys_empty(server):
    # We model no foreign keys, so get_foreign_keys() reflects empty (no error).
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE users (id bigint primary key, name text)"))
        assert sa.inspect(engine).get_foreign_keys("users") == []
    finally:
        engine.dispose()


def test_transaction_commit_and_rollback(server):
    conn = connect(server)
    conn.autocommit = False
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, n int)")
    conn.commit()
    # Roll back an insert — it must not survive.
    cur.execute("INSERT INTO t (id, n) VALUES (1, 10)")
    conn.rollback()
    cur.execute("SELECT COUNT(*) FROM t")
    assert cur.fetchall() == ([0],)
    # Commit an insert — it persists.
    cur.execute("INSERT INTO t (id, n) VALUES (2, 20)")
    conn.commit()
    cur.execute("SELECT COUNT(*) FROM t")
    assert cur.fetchall() == ([1],)
    conn.close()


def test_create_index_and_isolation_via_driver(server):
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, label text, n int)")
    cur.execute("CREATE INDEX ix_n ON t (n)")
    cur.execute("CREATE UNIQUE INDEX ux_label ON t (label)")
    assert {ix["name"] for ix in server.storage.list_indexes("db", "t")} == {"ix_n", "ux_label"}
    cur.execute("DROP INDEX ix_n")
    assert {ix["name"] for ix in server.storage.list_indexes("db", "t")} == {"ux_label"}
    # Isolation / read-only characteristics are accepted (single-node no-op).
    cur.execute("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
    cur.execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE")
    conn.close()


def test_ssl_request_declined_without_tls(server):
    # Sanity: a raw SSLRequest is declined when TLS isn't configured.
    host, port = server.address
    s = socket.create_connection((host, port), timeout=5)
    s.sendall(struct.pack("!ii", 8, pgwire.SSL_REQUEST_CODE))
    assert s.recv(1) == b"N"
    s.close()
