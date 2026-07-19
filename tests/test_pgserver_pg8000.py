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
from secantus.storage import Storage

pg8000 = pytest.importorskip("pg8000.dbapi")


@pytest.fixture
def server(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        st.close()


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


def test_ordered_set_aggregate_via_driver(server):
    # percentile_cont / mode WITHIN GROUP, grouped, on the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE m (id int primary key, g text, v int)")
    cur.execute("INSERT INTO m VALUES (1,'a',1),(2,'a',2),(3,'a',3),(4,'b',9),(5,'b',9)")
    cur.execute(
        "SELECT g, percentile_cont(0.5) WITHIN GROUP (ORDER BY v), "
        "mode() WITHIN GROUP (ORDER BY v) FROM m GROUP BY g ORDER BY g"
    )
    assert cur.fetchall() == (["a", 2.0, 1], ["b", 9.0, 9])
    conn.close()


def test_agg_order_by_via_driver(server):
    # array_agg / string_agg with an in-call ORDER BY, on the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE e (id int primary key, g text, name text, ord int)")
    cur.execute("INSERT INTO e VALUES (1,'a','c',3),(2,'a','a',1),(3,'a','b',2)")
    cur.execute(
        "SELECT g, array_agg(name ORDER BY ord), string_agg(name, ',' ORDER BY name DESC) "
        "FROM e GROUP BY g"
    )
    assert cur.fetchall() == (["a", ["a", "b", "c"], "c,b,a"],)
    conn.close()


def test_regex_string_funcs_via_driver(server):
    # regexp_replace / split_part / regexp_count on the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id int primary key, s text)")
    cur.execute("INSERT INTO t VALUES (1, 'a1b2c3')")
    cur.execute(
        "SELECT regexp_replace(s, '[0-9]', '#', 'g'), split_part(s, 'b', 1), "
        "regexp_count(s, '[0-9]') FROM t"
    )
    assert cur.fetchall() == (["a#b#c#", "a1", 3],)
    conn.close()


def test_math_funcs_via_driver(server):
    # trunc / sqrt / sign / log10 / factorial / gcd typed correctly on the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE m (id int primary key, x double precision, n int)")
    cur.execute("INSERT INTO m VALUES (1, 9.87, 5)")
    cur.execute(
        "SELECT trunc(x), sqrt(16.0), sign(x), log10(1000.0), factorial(n), gcd(n, 18) FROM m"
    )
    row = cur.fetchone()
    assert row[0] == 9  # trunc(9.87) -> 9
    assert row[1] == pytest.approx(4.0)  # sqrt(16)
    assert row[2] == 1  # sign(9.87)
    assert row[3] == pytest.approx(3.0)  # log10(1000)
    assert row[4] == 120  # factorial(5)
    assert row[5] == 1  # gcd(5, 18)
    conn.close()


def test_composite_type_via_driver(server):
    # CREATE TYPE composite: ROW insert, (col).field read (typed), and the whole
    # composite rendered as a Postgres record literal over the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TYPE addr AS (street text, zip int)")
    cur.execute("CREATE TABLE people (id int primary key, home addr)")
    cur.execute("INSERT INTO people VALUES (1, ROW('Main St', 90210))")
    cur.execute("SELECT (home).street, (home).zip FROM people")
    assert cur.fetchone() == ["Main St", 90210]  # zip is a real int, not '90210'
    cur.execute("SELECT home FROM people")
    # The column reports the composite's MINTED oid (like real PG); pg8000
    # doesn't know user oids, so the value arrives as the record text literal.
    assert cur.fetchone() == ['("Main St",90210)']
    conn.close()


def test_datetime_funcs_via_driver(server):
    # extract / date_trunc / to_char / interval arithmetic typed over the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE ev (id int primary key, at timestamptz)")
    cur.execute("INSERT INTO ev VALUES (1, '2021-03-15T14:30:45+00:00')")
    cur.execute(
        "SELECT extract(year FROM at), date_trunc('month', at), "
        "to_char(at, 'YYYY-MM-DD'), at + interval '1 day' FROM ev"
    )
    row = cur.fetchone()
    assert row[0] == 2021  # extract -> numeric, decoded as a number
    assert row[1] == _dt.datetime(2021, 3, 1, tzinfo=_dt.timezone.utc)  # date_trunc
    assert row[2] == "2021-03-15"  # to_char -> text
    assert row[3] == _dt.datetime(2021, 3, 16, 14, 30, 45, tzinfo=_dt.timezone.utc)
    conn.close()


def test_string_funcs2_via_driver(server):
    # lpad / left / ascii / position / overlay typed correctly on the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id int primary key, s text)")
    cur.execute("INSERT INTO t VALUES (1, 'hello')")
    cur.execute(
        "SELECT lpad(s, 8, '*'), left(s, 3), ascii(s), position('l' IN s), "
        "overlay(s placing 'XY' from 2 for 3) FROM t"
    )
    row = cur.fetchone()
    assert row[0] == "***hello"
    assert row[1] == "hel"
    assert row[2] == 104  # ascii -> real int
    assert row[3] == 3  # position -> real int
    assert row[4] == "hXYo"
    conn.close()


def test_composite_where_update_via_driver(server):
    # (col).field in WHERE and UPDATE SET col.field / col = ROW(...) over the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TYPE addr AS (street text, zip int)")
    cur.execute("CREATE TABLE people (id int primary key, home addr)")
    cur.execute("INSERT INTO people VALUES (1, ROW('Main St', 90210))")
    cur.execute("SELECT id FROM people WHERE (home).zip = 90210")
    assert cur.fetchone() == [1]
    cur.execute("UPDATE people SET home.zip = 55555 WHERE id = 1")
    cur.execute("SELECT (home).zip FROM people WHERE id = 1")
    assert cur.fetchone() == [55555]
    cur.execute("UPDATE people SET home = ROW('New Rd', 12345) WHERE id = 1")
    cur.execute("SELECT (home).street FROM people WHERE id = 1")
    assert cur.fetchone() == ["New Rd"]
    conn.close()


def test_jsonpath_via_driver(server):
    # jsonb_path_query / _exists and the @? / @@ operators typed over the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id int primary key, data jsonb)")
    cur.execute('INSERT INTO t VALUES (1, \'{"a": {"b": 5}, "items": [{"x": 1}, {"x": 2}]}\')')
    cur.execute("SELECT jsonb_path_query(data, '$.a.b') FROM t")
    assert cur.fetchone() == [5]
    cur.execute(
        "SELECT jsonb_path_exists(data, '$.a.b'), data @? '$.items[*] ? (@.x == 2)', "
        "data @@ '$.a.b == 5' FROM t"
    )
    assert cur.fetchone() == [True, True, True]  # all real booleans
    conn.close()


def test_stat_bit_agg_via_driver(server):
    # stddev / variance / bit_and|or|xor typed correctly on the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id int primary key, x float8, n int)")
    cur.execute("INSERT INTO t VALUES (1, 2.0, 6), (2, 4.0, 3), (3, 6.0, 5)")
    cur.execute("SELECT stddev_pop(x), var_pop(x), bit_and(n), bit_or(n), bit_xor(n) FROM t")
    row = cur.fetchone()
    assert row[0] == pytest.approx(1.632993, abs=1e-5)  # stddev_pop
    assert float(row[1]) == pytest.approx(2.666667, abs=1e-5)  # var_pop -> numeric
    assert row[2] == (6 & 3 & 5)  # bit_and -> real int
    assert row[3] == (6 | 3 | 5)
    assert row[4] == (6 ^ 3 ^ 5)
    conn.close()


def test_range_types_via_driver(server):
    # Range storage, constructors, text literals, accessors and the @> / <@ / &&
    # operators over the real pg8000 wire (which parses the range text form).
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE r (id int primary key, span int4range)")
    cur.execute("INSERT INTO r VALUES (1, int4range(1,10))")
    cur.execute("INSERT INTO r VALUES (2, '[5,20)')")
    cur.execute("INSERT INTO r VALUES (3, int4range(100,200))")

    # pg8000 parses the range OID into a Range object rendering as [lo,hi).
    cur.execute("SELECT span FROM r WHERE id = 1")
    assert str(cur.fetchone()[0]) == "[1,10)"

    # Accessors: lower/upper come back as ints, isempty as a real bool.
    cur.execute("SELECT lower(span), upper(span), isempty(span) FROM r WHERE id = 2")
    lo, hi, empty = cur.fetchone()
    assert (lo, hi, empty) == (5, 20, False)

    # Containment / overlap in WHERE.
    cur.execute("SELECT id FROM r WHERE span @> 7 ORDER BY id")
    assert [row[0] for row in cur.fetchall()] == [1, 2]
    cur.execute("SELECT id FROM r WHERE span && int4range(15,150) ORDER BY id")
    assert [row[0] for row in cur.fetchall()] == [2, 3]

    # Containment as a boolean projection.
    cur.execute("SELECT span @> 7 FROM r WHERE id = 3")
    assert cur.fetchone()[0] is False
    conn.close()


def test_jsonb_agg_and_builders_via_driver(server):
    # jsonb_agg / jsonb_object_agg aggregates and to_jsonb over the real wire; the
    # driver decodes the json column back into Python containers.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE j (id int primary key, g int, k text, v int)")
    cur.execute("INSERT INTO j VALUES (1,1,'a',10),(2,1,'b',20),(3,2,'c',30)")

    cur.execute("SELECT jsonb_agg(v ORDER BY v) AS a FROM j")
    assert cur.fetchone()[0] == [10, 20, 30]

    cur.execute("SELECT jsonb_object_agg(k, v) AS o FROM j")
    assert cur.fetchone()[0] == {"a": 10, "b": 20, "c": 30}

    cur.execute("SELECT g, jsonb_object_agg(k, v) AS o FROM j GROUP BY g ORDER BY g")
    assert [tuple(r) for r in cur.fetchall()] == [(1, {"a": 10, "b": 20}), (2, {"c": 30})]

    cur.execute("SELECT to_jsonb(v) AS j FROM j WHERE id = 1")
    assert cur.fetchone()[0] == 10
    conn.close()


def test_nested_composite_via_driver(server):
    # A composite type whose field is itself a composite: nested ROW insert, single-
    # and deep-level field access, and nested UPDATE over the real wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TYPE addr AS (street text, zip int)")
    cur.execute("CREATE TYPE person AS (name text, home addr)")
    cur.execute("CREATE TABLE t (id int primary key, p person)")
    cur.execute("INSERT INTO t VALUES (1, ROW('Bob', ROW('Main St', 90210)))")

    cur.execute("SELECT (p).name FROM t WHERE id = 1")
    assert cur.fetchone()[0] == "Bob"

    # A single-level composite field decodes into a record tuple.
    cur.execute("SELECT (p).home FROM t WHERE id = 1")
    assert tuple(cur.fetchone()[0]) == ("Main St", "90210")

    cur.execute("SELECT ((p).home).street, ((p).home).zip FROM t WHERE id = 1")
    assert tuple(cur.fetchone()) == ("Main St", 90210)

    cur.execute("UPDATE t SET p.home = ROW('Elm St', 11111) WHERE id = 1")
    cur.execute("SELECT ((p).home).zip FROM t WHERE id = 1")
    assert cur.fetchone()[0] == 11111
    conn.close()


def test_range_algebra_and_range_agg_via_driver(server):
    # Range operators, range_merge, and the range_agg -> multirange aggregate over
    # the real wire (pg8000 parses range / multirange text into its own objects).
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id int primary key, g int, r int4range)")
    cur.execute(
        "INSERT INTO t VALUES (1,1,int4range(1,5)),(2,1,int4range(3,8)),"
        "(3,1,int4range(20,25)),(4,2,int4range(1,2))"
    )

    cur.execute("SELECT int4range(1,10) * int4range(5,20)")
    assert str(cur.fetchone()[0]) == "[5,10)"
    cur.execute("SELECT int4range(1,10) + int4range(5,20)")
    assert str(cur.fetchone()[0]) == "[1,20)"
    cur.execute("SELECT range_merge(int4range(1,5), int4range(10,15))")
    assert str(cur.fetchone()[0]) == "[1,15)"
    cur.execute("SELECT int4range(1,5) -|- int4range(5,9)")
    assert cur.fetchone()[0] is True

    # range_agg coalesces each group's ranges into a multirange (pg8000 decodes it
    # into a list of Range objects).
    cur.execute("SELECT g, range_agg(r) AS m FROM t GROUP BY g ORDER BY g")
    rows = cur.fetchall()
    assert [r[0] for r in rows] == [1, 2]
    assert [[str(m) for m in r[1]] for r in rows] == [["[1,8)", "[20,25)"], ["[1,2)"]]
    conn.close()


def test_full_text_search_via_driver(server):
    # to_tsvector / to_tsquery / @@ match / ts_rank ordering over the real wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE docs (id int primary key, body tsvector)")
    cur.execute("INSERT INTO docs VALUES (1, to_tsvector('the quick brown fox'))")
    cur.execute("INSERT INTO docs VALUES (2, to_tsvector('a lazy dog sleeps'))")
    cur.execute("INSERT INTO docs VALUES (3, to_tsvector('the quick dog runs quick'))")

    cur.execute("SELECT to_tsvector('a cat sat') @@ to_tsquery('cat')")
    assert cur.fetchone()[0] is True

    cur.execute("SELECT id FROM docs WHERE body @@ to_tsquery('quick & dog') ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [3]

    # ts_rank orders the higher-frequency match first.
    cur.execute(
        "SELECT id FROM docs WHERE body @@ to_tsquery('quick') "
        "ORDER BY ts_rank(body, to_tsquery('quick')) DESC"
    )
    assert [r[0] for r in cur.fetchall()] == [3, 1]

    # The tsvector column renders as the Postgres text form 'lexeme':pos.
    cur.execute("SELECT body FROM docs WHERE id = 1")
    assert cur.fetchone()[0] == "'brown':3 'fox':4 'quick':2"
    conn.close()


def test_fts_followups_via_driver(server):
    # prefix (cat:*), phrase (<->), phraseto_tsquery, and ts_headline over the wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE docs (id int primary key, body tsvector)")
    cur.execute("INSERT INTO docs VALUES (1, to_tsvector('the quick brown fox'))")
    cur.execute("INSERT INTO docs VALUES (2, to_tsvector('brown quick fox'))")
    cur.execute("INSERT INTO docs VALUES (3, to_tsvector('the categories are nice'))")

    # phrase: only doc 1 has 'quick' immediately followed by 'brown'.
    cur.execute("SELECT id FROM docs WHERE body @@ to_tsquery('quick <-> brown') ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [1]

    # prefix: only doc 3 has a lexeme starting with 'cat'.
    cur.execute("SELECT id FROM docs WHERE body @@ to_tsquery('cat:*') ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [3]

    # phraseto_tsquery keeps word order.
    cur.execute("SELECT id FROM docs WHERE body @@ phraseto_tsquery('quick brown') ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [1]

    # ts_headline highlights matched terms.
    cur.execute("SELECT ts_headline('The quick brown fox', to_tsquery('quick | fox'))")
    assert cur.fetchone()[0] == "The <b>quick</b> brown <b>fox</b>"
    conn.close()


def test_network_types_via_driver(server):
    # inet / cidr / macaddr columns, the << / >> / && containment/overlap
    # operators, and the host / masklen / network accessors over the real wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE hosts (id int primary key, addr inet, mac macaddr)")
    cur.execute("INSERT INTO hosts VALUES (1, '10.1.2.3/32', '08:00:2b:01:02:03')")
    cur.execute("INSERT INTO hosts VALUES (2, '192.168.5.6', '08-00-2b-aa-bb-cc')")
    cur.execute("INSERT INTO hosts VALUES (3, '172.16.0.1/16', 'aabb.ccdd.eeff')")

    # An inet with a full-host mask renders without the /32; the macaddr renders
    # in canonical colon form. (pg8000 parses the inet OID into an ipaddress
    # object, so compare via str().)
    cur.execute("SELECT addr, mac FROM hosts WHERE id = 1")
    addr, mac = cur.fetchone()
    assert str(addr) == "10.1.2.3"
    assert str(mac) == "08:00:2b:01:02:03"

    # Subnet containment in WHERE routes through the per-row scalar path.
    cur.execute("SELECT id FROM hosts WHERE addr << '10.0.0.0/8'::cidr ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [1]

    # Overlap operator.
    cur.execute("SELECT id FROM hosts WHERE addr && '172.16.0.0/12'::cidr ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [3]

    # host / masklen / network accessors.
    cur.execute("SELECT host(addr), masklen(addr), network(addr) FROM hosts WHERE id = 3")
    host_v, mask_v, net_v = cur.fetchone()
    assert str(host_v) == "172.16.0.1"
    assert mask_v == 16
    assert str(net_v) == "172.16.0.0/16"
    conn.close()


def test_bit_string_types_via_driver(server):
    # bit(n) / varbit columns, B'…' literals, the &/|/#/~/<</>> operators, and
    # length / get_bit accessors over the real wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id int primary key, flags bit(8), mask varbit)")
    cur.execute("INSERT INTO t VALUES (1, '10101010', '111')")
    cur.execute("INSERT INTO t VALUES (2, '00001111', '0')")

    # A bit column renders as its '0'/'1' string.
    cur.execute("SELECT flags, mask FROM t WHERE id = 1")
    assert cur.fetchone() == ["10101010", "111"]

    # Bitwise algebra.
    cur.execute("SELECT b'1010' & b'0110', b'1010' | b'0110', ~ b'1010'")
    assert cur.fetchone() == ["0010", "1110", "0101"]

    # int -> bit and bit -> int casts.
    cur.execute("SELECT 10::bit(8), b'1010'::int")
    assert cur.fetchone() == ["00001010", 10]

    # A masked WHERE routes through the per-row scalar path.
    cur.execute("SELECT id FROM t WHERE flags & b'00001111' = b'00001010' ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [1]

    # length / get_bit.
    cur.execute("SELECT length(flags), get_bit(flags, 0) FROM t WHERE id = 1")
    assert cur.fetchone() == [8, 1]
    conn.close()


def test_interval_type_via_driver(server):
    # interval columns, literals, arithmetic, and functions over the real wire
    # (pg8000 parses the interval OID into its own Interval/timedelta object, so
    # compare via str()).
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id int primary key, dur interval)")
    cur.execute("INSERT INTO t VALUES (1, interval '2 hours 30 minutes')")

    cur.execute("SELECT interval '1 year 2 months 3 days'")
    # pg8000 parses the interval OID into a PGInterval ("1 years 2 months 3 days").
    got_year = str(cur.fetchone()[0])
    assert "1 year" in got_year and "2 month" in got_year and "3 day" in got_year

    # interval + interval.
    cur.execute("SELECT interval '1 day' + interval '2 hours'")
    got = cur.fetchone()[0]
    assert "1 day" in str(got) and ("02:00:00" in str(got) or "2:00:00" in str(got))

    # timestamp - timestamp -> interval (74 days).
    cur.execute("SELECT timestamp '2020-03-15' - timestamp '2020-01-01'")
    assert "74 days" in str(cur.fetchone()[0])

    # a stored interval column round-trips.
    cur.execute("SELECT dur FROM t WHERE id = 1")
    assert "2:30:00" in str(cur.fetchone()[0])
    conn.close()


def test_uuid_type_via_driver(server):
    # uuid columns, gen_random_uuid, casts, and equality over the real wire
    # (pg8000 parses the uuid OID into a Python uuid.UUID, so compare via str()).
    import uuid as _uuid

    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE people (id uuid primary key, name text)")
    sample = "550e8400-e29b-41d4-a716-446655440000"
    cur.execute(f"INSERT INTO people VALUES ('{sample}', 'alice')")
    cur.execute("INSERT INTO people VALUES (gen_random_uuid(), 'bob')")

    # The uuid column round-trips (pg8000 gives a uuid.UUID).
    cur.execute("SELECT id FROM people WHERE name = 'alice'")
    got = cur.fetchone()[0]
    assert str(got) == sample

    # gen_random_uuid produces a valid v4 uuid.
    cur.execute("SELECT gen_random_uuid()")
    v = cur.fetchone()[0]
    assert _uuid.UUID(str(v)).version == 4

    # equality in WHERE lowers to a Mongo filter.
    cur.execute(f"SELECT name FROM people WHERE id = '{sample}'")
    assert cur.fetchone()[0] == "alice"
    conn.close()


def test_date_time_types_via_driver(server):
    # date / time / timetz columns and literals over the real wire (pg8000 parses
    # the date/time OIDs into Python date/time objects, so compare via str()).
    import datetime as _dt

    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE ev (id int primary key, d date, t time, ttz timetz)")
    cur.execute("INSERT INTO ev VALUES (1, '2020-06-15', '09:00', '09:00:00+02')")

    cur.execute("SELECT d, t FROM ev WHERE id = 1")
    d, t = cur.fetchone()
    assert isinstance(d, _dt.date) and str(d) == "2020-06-15"
    assert str(t) == "09:00:00"

    # date - date -> integer days.
    cur.execute("SELECT date '2020-03-15' - date '2020-01-01'")
    assert cur.fetchone()[0] == 74

    # date + int -> date.
    cur.execute("SELECT date '2020-01-31' + 1")
    assert str(cur.fetchone()[0]) == "2020-02-01"

    # equality lowers to a Mongo filter.
    cur.execute("SELECT id FROM ev WHERE d = '2020-06-15'")
    assert cur.fetchone()[0] == 1
    conn.close()


def test_money_and_to_char_via_driver(server):
    # money columns render as $-formatted currency; numeric to_char formats over
    # the real wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE items (id int primary key, price money)")
    cur.execute("INSERT INTO items VALUES (1, '19.99'), (2, '$1,250.00')")

    cur.execute("SELECT price FROM items WHERE id = 2")
    assert cur.fetchone()[0] == "$1,250.00"

    # money arithmetic keeps the currency rendering.
    cur.execute("SELECT price + price FROM items WHERE id = 1")
    assert cur.fetchone()[0] == "$39.98"

    # numeric to_char.
    cur.execute("SELECT to_char(1234.5, 'FM$9,999.99')")
    assert cur.fetchone()[0] == "$1,234.50"

    # equality lowers to a Mongo filter.
    cur.execute("SELECT id FROM items WHERE price = '19.99'")
    assert cur.fetchone()[0] == 1
    conn.close()


def test_geometric_types_via_driver(server):
    # point / polygon columns, the <-> distance and @> containment operators, and
    # canonical rendering over the real wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE shapes (id int primary key, loc point, area polygon)")
    cur.execute("INSERT INTO shapes VALUES (1, '(1,1)', '((0,0),(4,0),(4,4),(0,4))')")
    cur.execute("INSERT INTO shapes VALUES (2, '(9,9)', '((5,5),(6,5),(6,6),(5,6))')")

    # A point column renders in canonical text; pg8000 parses the point OID into a
    # tuple of floats.
    cur.execute("SELECT loc FROM shapes WHERE id = 1")
    got = cur.fetchone()[0]
    assert tuple(float(x) for x in got) == (1.0, 1.0)

    # <-> distance.
    cur.execute("SELECT point '(0,0)' <-> point '(3,4)'")
    assert cur.fetchone()[0] == 5

    # @> containment routes through the per-row scalar path.
    cur.execute("SELECT id FROM shapes WHERE area @> point '(2,2)' ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [1]

    # distance ordering.
    cur.execute("SELECT id FROM shapes ORDER BY loc <-> point '(0,0)'")
    assert [r[0] for r in cur.fetchall()] == [1, 2]
    conn.close()


def test_bytea_type_via_driver(server):
    # bytea columns, encode/decode, and get_byte over the real wire. pg8000 maps
    # the bytea OID to Python bytes.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE blobs (id int primary key, data bytea)")
    cur.execute("INSERT INTO blobs VALUES (1, '\\xcafe')")

    cur.execute("SELECT data FROM blobs WHERE id = 1")
    assert cur.fetchone()[0] == b"\xca\xfe"

    # encode(bytea, 'hex') -> text.
    cur.execute("SELECT encode(data, 'hex') FROM blobs WHERE id = 1")
    assert cur.fetchone()[0] == "cafe"

    # decode(text, 'hex') -> bytea.
    cur.execute("SELECT decode('deadbeef', 'hex')")
    assert cur.fetchone()[0] == b"\xde\xad\xbe\xef"

    # get_byte / length.
    cur.execute("SELECT get_byte(data, 1), length(data) FROM blobs WHERE id = 1")
    assert cur.fetchone() == [0xFE, 2]
    conn.close()


def test_hstore_type_via_driver(server):
    # hstore columns, the -> lookup / @> containment / ? key-exists operators over
    # the real wire (rendered as canonical hstore text).
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE items (id int primary key, attrs hstore)")
    cur.execute("INSERT INTO items VALUES (1, 'color=>red, size=>big')")
    cur.execute("INSERT INTO items VALUES (2, 'color=>blue, size=>small')")

    # -> lookup returns text.
    cur.execute("SELECT attrs -> 'color' FROM items WHERE id = 1")
    assert cur.fetchone()[0] == "red"

    # @> containment (per-row) and ? key-exists.
    cur.execute("SELECT id FROM items WHERE attrs @> 'color=>blue'")
    assert [r[0] for r in cur.fetchall()] == [2]
    cur.execute("SELECT id FROM items WHERE attrs ? 'size' ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [1, 2]

    # akeys returns a text array.
    cur.execute("SELECT akeys('a=>1,b=>2'::hstore)")
    assert sorted(cur.fetchone()[0]) == ["a", "b"]
    conn.close()


def test_citext_type_via_driver(server):
    # citext columns compare / sort case-insensitively while preserving case for
    # display, over the real wire (sent as text).
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE u (id int primary key, name citext)")
    cur.execute("INSERT INTO u VALUES (1, 'Alice')")
    cur.execute("INSERT INTO u VALUES (2, 'BOB')")
    cur.execute("INSERT INTO u VALUES (3, 'carol')")

    # case preserved on read.
    cur.execute("SELECT name FROM u WHERE id = 1")
    assert cur.fetchone()[0] == "Alice"

    # case-insensitive equality and IN.
    cur.execute("SELECT id FROM u WHERE name = 'alice'")
    assert [r[0] for r in cur.fetchall()] == [1]
    cur.execute("SELECT id FROM u WHERE name IN ('ALICE', 'bob') ORDER BY id")
    assert [r[0] for r in cur.fetchall()] == [1, 2]

    # case-insensitive ORDER BY (a, b, c regardless of stored case).
    cur.execute("SELECT id FROM u ORDER BY name")
    assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
    conn.close()


def test_xml_type_via_driver(server):
    # xml columns, xmlelement / xpath / xml_is_well_formed over the real wire.
    conn = connect(server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE docs (id int primary key, body xml)")
    cur.execute("INSERT INTO docs VALUES (1, '<doc><title>Hi</title></doc>')")

    # xml column renders as its text.
    cur.execute("SELECT body FROM docs WHERE id = 1")
    assert cur.fetchone()[0] == "<doc><title>Hi</title></doc>"

    # xmlelement constructor.
    cur.execute("SELECT xmlelement(name foo, 'bar')")
    assert cur.fetchone()[0] == "<foo>bar</foo>"

    # xml_is_well_formed.
    cur.execute("SELECT xml_is_well_formed('<a/>'), xml_is_well_formed('<a>')")
    assert cur.fetchone() == [True, False]

    # xpath returns a text array.
    cur.execute("SELECT xpath('/doc/title/text()', body) FROM docs WHERE id = 1")
    assert cur.fetchone()[0] == ["Hi"]
    conn.close()


def test_listen_notify_via_driver(server):
    # A NOTIFY on one connection is delivered to a LISTENer on another connection,
    # over the real wire (NotificationResponse 'A'). Delivery is inline with the
    # listener's query cycle: it arrives before the ReadyForQuery of its next query.
    listener = connect(server)
    notifier = connect(server)
    try:
        lcur = listener.cursor()
        lcur.execute("LISTEN chan")
        listener.commit()

        ncur = notifier.cursor()
        ncur.execute("NOTIFY chan, 'payload-1'")
        notifier.commit()  # commit flushes the notification into the listener's queue

        # The listener's next query carries the pending notification back with it.
        lcur.execute("SELECT 1")
        lcur.fetchall()

        assert len(listener.notifications) >= 1
        _pid, channel, payload = listener.notifications.popleft()
        assert channel == "chan"
        assert payload == "payload-1"
    finally:
        listener.close()
        notifier.close()


def test_fts_ranking_via_driver(server):
    # websearch_to_tsquery + ranked search (ORDER BY rank alias) over the wire (#126).
    conn = connect(server)
    try:
        cur = conn.cursor()
        cur.execute("CREATE TABLE doc (id int, body text)")
        cur.execute(
            "INSERT INTO doc VALUES (1, 'the quick brown fox'), (2, 'quick quick fox runs')"
        )
        conn.commit()

        cur.execute(
            "SELECT id FROM doc WHERE to_tsvector(body) @@ websearch_to_tsquery('quick -runs')"
        )
        assert [r[0] for r in cur.fetchall()] == [1]

        cur.execute(
            "SELECT id, ts_rank(to_tsvector(body), to_tsquery('quick')) AS rank "
            "FROM doc WHERE to_tsvector(body) @@ to_tsquery('quick') ORDER BY rank DESC"
        )
        assert [r[0] for r in cur.fetchall()] == [2, 1]
    finally:
        conn.close()


def test_generate_series_via_driver(server):
    # generate_series + FROM-clause SRF over the real wire (#125).
    conn = connect(server)
    try:
        cur = conn.cursor()
        cur.execute("SELECT * FROM generate_series(1, 5)")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3, 4, 5]

        cur.execute("SELECT n FROM generate_series(1, 6, 2) AS g(n) ORDER BY n DESC")
        assert [r[0] for r in cur.fetchall()] == [5, 3, 1]

        cur.execute("SELECT * FROM generate_series(10, 30, 10) WITH ORDINALITY")
        assert [d[0] for d in cur.description] == ["generate_series", "ordinality"]
        assert cur.fetchall() == ([10, 1], [20, 2], [30, 3])
    finally:
        conn.close()


def test_create_function_via_driver(server):
    # CREATE FUNCTION ... LANGUAGE sql + call over the real wire (#124).
    conn = connect(server)
    try:
        cur = conn.cursor()
        cur.execute(
            "CREATE FUNCTION add(a int, b int) RETURNS int AS $$ SELECT a + b $$ LANGUAGE sql"
        )
        cur.execute("SELECT add(40, 2)")
        assert cur.fetchall() == ([42],)

        cur.execute("CREATE TABLE t (id int, v int)")
        cur.execute("INSERT INTO t VALUES (1, 5), (2, 10)")
        conn.commit()
        cur.execute("SELECT id, add(v, 100) FROM t ORDER BY id")
        assert cur.fetchall() == ([1, 105], [2, 110])

        cur.execute("DROP FUNCTION add(int, int)")
        with pytest.raises(pg8000.DatabaseError):
            cur.execute("SELECT add(1, 2)")
    finally:
        conn.close()


def test_array_operators_via_driver(server):
    # Array @> / <@ / && and a uuid[] roundtrip over the real wire (#123).
    conn = connect(server)
    try:
        cur = conn.cursor()
        cur.execute("CREATE TABLE a (id int, nums int[])")
        cur.execute("INSERT INTO a VALUES (1, ARRAY[1,2,3]), (2, ARRAY[4,5])")
        conn.commit()

        cur.execute("SELECT id FROM a WHERE nums @> ARRAY[1,2] ORDER BY id")
        assert cur.fetchall() == ([1],)
        cur.execute("SELECT id FROM a WHERE nums && ARRAY[3,4] ORDER BY id")
        assert [r[0] for r in cur.fetchall()] == [1, 2]

        # int[] decodes back to a Python list through the driver.
        cur.execute("SELECT nums FROM a WHERE id = 1")
        assert cur.fetchall() == ([[1, 2, 3]],)

        cur.execute("CREATE TABLE u (id int, tags uuid[])")
        cur.execute("INSERT INTO u VALUES (1, ARRAY['11111111-1111-1111-1111-111111111111'::uuid])")
        conn.commit()
        # The uuid[] OID (2951) lets the driver decode elements as UUID objects.
        import uuid as _uuid

        cur.execute("SELECT tags FROM u WHERE id = 1")
        assert cur.fetchall() == ([[_uuid.UUID("11111111-1111-1111-1111-111111111111")]],)
    finally:
        conn.close()


def test_prepare_execute_deallocate_via_driver(server):
    # SQL-level PREPARE / EXECUTE / DEALLOCATE over the real wire (#121). Distinct
    # from pg8000's own %s-parameter binding, which uses the extended protocol.
    conn = connect(server)
    try:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int, name text)")
        cur.execute("INSERT INTO t VALUES (1, 'alice'), (2, 'bob')")
        conn.commit()

        cur.execute("PREPARE byid (int) AS SELECT name FROM t WHERE id = $1")
        cur.execute("EXECUTE byid (2)")
        assert cur.fetchall() == (["bob"],)
        cur.execute("EXECUTE byid (1)")
        assert cur.fetchall() == (["alice"],)

        cur.execute("DEALLOCATE byid")
        with pytest.raises(pg8000.DatabaseError):
            cur.execute("EXECUTE byid (1)")
    finally:
        conn.close()


def test_explain_via_driver(server):
    # EXPLAIN returns a QUERY PLAN text column over the real wire (#122).
    conn = connect(server)
    try:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int, name text)")
        cur.execute("INSERT INTO t VALUES (1,'a'),(2,'b')")
        cur.execute("CREATE INDEX t_id_idx ON t (id)")
        conn.commit()

        cur.execute("EXPLAIN SELECT * FROM t WHERE id = 1")
        assert [d[0] for d in cur.description] == ["QUERY PLAN"]
        plan = "\n".join(row[0] for row in cur.fetchall())
        assert "Index Scan using t_id_idx on t" in plan

        cur.execute("EXPLAIN (FORMAT JSON) SELECT * FROM t WHERE id = 1")
        import json as _json

        doc = _json.loads(cur.fetchall()[0][0])
        assert doc[0]["Plan"]["Node Type"] == "Index Scan"
    finally:
        conn.close()


# -- auth / TLS via the real driver ------------------------------------------ #


def test_scram_auth_success_and_failure(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st, require_auth=True, users={"joe": "s3cret"})
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
        st.close()


def test_tls_connection(tmp_path):
    ca = trustme.CA()
    cert = ca.issue_cert("127.0.0.1")
    cert_file, key_file = tmp_path / "c.pem", tmp_path / "c.key"
    cert.cert_chain_pems[0].write_to_path(cert_file)
    cert.private_key_pem.write_to_path(key_file)
    st = Storage(str(tmp_path / "wt"))
    srv = SecantusPGServer(
        port=0, storage=st, tls_cert_file=str(cert_file), tls_key_file=str(key_file)
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
        st.close()


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
    # Real (WT-backed) Storage isolates uncommitted writes from the separate
    # white-box read below, so commit before inspecting the index list; and it
    # auto-creates the ``_id_`` index (like mongod), so exclude it from the set.
    conn.commit()
    names = {ix["name"] for ix in server.storage.list_indexes("db", "t")} - {"_id_"}
    assert names == {"ix_n", "ux_label"}
    cur.execute("DROP INDEX ix_n")
    conn.commit()
    names = {ix["name"] for ix in server.storage.list_indexes("db", "t")} - {"_id_"}
    assert names == {"ux_label"}
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


@pytest.fixture
def real_server(tmp_path):
    # A pg server over the real Storage (per the no-FakeStorage rule for new
    # tests) — used by the GRANT/REVOKE reflection test below.
    from secantus.storage import Storage

    srv = SecantusPGServer(port=0, storage=Storage(str(tmp_path / "wt")))
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


def test_grant_revoke_reflection_via_driver(real_server):
    # GRANT/REVOKE ON <table> persist and surface through
    # information_schema.role_table_grants + has_table_privilege() over the wire
    # (trust mode: recorded and reflected, not enforced).
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, n int)")
    cur.execute("GRANT SELECT, INSERT ON t TO alice")
    cur.execute("GRANT SELECT ON t TO PUBLIC")
    cur.execute(
        "SELECT grantee, privilege_type FROM information_schema.role_table_grants "
        "ORDER BY grantee, privilege_type"
    )
    assert cur.fetchall() == (
        ["PUBLIC", "SELECT"],
        ["alice", "INSERT"],
        ["alice", "SELECT"],
    )
    cur.execute("SELECT has_table_privilege('alice', 't', 'SELECT')")
    assert cur.fetchall() == ([True],)
    cur.execute("SELECT has_table_privilege('alice', 't', 'UPDATE')")
    assert cur.fetchall() == ([False],)
    cur.execute("REVOKE INSERT ON t FROM alice")
    cur.execute(
        "SELECT privilege_type FROM information_schema.table_privileges "
        "WHERE grantee = 'alice' ORDER BY privilege_type"
    )
    assert cur.fetchall() == (["SELECT"],)
    conn.close()


def test_set_role_and_session_authorization_via_driver(real_server):
    # SET ROLE changes current_user (not session_user); SET SESSION AUTHORIZATION
    # changes both; RESET restores the login — all over the wire.
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("SELECT session_user")
    login = cur.fetchall()[0][0]
    cur.execute("SET ROLE analyst")
    cur.execute("SELECT current_user, session_user")
    assert cur.fetchall() == (["analyst", login],)
    cur.execute("RESET ROLE")
    cur.execute("SELECT current_user")
    assert cur.fetchall() == ([login],)
    cur.execute("SET SESSION AUTHORIZATION alice")
    cur.execute("SELECT current_user, session_user")
    assert cur.fetchall() == (["alice", "alice"],)
    cur.execute("RESET SESSION AUTHORIZATION")
    cur.execute("SELECT session_user")
    assert cur.fetchall() == ([login],)
    conn.close()


def test_row_level_security_reflection_via_driver(real_server):
    # RLS DDL round-trips over the wire and pg_policies reflects it. (Enforcement
    # is unit-tested with gated sessions in test_sql_rls.py; the trust-mode wire
    # connection records but doesn't enforce.)
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE doc (id bigint primary key, owner text)")
    cur.execute("INSERT INTO doc VALUES (1,'alice'),(2,'bob')")
    cur.execute("ALTER TABLE doc ENABLE ROW LEVEL SECURITY")
    cur.execute("CREATE POLICY p_owner ON doc FOR ALL TO public USING (owner = current_user)")
    cur.execute(
        "SELECT tablename, policyname, cmd, qual FROM pg_catalog.pg_policies ORDER BY policyname"
    )
    assert cur.fetchall() == (["doc", "p_owner", "ALL", "owner = current_user"],)
    # Trust-mode connection is unrestricted: both rows visible.
    cur.execute("SELECT id FROM doc ORDER BY id")
    assert cur.fetchall() == ([1], [2])
    cur.execute("DROP POLICY p_owner ON doc")
    cur.execute("SELECT count(*) FROM pg_catalog.pg_policies")
    assert cur.fetchall() == ([0],)
    conn.close()


def test_udf_reflection_via_driver(real_server):
    # CREATE FUNCTION surfaces through pg_proc + pg_get_functiondef over the wire.
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute(
        "CREATE FUNCTION addup(a int, b int) RETURNS int AS $$ SELECT a + b $$ LANGUAGE sql"
    )
    cur.execute(
        "SELECT proname, pg_get_function_arguments(oid), pg_get_function_result(oid) "
        "FROM pg_catalog.pg_proc WHERE proname = 'addup'"
    )
    assert cur.fetchall() == (["addup", "a integer, b integer", "integer"],)
    cur.execute(
        "SELECT routine_name, data_type FROM information_schema.routines "
        "WHERE routine_name = 'addup'"
    )
    assert cur.fetchall() == (["addup", "integer"],)
    conn.close()


def test_column_privileges_reflection_via_driver(real_server):
    # Column-scoped GRANT round-trips over the wire; column_privileges +
    # has_column_privilege reflect it. (Enforcement is unit-tested with gated
    # sessions in test_sql_column_grants.py.)
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, a int, secret text)")
    cur.execute("GRANT SELECT (id, a) ON t TO alice")
    cur.execute(
        "SELECT grantee, column_name, privilege_type FROM information_schema.column_privileges "
        "ORDER BY column_name"
    )
    assert cur.fetchall() == (["alice", "a", "SELECT"], ["alice", "id", "SELECT"])
    cur.execute("SELECT has_column_privilege('alice', 't', 'a', 'SELECT')")
    assert cur.fetchall() == ([True],)
    cur.execute("SELECT has_column_privilege('alice', 't', 'secret', 'SELECT')")
    assert cur.fetchall() == ([False],)
    conn.close()


def test_select_for_update_via_driver(real_server):
    # SELECT … FOR UPDATE / FOR SHARE round-trip over the wire (SQLAlchemy's
    # with_for_update emits these); single-node no-op that returns the rows.
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id bigint primary key, a int)")
    cur.execute("INSERT INTO t VALUES (1, 10), (2, 20)")
    cur.execute("SELECT id FROM t WHERE a = 20 FOR UPDATE")
    assert cur.fetchall() == ([2],)
    cur.execute("SELECT id FROM t ORDER BY id FOR SHARE OF t")
    assert cur.fetchall() == ([1], [2])
    conn.close()


def test_truncate_via_driver(real_server):
    # TRUNCATE empties a table and RESTART IDENTITY resets its serial, over the wire.
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (id serial primary key, v int)")
    cur.execute("INSERT INTO t (v) VALUES (1), (2), (3)")
    cur.execute("SELECT count(*) FROM t")
    assert cur.fetchall() == ([3],)
    cur.execute("TRUNCATE TABLE t RESTART IDENTITY")
    cur.execute("SELECT count(*) FROM t")
    assert cur.fetchall() == ([0],)
    cur.execute("INSERT INTO t (v) VALUES (9)")
    cur.execute("SELECT id FROM t")
    assert cur.fetchall() == ([1],)
    conn.close()


def test_index_constraint_reflection_via_driver(real_server):
    # pg_indexes renders CREATE [UNIQUE] INDEX (with DESC), and
    # pg_get_constraintdef renders PRIMARY KEY / FOREIGN KEY / UNIQUE / CHECK —
    # exactly what psql's \d and SQLAlchemy read. (#134)
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("CREATE TABLE parent (id bigint primary key)")
    cur.execute("CREATE TABLE t (id bigint primary key, a int, b int, p bigint)")
    cur.execute("ALTER TABLE t ADD CONSTRAINT t_p_fkey FOREIGN KEY (p) REFERENCES parent(id)")
    cur.execute("ALTER TABLE t ADD CONSTRAINT t_b_key UNIQUE (b)")
    cur.execute("ALTER TABLE t ADD CONSTRAINT t_check CHECK (a > 0)")
    cur.execute("CREATE INDEX idx_desc ON t (a DESC, b)")
    cur.execute(
        "SELECT indexname, indexdef FROM pg_catalog.pg_indexes "
        "WHERE tablename = 't' ORDER BY indexname"
    )
    assert cur.fetchall() == (
        ["idx_desc", "CREATE INDEX idx_desc ON public.t USING btree (a DESC, b)"],
        ["t_b_key", "CREATE UNIQUE INDEX t_b_key ON public.t USING btree (b)"],
        ["t_pkey", "CREATE UNIQUE INDEX t_pkey ON public.t USING btree (id)"],
    )
    # No WiredTiger physical _id_ index leaks into the SQL surface.
    cur.execute("SELECT count(*) FROM pg_catalog.pg_indexes WHERE indexname = '_id_'")
    assert cur.fetchall() == ([0],)
    cur.execute(
        "SELECT conname, pg_get_constraintdef(oid) FROM pg_catalog.pg_constraint ORDER BY conname"
    )
    defs = dict(tuple(r) for r in cur.fetchall())
    assert defs["t_pkey"] == "PRIMARY KEY (id)"
    assert defs["t_p_fkey"] == "FOREIGN KEY (p) REFERENCES parent(id)"
    assert defs["t_b_key"] == "UNIQUE (b)"
    assert defs["t_check"] == "CHECK ((a > 0))"
    conn.close()


def test_advisory_locks_via_driver(real_server):
    # pg_advisory_lock family round-trips over the wire and pg_locks reflects the
    # held locks; single-node no-op that always grants (#135).
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("SELECT pg_advisory_lock(1)")
    cur.execute("SELECT pg_try_advisory_lock(1, 2)")
    assert cur.fetchall() == ([True],)
    cur.execute("SELECT pg_advisory_lock_shared(5)")
    cur.execute(
        "SELECT locktype, classid, objid, objsubid, mode, granted "
        "FROM pg_catalog.pg_locks ORDER BY objid"
    )
    assert cur.fetchall() == (
        ["advisory", 0, 1, 1, "ExclusiveLock", True],
        ["advisory", 1, 2, 2, "ExclusiveLock", True],
        ["advisory", 0, 5, 1, "ShareLock", True],
    )
    cur.execute("SELECT pg_advisory_unlock(1)")
    assert cur.fetchall() == ([True],)
    cur.execute("SELECT pg_advisory_unlock(1)")  # not held any more
    assert cur.fetchall() == ([False],)
    cur.execute("SELECT pg_advisory_unlock_all()")
    cur.execute("SELECT count(*) FROM pg_catalog.pg_locks")
    assert cur.fetchall() == ([0],)
    conn.close()


def test_show_all_and_pg_settings_via_driver(real_server):
    # SHOW ALL returns (name, setting, description); pg_settings reflects a SET. (#136)
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("SET TimeZone = 'America/New_York'")
    cur.execute("SHOW ALL")
    assert [d[0] for d in cur.description] == ["name", "setting", "description"]
    by_name = {r[0]: r[1] for r in cur.fetchall()}
    assert by_name["TimeZone"] == "America/New_York"
    assert by_name["client_encoding"] == "UTF8"
    cur.execute(
        "SELECT setting, vartype, source FROM pg_catalog.pg_settings WHERE name = 'TimeZone'"
    )
    assert cur.fetchall() == (["America/New_York", "string", "session"],)
    conn.close()


def test_role_membership_via_driver(real_server):
    # GRANT <role> TO <member> round-trips over the wire and pg_auth_members
    # reflects it (joined to pg_roles by oid). (#138)
    conn = connect(real_server)
    cur = conn.cursor()
    cur.execute("CREATE ROLE readers")
    cur.execute("CREATE ROLE alice LOGIN")
    cur.execute("GRANT readers TO alice WITH ADMIN OPTION")
    cur.execute(
        "SELECT r.rolname, m.rolname, am.admin_option "
        "FROM pg_catalog.pg_auth_members am "
        "JOIN pg_catalog.pg_roles r ON r.oid = am.roleid "
        "JOIN pg_catalog.pg_roles m ON m.oid = am.member"
    )
    assert cur.fetchall() == (["readers", "alice", True],)
    cur.execute("REVOKE readers FROM alice")
    cur.execute("SELECT count(*) FROM pg_catalog.pg_auth_members")
    assert cur.fetchall() == ([0],)
    conn.close()


def test_pg_stat_activity_via_driver(real_server):
    # pg_stat_activity reflects the live backend: a client running the query sees
    # its own row as state='active' with a distinct pid and this query. (#137)
    conn = connect(real_server, application_name="probe")
    cur = conn.cursor()
    cur.execute(
        "SELECT pid, datname, usename, application_name, state, query, backend_type "
        "FROM pg_catalog.pg_stat_activity"
    )
    rows = cur.fetchall()
    assert len(rows) == 1
    pid, datname, usename, app, state, query, backend_type = rows[0]
    assert isinstance(pid, int) and pid > 0
    assert app == "probe"
    assert state == "active"
    assert "pg_stat_activity" in query
    assert backend_type == "client backend"
    # backend_start / query_start are real timestamps.
    cur.execute("SELECT backend_start IS NOT NULL, query_start IS NOT NULL FROM pg_stat_activity")
    assert cur.fetchall() == ([True, True],)
    # pg_stat_database counts this one backend (only this connection is live).
    cur.execute("SELECT numbackends FROM pg_catalog.pg_stat_database")
    assert cur.fetchall() == ([1],)
    conn.close()


def test_set_local_reverts_at_commit_via_driver(real_server):
    # SET LOCAL applies inside the transaction and reverts when it commits. (#136)
    conn = connect(real_server)
    conn.autocommit = False
    cur = conn.cursor()
    cur.execute("SET statement_timeout = '10s'")  # session value
    conn.commit()
    cur.execute("SET LOCAL statement_timeout = '99s'")  # opens a txn implicitly
    cur.execute("SHOW statement_timeout")
    assert cur.fetchall() == (["99s"],)
    conn.commit()
    cur.execute("SHOW statement_timeout")
    assert cur.fetchall() == (["10s"],)  # reverted to the session value
    conn.close()


def test_two_phase_commit_cross_connection_via_driver(real_server):
    # PREPARE TRANSACTION on one connection, COMMIT PREPARED on another (#139).
    # The prepared xact lives in the server-wide registry, so a second backend
    # both sees it in pg_prepared_xacts and can commit it.
    setup = connect(real_server)
    setup.autocommit = True
    setup.cursor().execute("CREATE TABLE t2p (id int primary key, v text)")
    setup.close()

    a = connect(real_server)
    a.autocommit = True
    cura = a.cursor()
    cura.execute("BEGIN")
    cura.execute("INSERT INTO t2p VALUES (1, 'from-A')")
    cura.execute("PREPARE TRANSACTION 'wgtx'")

    b = connect(real_server)
    b.autocommit = True
    curb = b.cursor()
    # Uncommitted: B sees no rows yet.
    curb.execute("SELECT count(*) FROM t2p")
    assert curb.fetchall() == ([0],)
    # B sees the prepared xact in the shared registry.
    curb.execute("SELECT gid, database FROM pg_catalog.pg_prepared_xacts")
    assert curb.fetchall() == (["wgtx", "db"],)
    # B commits the xact A prepared.
    curb.execute("COMMIT PREPARED 'wgtx'")
    curb.execute("SELECT * FROM t2p")
    assert curb.fetchall() == ([1, "from-A"],)
    curb.execute("SELECT count(*) FROM pg_catalog.pg_prepared_xacts")
    assert curb.fetchall() == ([0],)
    a.close()
    b.close()


def test_two_phase_rollback_prepared_via_driver(real_server):
    setup = connect(real_server)
    setup.autocommit = True
    cur = setup.cursor()
    cur.execute("CREATE TABLE t2pr (id int primary key)")
    cur.execute("BEGIN")
    cur.execute("INSERT INTO t2pr VALUES (5)")
    cur.execute("PREPARE TRANSACTION 'rbk'")
    cur.execute("ROLLBACK PREPARED 'rbk'")
    cur.execute("SELECT count(*) FROM t2pr")
    assert cur.fetchall() == ([0],)
    setup.close()
