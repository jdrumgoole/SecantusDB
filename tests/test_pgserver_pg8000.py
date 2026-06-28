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


def test_ssl_request_declined_without_tls(server):
    # Sanity: a raw SSLRequest is declined when TLS isn't configured.
    host, port = server.address
    s = socket.create_connection((host, port), timeout=5)
    s.sendall(struct.pack("!ii", 8, pgwire.SSL_REQUEST_CODE))
    assert s.recv(1) == b"N"
    s.close()
