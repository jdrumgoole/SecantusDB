"""Parse-time analysis matches Postgres: errors arrive in reply to Parse.

Postgres runs parse analysis when it receives Parse, so a syntax error (42601)
and a missing relation (42P01) are the Parse message's reply. This server
answered ParseComplete and deferred both to Execute (measured 2026-09-18), so a
client that prepares now and executes later -- pgx's Prepare, a JDBC
prepareStatement with server-side prepare -- saw the error on the wrong
message, or not until much later.

The relation check reuses planning's own resolver, so the risk it guards
against is the opposite one -- rejecting a VALID query at Parse -- and most of
the cases below are relations that must still be accepted.

Also here, found while probing the same path: ``ABORT`` (Postgres' synonym for
``ROLLBACK``) and ``CHECKPOINT`` were 42601 syntax errors, and a rejected
``ABORT`` inside a failed transaction block left the session stuck at 25P02.
"""

from __future__ import annotations

import re
import socket
import struct

import pytest

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


def _msg(kind: bytes, body: bytes = b"") -> bytes:
    return kind + struct.pack("!i", len(body) + 4) + body


def _cstr(s: str) -> bytes:
    return s.encode() + b"\x00"


@pytest.fixture
def server(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        c = psycopg.connect(
            host=host, port=port, user="postgres", dbname="postgres", autocommit=True
        )
        try:
            for ddl in (
                "create table t (id int primary key, a int)",
                "create table u (id int, b int)",
                "create view v as select * from t",
                "create sequence sq",
                "create materialized view mv as select * from t",
                "create schema s2",
                "create table s2.t2 (z int)",
            ):
                c.execute(ddl)
            yield srv, c
        finally:
            c.close()
    finally:
        srv.stop()
        st.close()


@pytest.fixture
def wire(server):
    srv, _ = server
    s = socket.create_connection(srv.address, timeout=10)
    try:
        s.sendall(pgwire.build_startup_message({"user": "postgres", "database": "postgres"}))
        while pgwire.read_message(s).type != "Z":
            pass
        yield s
    finally:
        s.close()


def _parse_reply(s: socket.socket, query: str) -> tuple[str, str | None, str | None]:
    """Send Parse + Flush; return (message type, SQLSTATE, message) for the reply."""
    s.sendall(_msg(b"P", _cstr("s1") + _cstr(query) + struct.pack("!h", 0)) + _msg(b"H"))
    m = pgwire.read_message(s)
    code = text = None
    if m.type == "E":
        c = re.search(rb"C([0-9A-Z]{5})\x00", m.payload)
        t = re.search(rb"M([^\x00]*)\x00", m.payload)
        code = c.group(1).decode() if c else None
        text = t.group(1).decode() if t else None
    s.sendall(_msg(b"C", b"S" + _cstr("s1")) + _msg(b"S"))
    while pgwire.read_message(s).type != "Z":
        pass
    return m.type, code, text


@pytest.mark.parametrize(
    "query",
    [
        "select * from nowhere_at_all",
        "select from nowhere_at_all",
        "insert into nowhere_at_all values (1)",
        "update nowhere_at_all set a = 1",
        "delete from nowhere_at_all",
        "select * from t join nowhere_at_all n on true",
        "select * from s2.nowhere_at_all",
    ],
)
def test_a_missing_relation_fails_the_parse(wire, query: str) -> None:
    kind, code, text = _parse_reply(wire, query)
    assert (kind, code) == ("E", "42P01"), (kind, code, text)
    assert "nowhere_at_all" in (text or "")


@pytest.mark.parametrize(
    "query",
    [
        "select * from t",
        "select * from v",
        "select * from sq",
        "select * from mv",
        "select * from s2.t2",
        "select * from public.t",
        "select * from pg_catalog.pg_class limit 1",
        "select * from pg_class limit 1",
        "select * from information_schema.tables limit 1",
        "with a as (select 1 x) select * from a",
        "with recursive r(n) as (select 1 union all select n+1 from r where n<3) select * from r",
        "select * from generate_series(1,2) g",
        "select * from unnest(array[1,2]) x",
        "select * from (values (1),(2)) v(x)",
        "select * from t where id in (select id from u)",
        "select * from t, lateral (select * from u where u.id = t.id) l",
        "insert into t values ($1, 2)",
        "insert into t (id, a) select id, b from u",
        "update t set a = u.b from u where u.id = t.id",
        "delete from t using u where u.id = t.id",
        "insert into t values (9, 9) on conflict (id) do update set a = excluded.a",
        "select nextval('sq')",
    ],
)
def test_a_valid_relation_still_parses(wire, query: str) -> None:
    assert _parse_reply(wire, query)[0] == "1"


def test_a_temp_table_parses(wire) -> None:
    wire.sendall(_msg(b"Q", _cstr("create temp table tt (x int)")))
    while pgwire.read_message(wire).type != "Z":
        pass
    assert _parse_reply(wire, "select * from tt")[0] == "1"
    assert _parse_reply(wire, "select * from pg_temp.tt")[0] == "1"


@pytest.mark.parametrize(
    ("query", "token"),
    [("this is not sql", "this"), ("wat", "wat")],
)
def test_garbage_is_a_syntax_error_at_parse(wire, query: str, token: str) -> None:
    """``this is not sql`` parses as ``NOT (this IS sql)`` -- a bare expression
    the old allow-list of node types missed -- and answered 0A000 at Execute."""
    kind, code, text = _parse_reply(wire, query)
    assert (kind, code) == ("E", "42601"), (kind, code, text)
    assert text == f'syntax error at or near "{token}"'


def test_simple_query_names_the_leading_token(server) -> None:
    _, c = server
    with pytest.raises(psycopg.errors.SyntaxError, match='at or near "this"'):
        c.execute("this is not sql")


def test_abort_is_rollback(server) -> None:
    _, c = server
    for spelling in ("abort", "abort work", "abort transaction"):
        r = c.pgconn.exec_(b"begin")
        assert r.command_status == b"BEGIN"
        r = c.pgconn.exec_(spelling.encode())
        assert r.error_message == b"", r.error_message
        assert r.command_status == b"ROLLBACK", spelling


def test_abort_ends_a_failed_transaction_block(server) -> None:
    """The case that mattered: a failed block could not be ended with ABORT,
    because the ABORT itself was rejected -- so the session stayed at 25P02."""
    _, c = server
    c.pgconn.exec_(b"begin")
    c.pgconn.exec_(b"select * from nowhere_at_all")  # fails the block
    r = c.pgconn.exec_(b"abort")
    assert r.command_status == b"ROLLBACK", r.error_message
    assert c.pgconn.exec_(b"select 1").command_status == b"SELECT 1"


def test_checkpoint_is_accepted(server) -> None:
    _, c = server
    assert c.pgconn.exec_(b"checkpoint").command_status == b"CHECKPOINT"
