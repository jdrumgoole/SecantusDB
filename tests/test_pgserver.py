"""End-to-end tests for the PostgreSQL-wire server (P1).

A pure-Python PG v3 client drives a real ``SecantusPGServer`` over a loopback
socket — the deterministic "a real client connected and got rows" proof. The
server runs over an injected in-memory ``FakeStorage`` so no WiredTiger build is
needed; the wire framing and handshake are exercised for real.
"""

from __future__ import annotations

import contextlib
import socket
import struct

import pytest

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from sqlfake import FakeStorage


class PGClient:
    """Minimal PostgreSQL v3 simple-query client."""

    def __init__(self, host: str, port: int) -> None:
        self.sock = socket.create_connection((host, port), timeout=5)

    def request_ssl(self) -> bytes:
        self.sock.sendall(struct.pack("!ii", 8, pgwire.SSL_REQUEST_CODE))
        return self.sock.recv(1)

    def startup(self, user: str = "secantus", database: str = "testdb") -> list[pgwire.Message]:
        self.sock.sendall(pgwire.build_startup_message({"user": user, "database": database}))
        return self._read_until_ready()

    def query(self, sql: str) -> list[pgwire.Message]:
        self.sock.sendall(pgwire.build_query(sql))
        return self._read_until_ready()

    def _read_until_ready(self) -> list[pgwire.Message]:
        msgs: list[pgwire.Message] = []
        while True:
            m = pgwire.read_message(self.sock)
            msgs.append(m)
            if m.type == "Z":  # ReadyForQuery
                return msgs

    def close(self) -> None:
        with contextlib.suppress(OSError):
            self.sock.sendall(pgwire.build_terminate())
        self.sock.close()


def parse_results(msgs: list[pgwire.Message]) -> dict:
    """Collapse a message stream into {results, errors, empty, params}."""
    results: list[dict] = []
    errors: list[dict] = []
    empty = False
    columns: list[str] = []
    rows: list[list[bytes | None]] = []
    for m in msgs:
        if m.type == "T":
            columns = pgwire.parse_row_description(m.payload)
            rows = []
        elif m.type == "D":
            rows.append(pgwire.parse_data_row(m.payload))
        elif m.type == "C":
            results.append(
                {"tag": pgwire.parse_command_complete(m.payload), "columns": columns, "rows": rows}
            )
            columns, rows = [], []
        elif m.type == "E":
            errors.append(pgwire.parse_error_response(m.payload))
        elif m.type == "I":
            empty = True
    return {"results": results, "errors": errors, "empty": empty}


@pytest.fixture
def server():
    srv = SecantusPGServer(port=0, storage=FakeStorage())
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def client(server):
    host, port = server.address
    c = PGClient(host, port)
    c.startup()
    try:
        yield c
    finally:
        c.close()


# --------------------------------------------------------------------------- #


def test_startup_sends_parameter_status_and_ready(server):
    host, port = server.address
    c = PGClient(host, port)
    try:
        msgs = c.startup()
        types = [m.type for m in msgs]
        assert types[0] == "R"  # AuthenticationOk
        assert "S" in types  # ParameterStatus
        assert types[-1] == "Z"  # ReadyForQuery
        # server_version is advertised so libpq can gate features.
        params = {}
        for m in msgs:
            if m.type == "S":
                name, value, _ = m.payload.split(b"\x00", 2)
                params[name.decode()] = value.decode()
        assert params["server_version"].startswith("15")
        assert params["client_encoding"] == "UTF8"
    finally:
        c.close()


def test_select_one(client):
    out = parse_results(client.query("SELECT 1"))
    assert out["results"][0]["tag"] == "SELECT 1"
    assert out["results"][0]["columns"] == ["?column?"]
    assert out["results"][0]["rows"] == [[b"1"]]


def test_create_insert_select_roundtrip(client):
    client.query("CREATE TABLE users (id bigint primary key, name text, active boolean)")
    out = parse_results(
        client.query("INSERT INTO users (id, name, active) VALUES (1, 'alice', true)")
    )
    assert out["results"][0]["tag"] == "INSERT 0 1"

    out = parse_results(client.query("SELECT id, name, active FROM users"))
    res = out["results"][0]
    assert res["tag"] == "SELECT 1"
    assert res["columns"] == ["id", "name", "active"]
    assert res["rows"] == [[b"1", b"alice", b"t"]]


def test_null_renders_as_minus_one(client):
    client.query("CREATE TABLE t (id bigint primary key, note text)")
    client.query("INSERT INTO t (id, note) VALUES (1, NULL)")
    res = parse_results(client.query("SELECT note FROM t"))["results"][0]
    assert res["rows"] == [[None]]


def test_error_response_keeps_connection_alive(client):
    out = parse_results(client.query("SELECT * FROM nope"))
    assert out["errors"][0]["C"] == "42P01"
    # The connection survives — a follow-up query still works.
    assert parse_results(client.query("SELECT 7"))["results"][0]["rows"] == [[b"7"]]


def test_multi_statement_single_query(client):
    out = parse_results(
        client.query(
            "CREATE TABLE m (id bigint primary key, n int);"
            "INSERT INTO m (id, n) VALUES (1, 10);"
            "SELECT n FROM m;"
        )
    )
    assert [r["tag"] for r in out["results"]] == ["CREATE TABLE", "INSERT 0 1", "SELECT 1"]
    assert out["results"][-1]["rows"] == [[b"10"]]


def test_empty_query(client):
    out = parse_results(client.query(""))
    assert out["empty"] is True


def test_ssl_request_is_declined_then_startup_proceeds(server):
    host, port = server.address
    c = PGClient(host, port)
    try:
        assert c.request_ssl() == b"N"
        msgs = c.startup()
        assert msgs[-1].type == "Z"
        assert parse_results(c.query("SELECT 1"))["results"][0]["rows"] == [[b"1"]]
    finally:
        c.close()


# -- P2: session functions / SET / catalog over the wire --------------------- #


def test_version_and_current_database_over_wire(client):
    res = parse_results(client.query("SELECT version()"))["results"][0]
    assert res["columns"] == ["version"]
    assert res["rows"][0][0].startswith(b"PostgreSQL 15.0 (SecantusDB)")
    # The startup used database "testdb".
    db = parse_results(client.query("SELECT current_database()"))["results"][0]
    assert db["rows"] == [[b"testdb"]]


def test_set_emits_parameter_status(client):
    msgs = client.query("SET client_encoding = 'LATIN1'")
    statuses = {}
    for m in msgs:
        if m.type == "S":
            name, value, _ = m.payload.split(b"\x00", 2)
            statuses[name.decode()] = value.decode()
    assert statuses.get("client_encoding") == "LATIN1"
    assert any(m.type == "C" and m.payload.startswith(b"SET") for m in msgs)


def test_show_over_wire(client):
    client.query("SET search_path TO appschema")
    res = parse_results(client.query("SHOW search_path"))["results"][0]
    assert res["tag"] == "SHOW"
    assert res["rows"] == [[b"appschema"]]


def test_information_schema_over_wire(client):
    client.query("CREATE TABLE widgets (id bigint primary key, label text)")
    res = parse_results(
        client.query(
            "SELECT table_name FROM information_schema.tables WHERE table_name = 'widgets'"
        )
    )["results"][0]
    assert res["rows"] == [[b"widgets"]]


def test_connection_cap_rejects_over_limit():
    """Over the max_connections cap, an accepted socket is closed immediately
    rather than served (issue #194)."""
    srv = SecantusPGServer(port=0, storage=FakeStorage(), max_connections=1)
    srv.start()
    try:
        host, port = srv.address
        c1 = PGClient(host, port)
        c1.startup()  # occupies the single connection slot
        # A second connection is accepted, then closed immediately by the server.
        s2 = socket.create_connection((host, port), timeout=5)
        try:
            s2.sendall(pgwire.build_startup_message({"user": "x", "database": "d"}))
            s2.settimeout(5)
            try:
                data = s2.recv(1)
                assert data == b""  # clean EOF: the server closed the socket
            except (ConnectionResetError, ConnectionAbortedError):
                pass  # RST also acceptable — the server dropped the connection
        finally:
            s2.close()
        # The first connection is unaffected.
        assert parse_results(c1.query("SELECT 1"))["results"][0]["rows"] == [[b"1"]]
        c1.close()
    finally:
        srv.stop()
