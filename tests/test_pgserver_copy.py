"""``COPY … FROM/TO STDIN/STDOUT`` over the wire.

Drives the CopyIn / CopyOut sub-protocol against a real ``SecantusPGServer`` over
a loopback socket (backed by an in-memory ``FakeStorage`` — no WiredTiger). The
client sends ``COPY t FROM STDIN``, gets ``CopyInResponse`` ('G'), streams
``CopyData`` ('d') + ``CopyDone`` ('c'), and reads ``CommandComplete`` +
``ReadyForQuery``; the reverse for ``COPY t TO STDOUT``.
"""

from __future__ import annotations

import contextlib
import socket

import pytest

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from sqlfake import FakeStorage


class PGClient:
    def __init__(self, host: str, port: int) -> None:
        self.sock = socket.create_connection((host, port), timeout=5)

    def startup(self, user: str = "secantus", database: str = "testdb") -> None:
        self.sock.sendall(pgwire.build_startup_message({"user": user, "database": database}))
        self._read_until_ready()

    def query(self, sql: str) -> list[pgwire.Message]:
        self.sock.sendall(pgwire.build_query(sql))
        return self._read_until_ready()

    def read_message(self) -> pgwire.Message:
        return pgwire.read_message(self.sock)

    def send(self, data: bytes) -> None:
        self.sock.sendall(data)

    def _read_until_ready(self) -> list[pgwire.Message]:
        msgs: list[pgwire.Message] = []
        while True:
            m = pgwire.read_message(self.sock)
            msgs.append(m)
            if m.type == "Z":
                return msgs

    def read_until_ready(self) -> list[pgwire.Message]:
        return self._read_until_ready()

    def close(self) -> None:
        with contextlib.suppress(OSError):
            self.sock.sendall(pgwire.build_terminate())
        self.sock.close()


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
    c.query("CREATE TABLE t (id bigint primary key, name text, active boolean)")
    try:
        yield c
    finally:
        c.close()


def _tag(msgs: list[pgwire.Message]) -> str:
    for m in msgs:
        if m.type == "C":
            return pgwire.parse_command_complete(m.payload)
    return ""


def _copy_in(client: PGClient, sql: str, data: bytes) -> list[pgwire.Message]:
    """Send a COPY FROM STDIN and stream ``data``; return the trailing messages."""
    client.send(pgwire.build_query(sql))
    g = client.read_message()
    assert g.type == "G", f"expected CopyInResponse, got {g.type}"
    client.send(pgwire.copy_data(data))
    client.send(pgwire.copy_done())
    return client.read_until_ready()


# --------------------------------------------------------------------------- #


def test_copy_from_stdin_text(client):
    msgs = _copy_in(client, "COPY t (id, name, active) FROM STDIN", b"1\talice\tt\n2\tbob\tf\n")
    assert _tag(msgs) == "COPY 2"
    from test_pgserver import parse_results  # reuse the row collapser

    res = parse_results(client.query("SELECT id, name, active FROM t ORDER BY id"))["results"][0]
    assert res["rows"] == [[b"1", b"alice", b"t"], [b"2", b"bob", b"f"]]


def test_copy_from_stdin_null(client):
    msgs = _copy_in(client, "COPY t (id, name) FROM STDIN", b"1\t\\N\n")
    assert _tag(msgs) == "COPY 1"
    from test_pgserver import parse_results

    res = parse_results(client.query("SELECT name FROM t"))["results"][0]
    assert res["rows"] == [[None]]


def test_copy_from_stdin_csv(client):
    msgs = _copy_in(
        client, "COPY t (id, name, active) FROM STDIN WITH CSV", b"1,alice,t\r\n2,bob,f\r\n"
    )
    assert _tag(msgs) == "COPY 2"
    from test_pgserver import parse_results

    res = parse_results(client.query("SELECT id, name FROM t ORDER BY id"))["results"][0]
    assert res["rows"] == [[b"1", b"alice"], [b"2", b"bob"]]


def test_copy_to_stdout_text(client):
    client.query("INSERT INTO t (id, name, active) VALUES (1, 'alice', true), (2, 'bob', false)")
    client.send(pgwire.build_query("COPY t (id, name, active) TO STDOUT"))
    h = client.read_message()
    assert h.type == "H"  # CopyOutResponse
    data = bytearray()
    while True:
        m = client.read_message()
        if m.type == "d":
            data += m.payload
        elif m.type == "c":  # CopyDone
            break
    rest = client.read_until_ready()
    assert _tag(rest) == "COPY 2"
    assert data.decode() == "1\talice\tt\n2\tbob\tf\n"


def test_copy_to_stdout_csv_with_header(client):
    client.query("INSERT INTO t (id, name, active) VALUES (1, 'alice', true)")
    client.send(pgwire.build_query("COPY t (id, name) TO STDOUT WITH CSV HEADER"))
    assert client.read_message().type == "H"
    data = bytearray()
    while True:
        m = client.read_message()
        if m.type == "d":
            data += m.payload
        elif m.type == "c":
            break
    client.read_until_ready()
    assert data.decode() == "id,name\n1,alice\n"


def test_copy_roundtrip(client):
    """COPY out then back in reproduces the same rows."""
    client.query("INSERT INTO t (id, name, active) VALUES (7, 'x', true)")
    client.send(pgwire.build_query("COPY t (id, name, active) TO STDOUT"))
    assert client.read_message().type == "H"
    data = bytearray()
    while True:
        m = client.read_message()
        if m.type == "d":
            data += m.payload
        elif m.type == "c":
            break
    client.read_until_ready()
    client.query("DELETE FROM t")
    msgs = _copy_in(client, "COPY t (id, name, active) FROM STDIN", bytes(data))
    assert _tag(msgs) == "COPY 1"
    from test_pgserver import parse_results

    res = parse_results(client.query("SELECT id, name, active FROM t"))["results"][0]
    assert res["rows"] == [[b"7", b"x", b"t"]]


def test_copy_from_missing_table_errors(client):
    client.send(pgwire.build_query("COPY nope FROM STDIN"))
    m = client.read_message()
    # The table doesn't exist, so the server errors before CopyInResponse.
    assert m.type == "E"
    err = pgwire.parse_error_response(m.payload)
    assert err["C"] == "42P01"
    client.read_until_ready()
