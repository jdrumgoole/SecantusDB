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


def test_fromless_expression_and_where_via_driver(server):
    # FROM-less constant expression + a constant WHERE, through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT 2 * 3 AS six")
    assert cur.fetchall() == ([6],)
    cur.execute("SELECT 1 AS x WHERE 1 = 0")
    assert cur.fetchall() == ()
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
    # A computed RETURNING expression, evaluated per returned row.
    cur.execute(
        "INSERT INTO t (id, name, n) VALUES (3, 'c', 4) RETURNING n * 2 AS dbl, upper(name)"
    )
    assert cur.fetchall() == ([8, "C"],)
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


def test_window_over_group_by_via_driver(server):
    # A window function that ranks GROUP BY aggregates, through the real driver.
    server.storage.insert(
        "db",
        "sales",
        [
            {"_id": bson.Int64(i), "region": r, "amount": bson.Int64(a)}
            for i, (r, a) in enumerate([("e", 10), ("e", 20), ("w", 30), ("w", 5), ("w", 15)], 1)
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "SELECT region, SUM(amount) AS s, "
        "RANK() OVER (ORDER BY SUM(amount) DESC) AS rk, "
        "SUM(SUM(amount)) OVER () AS total "
        "FROM sales GROUP BY region ORDER BY region"
    )
    # e: 30, w: 50 → ranked desc w=1, e=2; grand total = 80 on every row.
    assert cur.fetchall() == (["e", 30, 2, 80], ["w", 50, 1, 80])
    conn.close()


def test_window_over_join_group_via_driver(server):
    # A window ranking GROUP BY aggregates that span a JOIN, through the driver.
    server.storage.insert(
        "db",
        "orders",
        [
            {"_id": bson.Int64(i), "cid": bson.Int64(c), "amt": bson.Int64(a)}
            for i, (c, a) in enumerate([(1, 10), (1, 20), (2, 30), (2, 5), (3, 40)], 1)
        ],
    )
    server.storage.insert(
        "db",
        "customers",
        [{"_id": bson.Int64(i), "region": r} for i, r in [(1, "e"), (2, "e"), (3, "w")]],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "SELECT c.region, SUM(o.amt) AS s, "
        "RANK() OVER (ORDER BY SUM(o.amt) DESC) AS rk, "
        "SUM(SUM(o.amt)) OVER () AS total "
        "FROM orders o JOIN customers c ON o.cid = c._id GROUP BY c.region ORDER BY c.region"
    )
    # region e total = 65, w = 40 → ranked desc e=1, w=2; grand total 105.
    assert cur.fetchall() == (["e", 65, 1, 105], ["w", 40, 2, 105])
    conn.close()


def test_join_where_subquery_via_driver(server):
    # A scalar WHERE-subquery inside a JOIN query, through the real driver.
    server.storage.insert(
        "db",
        "o",
        [
            {"_id": bson.Int64(i), "cid": bson.Int64(c), "amt": bson.Int64(a)}
            for i, c, a in [(1, 1, 10), (2, 1, 50)]
        ],
    )
    server.storage.insert("db", "c", [{"_id": bson.Int64(1), "region": "e"}])
    server.storage.insert("db", "lim", [{"_id": bson.Int64(1), "cap": bson.Int64(40)}])
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT o._id FROM o JOIN c ON o.cid = c._id WHERE o.amt > (SELECT cap FROM lim)")
    assert cur.fetchall() == ([2],)
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


def test_merge_via_driver(server):
    # MERGE upsert (matched UPDATE + not-matched INSERT + conditional DELETE)
    # over Mongo-written data, through the real driver.
    server.storage.insert(
        "db",
        "tgt",
        [{"_id": bson.Int64(i), "amt": bson.Int64(a)} for i, a in [(1, 10), (2, 20), (3, 30)]],
    )
    server.storage.insert(
        "db",
        "src",
        [{"_id": bson.Int64(i), "amt": bson.Int64(a)} for i, a in [(1, 0), (2, 200), (5, 50)]],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "MERGE INTO tgt t USING src s ON t._id = s._id "
        "WHEN MATCHED AND s.amt = 0 THEN DELETE "
        "WHEN MATCHED THEN UPDATE SET amt = t.amt + s.amt "
        "WHEN NOT MATCHED THEN INSERT (_id, amt) VALUES (s._id, s.amt)"
    )
    assert cur.rowcount == 3  # 1 deleted, 1 updated, 1 inserted
    cur.execute("SELECT _id, amt FROM tgt ORDER BY _id")
    # id1 deleted, id2 = 20+200, id3 untouched, id5 inserted.
    assert cur.fetchall() == ([2, 220], [3, 30], [5, 50])
    conn.close()


def test_merge_returning_and_by_source_via_driver(server):
    # MERGE with RETURNING and WHEN NOT MATCHED BY SOURCE, through the driver.
    server.storage.insert(
        "db",
        "tgt",
        [{"_id": bson.Int64(i), "amt": bson.Int64(a)} for i, a in [(1, 10), (2, 20), (3, 30)]],
    )
    server.storage.insert("db", "src", [{"_id": bson.Int64(1), "amt": bson.Int64(100)}])
    conn = connect(server)
    cur = conn.cursor()
    # id1 matched → UPDATE; id2/id3 unmatched by source → set to 0; RETURNING the
    # affected rows' post-images.
    cur.execute(
        "MERGE INTO tgt t USING src s ON t._id = s._id "
        "WHEN MATCHED THEN UPDATE SET amt = s.amt "
        "WHEN NOT MATCHED BY SOURCE THEN UPDATE SET amt = 0 "
        "RETURNING t._id, t.amt"
    )
    assert sorted(cur.fetchall()) == [[1, 100], [2, 0], [3, 0]]
    cur.execute("SELECT _id, amt FROM tgt ORDER BY _id")
    assert cur.fetchall() == ([1, 100], [2, 0], [3, 0])
    conn.close()


def test_with_insert_via_driver(server):
    # WITH ... INSERT ... SELECT FROM cte over the real driver.
    server.storage.insert(
        "db",
        "src",
        [{"_id": bson.Int64(i), "n": bson.Int64(v)} for i, v in [(1, 10), (2, 40)]],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE dst (id bigint primary key, n int)")
    cur.execute(
        "WITH big AS (SELECT _id, n FROM src WHERE n >= 20) "
        "INSERT INTO dst (id, n) SELECT _id, n FROM big"
    )
    assert cur.rowcount == 1
    cur.execute("SELECT id, n FROM dst ORDER BY id")
    assert cur.fetchall() == ([2, 40],)
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


def test_cross_join_via_driver(server):
    # CROSS JOIN / comma-join cartesian product over the real driver.
    server.storage.insert(
        "db",
        "a",
        [
            {"_id": bson.Int64(1), "av": bson.Int64(10)},
            {"_id": bson.Int64(2), "av": bson.Int64(20)},
        ],
    )
    server.storage.insert("db", "b", [{"_id": bson.Int64(1), "bv": bson.Int64(100)}])
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("SELECT a.av, b.bv FROM a CROSS JOIN b ORDER BY a.av")
    assert cur.fetchall() == ([10, 100], [20, 100])
    cur.execute("SELECT a.av, b.bv FROM a, b WHERE a._id = 1")
    assert cur.fetchall() == ([10, 100],)
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


def test_correlated_subquery_in_join_and_group_via_driver(server):
    # A correlated EXISTS in the WHERE of a JOIN and of a GROUP BY, through the
    # real driver — the two pipeline paths this slice added.
    server.storage.insert(
        "db",
        "customers",
        [
            {"_id": bson.Int64(1), "name": "alice", "region": "e"},
            {"_id": bson.Int64(2), "name": "bob", "region": "w"},
            {"_id": bson.Int64(3), "name": "carol", "region": "e"},
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
    # JOIN + correlated EXISTS: keep joined rows whose customer has an order.
    cur.execute(
        "SELECT o._id, c.name FROM orders o JOIN customers c ON o.cust = c._id "
        "WHERE EXISTS (SELECT 1 FROM orders o2 WHERE o2.cust = c._id) ORDER BY o._id"
    )
    assert cur.fetchall() == ([10, "alice"], [11, "carol"])
    # GROUP BY + correlated EXISTS: count per region, only customers with orders.
    cur.execute(
        "SELECT c.region, COUNT(*) AS n FROM customers c "
        "WHERE EXISTS (SELECT 1 FROM orders o WHERE o.cust = c._id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    assert cur.fetchall() == (["e", 2],)
    conn.close()


def test_correlated_where_in_join_and_group_via_driver(server):
    # A correlated EXISTS in the WHERE of a query that BOTH joins AND groups,
    # through the real driver — only shipped orders are grouped per region.
    server.storage.insert(
        "db",
        "orders",
        [
            {"_id": bson.Int64(i), "cust": bson.Int64(c), "amt": bson.Int64(a)}
            for i, (c, a) in enumerate([(1, 10), (1, 20), (2, 30), (2, 5)], 1)
        ],
    )
    server.storage.insert(
        "db",
        "customers",
        [{"_id": bson.Int64(i), "region": r} for i, r in [(1, "e"), (2, "w")]],
    )
    server.storage.insert(
        "db",
        "shipments",
        [
            {"_id": bson.Int64(1), "oid": bson.Int64(1)},
            {"_id": bson.Int64(2), "oid": bson.Int64(3)},
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "SELECT c.region, SUM(o.amt) AS s, COUNT(*) AS n "
        "FROM orders o JOIN customers c ON o.cust = c._id "
        "WHERE EXISTS (SELECT 1 FROM shipments sh WHERE sh.oid = o._id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    # Only order 1 (region e, 10) and order 3 (region w, 30) have shipments.
    assert cur.fetchall() == (["e", 10, 1], ["w", 30, 1])
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


def test_create_domain_via_driver(server):
    # CREATE DOMAIN + a domain-typed column enforced + reflected, on the wire.
    conn = connect(server)
    conn.autocommit = True
    cur = conn.cursor()
    cur.execute("CREATE DOMAIN posint AS integer CHECK (VALUE > 0)")
    cur.execute("CREATE DOMAIN nonblank AS text NOT NULL CHECK (length(VALUE) > 0)")
    cur.execute("CREATE TABLE parts (id int primary key, qty posint, label nonblank)")
    cur.execute("INSERT INTO parts VALUES (1, 5, 'bolt')")
    cur.execute("SELECT id, qty, label FROM parts")
    assert cur.fetchall() == ([1, 5, "bolt"],)

    # CHECK violation surfaces as an error over the wire.
    with pytest.raises(pg8000.DatabaseError):
        cur.execute("INSERT INTO parts VALUES (2, -1, 'nut')")
    # NOT NULL domain violation likewise.
    with pytest.raises(pg8000.DatabaseError):
        cur.execute("INSERT INTO parts VALUES (3, 5, NULL)")

    # Reflection: the domain is a pg_type row with typtype 'd', and the column's
    # atttypid points at it.
    cur.execute(
        "SELECT a.attname, ty.typname, ty.typtype "
        "FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "JOIN pg_catalog.pg_type ty ON a.atttypid = ty.oid "
        "WHERE c.relname = 'parts' AND ty.typtype = 'd' ORDER BY a.attname"
    )
    assert cur.fetchall() == (["label", "nonblank", "d"], ["qty", "posint", "d"])
    conn.close()


def test_alter_domain_via_driver(server):
    # ALTER DOMAIN ADD CONSTRAINT (re-validating existing data) + SET NOT NULL,
    # enforced on the wire.
    conn = connect(server)
    conn.autocommit = True
    cur = conn.cursor()
    cur.execute("CREATE DOMAIN pct AS integer CHECK (VALUE >= 0)")
    cur.execute("CREATE TABLE m (id int primary key, v pct)")
    cur.execute("INSERT INTO m VALUES (1, 50)")
    cur.execute("ALTER DOMAIN pct ADD CONSTRAINT le100 CHECK (VALUE <= 100)")
    # New constraint is enforced.
    with pytest.raises(pg8000.DatabaseError):
        cur.execute("INSERT INTO m VALUES (2, 200)")
    cur.execute("INSERT INTO m VALUES (3, 75)")
    cur.execute("SELECT v FROM m ORDER BY id")
    assert cur.fetchall() == ([50], [75])
    conn.close()


def test_aggregate_filter_via_driver(server):
    # agg(...) FILTER (WHERE ...) grouped, on the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE s (id int primary key, dept text, amt int, ok bool)")
    cur.execute("INSERT INTO s VALUES (1,'a',10,true),(2,'a',20,false),(3,'b',30,true)")
    cur.execute(
        "SELECT dept, count(*) FILTER (WHERE ok), sum(amt) FILTER (WHERE ok) "
        "FROM s GROUP BY dept ORDER BY dept"
    )
    assert cur.fetchall() == (["a", 1, 10], ["b", 1, 30])
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


def test_information_schema_constraints_via_driver(server):
    # The information_schema constraint views ORM/migration tooling reflects with.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE items (id bigint primary key, sku text)")
    cur.execute(
        "SELECT tc.constraint_type, kcu.column_name "
        "FROM information_schema.table_constraints tc "
        "JOIN information_schema.key_column_usage kcu "
        "ON tc.constraint_name = kcu.constraint_name "
        "WHERE tc.table_name = 'items'"
    )
    assert cur.fetchall() == (["PRIMARY KEY", "id"],)
    # FK / sequence views resolve (empty) rather than erroring.
    cur.execute("SELECT count(*) FROM information_schema.referential_constraints")
    assert cur.fetchall() == ([0],)
    cur.execute("SELECT count(*) FROM information_schema.sequences")
    assert cur.fetchall() == ([0],)
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
    # A table with no FK reflects empty (no error).
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE users (id bigint primary key, name text)"))
        assert sa.inspect(engine).get_foreign_keys("users") == []
    finally:
        engine.dispose()


def test_sqlalchemy_reflects_foreign_keys(server):
    # A declared FK reflects through SQLAlchemy's inspector: constrained columns,
    # referred table/columns, and ON DELETE / ON UPDATE actions. SQLAlchemy's PG
    # dialect regex-parses pg_get_constraintdef(oid), so this proves that renders.
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE users (id bigint primary key, name text)"))
            conn.execute(
                sa.text(
                    "CREATE TABLE orders (id bigint primary key, "
                    "user_id bigint REFERENCES users(id) ON DELETE CASCADE, total int)"
                )
            )
        fks = sa.inspect(engine).get_foreign_keys("orders")
        assert len(fks) == 1
        fk = fks[0]
        assert fk["name"] == "orders_user_id_fkey"
        assert fk["constrained_columns"] == ["user_id"]
        assert fk["referred_table"] == "users"
        assert fk["referred_columns"] == ["id"]
        assert fk["options"] == {"ondelete": "CASCADE"}
        # Full MetaData reflection resolves the relationship end to end.
        md = sa.MetaData()
        md.reflect(bind=engine)
        targets = [
            (fk.column.table.name, fk.column.name) for fk in md.tables["orders"].foreign_keys
        ]
        assert targets == [("users", "id")]
    finally:
        engine.dispose()


def test_table_level_foreign_key_via_driver(server):
    # Table-level FOREIGN KEY (col) REFERENCES t(col), surfaced in
    # information_schema.referential_constraints through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE users (id bigint primary key, name text)")
    cur.execute(
        "CREATE TABLE orders (id bigint primary key, user_id bigint, total int, "
        "FOREIGN KEY (user_id) REFERENCES users(id) ON UPDATE CASCADE ON DELETE SET NULL)"
    )
    cur.execute(
        "SELECT constraint_name, unique_constraint_name, update_rule, delete_rule "
        "FROM information_schema.referential_constraints"
    )
    assert cur.fetchall() == (["orders_user_id_fkey", "users_pkey", "CASCADE", "SET NULL"],)
    cur.execute(
        "SELECT constraint_type FROM information_schema.table_constraints "
        "WHERE constraint_name = 'orders_user_id_fkey'"
    )
    assert cur.fetchall() == (["FOREIGN KEY"],)
    conn.close()


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


def test_savepoints_via_driver(server):
    # SAVEPOINT / ROLLBACK TO SAVEPOINT / RELEASE through the real driver — the
    # standard `RELEASE SAVEPOINT name` keyword form SQLAlchemy / psycopg emit.
    conn = connect(server)
    conn.autocommit = False
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, n int)")
    conn.commit()
    cur.execute("INSERT INTO t (id, n) VALUES (1, 10)")
    cur.execute("SAVEPOINT sp1")
    cur.execute("INSERT INTO t (id, n) VALUES (2, 20)")
    cur.execute("ROLLBACK TO SAVEPOINT sp1")  # undoes id=2 only
    cur.execute("INSERT INTO t (id, n) VALUES (3, 30)")
    cur.execute("SAVEPOINT sp2")
    cur.execute("INSERT INTO t (id, n) VALUES (4, 40)")
    cur.execute("RELEASE SAVEPOINT sp2")  # keeps id=4
    conn.commit()
    cur.execute("SELECT id FROM t ORDER BY id")
    assert cur.fetchall() == ([1], [3], [4])
    conn.close()


def test_cursors_via_driver(server):
    # DECLARE / FETCH / MOVE / CLOSE server-side cursor through the real driver.
    server.storage.insert(
        "db",
        "t",
        [{"_id": bson.Int64(i), "n": bson.Int64(i * 10)} for i in range(1, 6)],
    )
    conn = connect(server)
    conn.autocommit = False
    cur = conn.cursor()
    cur.execute("DECLARE c CURSOR FOR SELECT _id, n FROM t ORDER BY _id")
    cur.execute("FETCH 2 FROM c")
    assert cur.fetchall() == ([1, 10], [2, 20])
    cur.execute("MOVE 1 FROM c")  # skip _id=3
    cur.execute("FETCH NEXT FROM c")
    assert cur.fetchall() == ([4, 40],)
    cur.execute("FETCH BACKWARD 2 FROM c")
    assert cur.fetchall() == ([3, 30], [2, 20])
    cur.execute("CLOSE c")
    conn.commit()
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


def test_alter_table_via_driver(server):
    # ADD / DROP / RENAME COLUMN + RENAME TO through the real driver, with the
    # data following (dropped field $unset, renamed field $rename).
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, a text, b int)")
    cur.execute("INSERT INTO t (id, a, b) VALUES (1, 'x', 10), (2, 'y', 20)")
    cur.execute("ALTER TABLE t ADD COLUMN c text")
    cur.execute("UPDATE t SET c = 'hi' WHERE id = 1")
    cur.execute("SELECT id, a, b, c FROM t ORDER BY id")
    assert cur.fetchall() == ([1, "x", 10, "hi"], [2, "y", 20, None])
    cur.execute("ALTER TABLE t DROP COLUMN b")
    cur.execute("ALTER TABLE t RENAME COLUMN a TO label")
    cur.execute("SELECT id, label, c FROM t ORDER BY id")
    assert cur.fetchall() == ([1, "x", "hi"], [2, "y", None])
    cur.execute("ALTER TABLE t RENAME TO t2")
    cur.execute("SELECT id, label FROM t2 ORDER BY id")
    assert cur.fetchall() == ([1, "x"], [2, "y"])
    conn.close()


def test_column_default_and_alter_type_via_driver(server):
    # Literal DEFAULT filled on omit + ALTER COLUMN TYPE/SET DEFAULT, via the driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, n int DEFAULT 5, s text)")
    cur.execute("INSERT INTO t (id) VALUES (1)")
    cur.execute("SELECT id, n FROM t WHERE id = 1")
    assert cur.fetchall() == ([1, 5],)
    cur.execute("ALTER TABLE t ALTER COLUMN n SET DEFAULT 42")
    cur.execute("INSERT INTO t (id) VALUES (2)")
    cur.execute("SELECT n FROM t WHERE id = 2")
    assert cur.fetchall() == ([42],)
    cur.execute("ALTER TABLE t ALTER COLUMN n TYPE bigint")
    cur.execute(
        "SELECT data_type FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name = 'n'"
    )
    assert cur.fetchall() == (["bigint"],)
    conn.close()


def test_comment_reflection_via_driver(server):
    # COMMENT ON TABLE / COLUMN reflected through SQLAlchemy's inspector.
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE t (id bigint primary key, n int)"))
            conn.execute(sa.text("COMMENT ON TABLE t IS 'my table'"))
            conn.execute(sa.text("COMMENT ON COLUMN t.n IS 'the n col'"))
        insp = sa.inspect(engine)
        assert insp.get_table_comment("t") == {"text": "my table"}
        cols = {c["name"]: c.get("comment") for c in insp.get_columns("t")}
        assert cols["n"] == "the n col"
    finally:
        engine.dispose()


def test_alter_add_foreign_key_via_driver(server):
    # ALTER TABLE ADD CONSTRAINT ... FOREIGN KEY, reflected via SQLAlchemy.
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE users (id bigint primary key, name text)"))
            conn.execute(sa.text("CREATE TABLE orders (id bigint primary key, user_id bigint)"))
            conn.execute(
                sa.text(
                    "ALTER TABLE orders ADD CONSTRAINT ofk FOREIGN KEY (user_id) "
                    "REFERENCES users(id) ON DELETE CASCADE"
                )
            )
        fks = sa.inspect(engine).get_foreign_keys("orders")
        assert len(fks) == 1
        assert fks[0]["name"] == "ofk"
        assert fks[0]["constrained_columns"] == ["user_id"]
        assert fks[0]["referred_table"] == "users"
        assert fks[0]["options"] == {"ondelete": "CASCADE"}
    finally:
        engine.dispose()


def test_grouping_sets_via_driver(server):
    # ROLLUP over Mongo-written data, through the real driver: per-region
    # subtotals + a grand-total row (region NULL).
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
    cur.execute("SELECT region, SUM(amount) AS s FROM sales GROUP BY ROLLUP(region)")
    got = cur.fetchall()
    assert ["e", 40] in [list(r) for r in got]
    assert ["w", 70] in [list(r) for r in got]
    assert [None, 110] in [list(r) for r in got]
    assert len(got) == 3
    conn.close()


def test_distinct_on_via_driver(server):
    # DISTINCT ON keeps the first row per key in ORDER BY order, through the driver.
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
        "SELECT DISTINCT ON (region) region, amount FROM sales ORDER BY region, amount DESC"
    )
    assert cur.fetchall() == (["e", 30], ["w", 50])
    conn.close()


def test_lateral_join_via_driver(server):
    # A correlated LATERAL subquery (top-1 per outer row) through the real driver.
    server.storage.insert(
        "db", "t", [{"_id": bson.Int64(i), "name": n} for i, n in [(1, "a"), (2, "b")]]
    )
    server.storage.insert(
        "db",
        "u",
        [
            {"_id": bson.Int64(i), "tid": bson.Int64(tid), "val": bson.Int64(v)}
            for i, (tid, v) in enumerate([(1, 10), (1, 40), (2, 30), (2, 5)], 1)
        ],
    )
    conn = connect(server)
    cur = conn.cursor()
    cur.execute(
        "SELECT t.name, s.val FROM t CROSS JOIN LATERAL "
        "(SELECT val FROM u WHERE u.tid = t._id ORDER BY val DESC LIMIT 1) s ORDER BY t.name"
    )
    assert cur.fetchall() == (["a", 40], ["b", 30])
    conn.close()


def test_view_via_driver(server):
    # CREATE VIEW / query a view / DROP VIEW through the real driver — the view
    # expands to its stored SELECT so it reads like the table it stands for.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, n int, grp text)")
    cur.execute("INSERT INTO t (id, n, grp) VALUES (1, 10, 'a'), (2, 20, 'a'), (3, 5, 'b')")
    cur.execute("CREATE VIEW hi AS SELECT id, grp FROM t WHERE n > 8")
    cur.execute("SELECT id, grp FROM hi ORDER BY id")
    assert cur.fetchall() == ([1, "a"], [2, "a"])
    # An aggregate over the view, and a join against a real table.
    cur.execute("SELECT count(*) FROM hi")
    assert cur.fetchall() == ([2],)
    cur.execute("DROP VIEW hi")
    cur.execute("SELECT relname FROM pg_catalog.pg_class WHERE relkind = 'v'")
    assert cur.fetchall() == ()
    conn.close()


def test_array_columns_via_driver(server):
    # Array columns round-trip through the real driver: pg8000 reads the array
    # type OID from RowDescription and decodes the ``{...}`` text into a list.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, tags text[], nums int[])")
    cur.execute("INSERT INTO t (id, tags, nums) VALUES (1, ARRAY['a','b'], ARRAY[1,2,3])")
    cur.execute("INSERT INTO t (id, tags, nums) VALUES (2, '{x,y}', '{7,8}')")
    cur.execute("SELECT id, tags, nums FROM t ORDER BY id")
    assert cur.fetchall() == ([1, ["a", "b"], [1, 2, 3]], [2, ["x", "y"], [7, 8]])
    # = ANY(col) membership, @> containment, array_length.
    cur.execute("SELECT id FROM t WHERE 'a' = ANY(tags)")
    assert cur.fetchall() == ([1],)
    cur.execute("SELECT id FROM t WHERE nums @> ARRAY[7]")
    assert cur.fetchall() == ([2],)
    cur.execute("SELECT id, array_length(tags, 1) FROM t ORDER BY id")
    assert cur.fetchall() == ([1, 2], [2, 2])
    conn.close()


def test_alter_type_add_value_via_driver(server):
    # ALTER TYPE … ADD VALUE extends an enum; ORDER BY on the enum column follows
    # the declared label order (not lexical) — through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')")
    cur.execute("CREATE TABLE t (id bigint primary key, m mood)")
    cur.execute("INSERT INTO t (id, m) VALUES (1, 'happy'), (2, 'sad'), (3, 'ok')")
    # Declared order, not lexical (which would be happy, ok, sad).
    cur.execute("SELECT id, m FROM t ORDER BY m")
    assert cur.fetchall() == ([2, "sad"], [3, "ok"], [1, "happy"])
    cur.execute("ALTER TYPE mood ADD VALUE 'meh' AFTER 'ok'")
    cur.execute("INSERT INTO t (id, m) VALUES (4, 'meh')")
    cur.execute("SELECT m FROM t ORDER BY m")
    assert cur.fetchall() == (["sad"], ["ok"], ["meh"], ["happy"])
    conn.close()


def test_array_subscript_via_driver(server):
    # arr[i] element access and arr[lo:hi] slicing through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, tags text[], nums int[])")
    cur.execute("INSERT INTO t (id, tags, nums) VALUES (1, ARRAY['a','b','c'], ARRAY[10,20,30])")
    cur.execute("SELECT tags[1], tags[3], nums[2] FROM t WHERE id = 1")
    assert cur.fetchall() == (["a", "c", 20],)
    cur.execute("SELECT tags[2:3] FROM t WHERE id = 1")
    assert cur.fetchall() == ([["b", "c"]],)
    cur.execute("SELECT id FROM t WHERE tags[1] = 'a'")
    assert cur.fetchall() == ([1],)
    conn.close()


def test_composite_primary_key_via_driver(server):
    # A composite PK round-trips through the real driver, enforces uniqueness on
    # the (a, b) pair, and reflects both columns via SQLAlchemy's inspector.
    sa = pytest.importorskip("sqlalchemy")
    conn = connect(server)
    conn.autocommit = True  # commit DDL so a separate SQLAlchemy connection sees it
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (a bigint, b text, n int, PRIMARY KEY (a, b))")
    cur.execute("INSERT INTO t (a, b, n) VALUES (1, 'x', 10), (1, 'y', 20), (2, 'z', 30)")
    cur.execute("SELECT a, b, n FROM t ORDER BY a, b")
    assert cur.fetchall() == ([1, "x", 10], [1, "y", 20], [2, "z", 30])
    cur.execute("SELECT n FROM t WHERE a = 1 AND b = 'y'")
    assert cur.fetchall() == ([20],)

    # Duplicate composite key is rejected.
    try:
        cur.execute("INSERT INTO t (a, b, n) VALUES (1, 'x', 99)")
        raise AssertionError("expected a unique-violation error")
    except Exception as exc:  # pg8000 surfaces the SQLSTATE in the message
        assert "23505" in str(exc) or "duplicate" in str(exc).lower()
    conn.close()

    # SQLAlchemy reflects both PK columns, in order.
    host, port = server.address
    engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
    insp = sa.inspect(engine)
    pk = insp.get_pk_constraint("t")
    assert pk["constrained_columns"] == ["a", "b"]
    engine.dispose()


def test_array_functions_via_driver(server):
    # Array manipulation functions round-trip through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, tags text[], nums int[])")
    cur.execute("INSERT INTO t (id, tags, nums) VALUES (1, ARRAY['a','b','c'], ARRAY[10,20,30])")
    cur.execute("SELECT array_append(tags, 'd') FROM t")
    assert cur.fetchall() == ([["a", "b", "c", "d"]],)
    cur.execute("SELECT array_cat(nums, ARRAY[40,50]) FROM t")
    assert cur.fetchall() == ([[10, 20, 30, 40, 50]],)
    cur.execute("SELECT array_position(tags, 'b'), array_to_string(tags, '-') FROM t")
    assert cur.fetchall() == ([2, "a-b-c"],)
    conn.close()


def test_unnest_in_from_via_driver(server):
    # unnest(array_col) as a FROM table-function source, through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, tags text[])")
    cur.execute("INSERT INTO t (id, tags) VALUES (1, ARRAY['a','b']), (2, ARRAY['c'])")
    cur.execute("SELECT id, tag FROM t, unnest(t.tags) AS tag ORDER BY id, tag")
    assert cur.fetchall() == ([1, "a"], [1, "b"], [2, "c"])
    cur.execute("SELECT id, count(*) FROM t, unnest(t.tags) AS tag GROUP BY id ORDER BY id")
    assert cur.fetchall() == ([1, 2], [2, 1])
    conn.close()


def test_jsonb_functions_via_driver(server):
    # jsonb_set / jsonb_insert / #- through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, data jsonb)")
    cur.execute("""INSERT INTO t (id, data) VALUES (1, '{"a": 1, "b": {"c": 2}}')""")
    cur.execute("SELECT jsonb_set(data, '{a}', '5') FROM t")
    assert cur.fetchall() == ([{"a": 5, "b": {"c": 2}}],)
    cur.execute("SELECT jsonb_insert(data, '{d}', '9') FROM t")
    assert cur.fetchall() == ([{"a": 1, "b": {"c": 2}, "d": 9}],)
    cur.execute("SELECT data #- '{b,c}' FROM t")
    assert cur.fetchall() == ([{"a": 1, "b": {}}],)
    conn.close()


def test_string_agg_and_bool_aggregates_via_driver(server):
    # string_agg + bool_and / bool_or through the real driver.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, grp text, name text, active boolean)")
    cur.execute(
        "INSERT INTO t (id, grp, name, active) VALUES "
        "(1,'a','x',true),(2,'a','y',false),(3,'b','z',true)"
    )
    cur.execute("SELECT grp, string_agg(name, ',') FROM t GROUP BY grp ORDER BY grp")
    assert cur.fetchall() == (["a", "x,y"], ["b", "z"])
    cur.execute("SELECT grp, bool_and(active), bool_or(active) FROM t GROUP BY grp ORDER BY grp")
    assert cur.fetchall() == (["a", False, True], ["b", True, True])
    conn.close()


def test_ssl_request_declined_without_tls(server):
    # Sanity: a raw SSLRequest is declined when TLS isn't configured.
    host, port = server.address
    s = socket.create_connection((host, port), timeout=5)
    s.sendall(struct.pack("!ii", 8, pgwire.SSL_REQUEST_CODE))
    assert s.recv(1) == b"N"
    s.close()
