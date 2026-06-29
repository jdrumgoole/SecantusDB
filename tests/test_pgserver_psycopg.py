"""Real-driver gauge: psycopg 3 (libpq) against ``SecantusPGServer``.

``psycopg`` is the mainstream Python PostgreSQL driver and a thin layer over
**libpq** (bundled via the ``psycopg[binary]`` wheel, so it runs here). Unlike
the pure-Python pg8000 it sends most parameters in the **binary** format and
maintains server-side prepared statements with ``DEALLOCATE`` — the strictest
wire-protocol exercise we have. It found (and these tests now guard) two real
bugs: binary ``timestamptz``/``numeric`` parameters weren't decoded, and the
``DEALLOCATE`` psycopg emits to recycle prepared statements wasn't accepted.
"""

from __future__ import annotations

import datetime as _dt
from decimal import Decimal

import bson
import pytest

from secantus.sql.pgserver import SecantusPGServer
from sqlfake import FakeStorage

psycopg = pytest.importorskip("psycopg")


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
    return psycopg.connect(host=host, port=port, dbname="db", user="joe", **kw)


# --------------------------------------------------------------------------- #


def test_connect_and_select_one(server):
    with connect(server, autocommit=True) as conn:
        assert conn.execute("SELECT 1").fetchone() == (1,)


def test_crud_with_binary_parameters(server):
    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TABLE users (id bigint primary key, name text, age int)")
        conn.execute("INSERT INTO users (id,name,age) VALUES (%s,%s,%s)", (1, "alice", 30))
        conn.execute("INSERT INTO users (id,name,age) VALUES (%s,%s,%s)", (2, "bob", 17))
        rows = conn.execute(
            "SELECT id, name FROM users WHERE age > %s ORDER BY id", (18,)
        ).fetchall()
        assert rows == [(1, "alice")]


def test_binary_parameter_type_roundtrip(server):
    # The case that found the binary-param bug: psycopg sends numeric/bool/
    # timestamptz parameters in binary, which the server must decode.
    with connect(server, autocommit=True) as conn:
        conn.execute(
            "CREATE TABLE m (id bigint primary key, price numeric, flag boolean, at timestamptz)"
        )
        when = _dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=_dt.timezone.utc)
        conn.execute(
            "INSERT INTO m (id, price, flag, at) VALUES (%s, %s, %s, %s)",
            (1, Decimal("19.99"), True, when),
        )
        row = conn.execute("SELECT id, price, flag, at FROM m").fetchone()
        assert row[0] == 1
        assert row[1] == Decimal("19.99")
        assert row[2] is True
        assert row[3] == when


def test_prepared_statement_and_deallocate(server):
    # prepare=True forces a server-side prepared statement; psycopg later emits
    # DEALLOCATE to recycle it, which the server accepts as a no-op.
    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TABLE t (id bigint primary key, n int)")
        conn.execute("INSERT INTO t (id, n) VALUES (1, 10)")
        for _ in range(3):
            assert conn.execute("SELECT n FROM t WHERE id = %s", (1,), prepare=True).fetchone() == (
                10,
            )


def test_group_by_and_join(server):
    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TABLE sales (id bigint primary key, region text, amount int)")
        for i, (r, a) in enumerate([("e", 10), ("e", 20), ("w", 30)], 1):
            conn.execute("INSERT INTO sales (id,region,amount) VALUES (%s,%s,%s)", (i, r, a))
        rows = conn.execute(
            "SELECT region, SUM(amount) FROM sales GROUP BY region ORDER BY region"
        ).fetchall()
        assert rows == [("e", 30), ("w", 30)]


def test_transaction_commit_and_rollback(server):
    with connect(server) as conn:  # autocommit off
        conn.execute("CREATE TABLE t (id bigint primary key, n int)")
        conn.commit()
        conn.execute("INSERT INTO t (id, n) VALUES (1, 10)")
        conn.rollback()
        assert conn.execute("SELECT count(*) FROM t").fetchone() == (0,)
        conn.execute("INSERT INTO t (id, n) VALUES (2, 20)")
        conn.commit()
        assert conn.execute("SELECT count(*) FROM t").fetchone() == (1,)


def test_reflected_table_read(server):
    server.storage.insert(
        "db",
        "people",
        [
            {"_id": bson.Int64(1), "name": "alice", "profile": {"city": "NYC"}},
            {"_id": bson.Int64(2), "name": "bob", "profile": {"city": "LA"}},
        ],
    )
    with connect(server, autocommit=True) as conn:
        rows = conn.execute("SELECT name, profile->>'city' FROM people ORDER BY _id").fetchall()
        assert rows == [("alice", "NYC"), ("bob", "LA")]


def test_write_to_reflected_table(server):
    # Dual-protocol writes through libpq binary params: INSERT/UPDATE/DELETE on a
    # Mongo-written collection with no CREATE TABLE, verified as a real document.
    server.storage.insert(
        "db",
        "people",
        [
            {"_id": bson.Int64(1), "name": "alice", "age": bson.Int64(30)},
            {"_id": bson.Int64(2), "name": "bob", "age": bson.Int64(17)},
        ],
    )
    with connect(server, autocommit=True) as conn:
        conn.execute("INSERT INTO people (_id, name, age) VALUES (%s, %s, %s)", (3, "dave", 40))
        conn.execute("UPDATE people SET age = %s WHERE name = %s", (99, "alice"))
        conn.execute("DELETE FROM people WHERE age < %s", (18,))
        rows = conn.execute("SELECT _id, name, age FROM people ORDER BY _id").fetchall()
        assert rows == [(1, "alice", 99), (3, "dave", 40)]
    stored = server.storage.find_matching("db", "people", {"_id": bson.Int64(3)})
    assert stored[0]["name"] == "dave" and stored[0]["age"] == 40


def test_undefined_table_sqlstate(server):
    with connect(server, autocommit=True) as conn, pytest.raises(psycopg.errors.UndefinedTable):
        conn.execute("SELECT * FROM nonexistent")


# -- SQLAlchemy via the psycopg dialect -------------------------------------- #


def test_sqlalchemy_psycopg_reflection(server):
    sa = pytest.importorskip("sqlalchemy")
    host, port = server.address
    engine = sa.create_engine(f"postgresql+psycopg://joe@{host}:{port}/db")
    try:
        with engine.begin() as conn:
            conn.execute(sa.text("CREATE TABLE users (id bigint primary key, name text, age int)"))
            conn.execute(sa.text("CREATE INDEX ix_name ON users (name)"))
        insp = sa.inspect(engine)
        assert [c["name"] for c in insp.get_columns("users")] == ["id", "name", "age"]
        assert insp.get_pk_constraint("users")["constrained_columns"] == ["id"]
        t = sa.Table("users", sa.MetaData(), autoload_with=engine)
        assert [c.name for c in t.columns] == ["id", "name", "age"]
        assert {ix.name for ix in t.indexes} == {"ix_name"}
    finally:
        engine.dispose()
