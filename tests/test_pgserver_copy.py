"""``COPY … FROM/TO STDIN/STDOUT`` over the wire.

Drives the CopyIn / CopyOut sub-protocol against a real ``SecantusPGServer`` over
a loopback socket (backed by the real WT-backed ``Storage``). The
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
from secantus.storage import Storage


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
def server(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        st.close()


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


def _copy_out(client: PGClient, sql: str) -> bytes:
    """Drive a COPY … TO STDOUT and return the concatenated CopyData payloads."""
    client.send(pgwire.build_query(sql))
    assert client.read_message().type == "H"  # CopyOutResponse
    data = bytearray()
    while True:
        m = client.read_message()
        if m.type == "d":
            data += m.payload
        elif m.type == "c":  # CopyDone
            break
    client.read_until_ready()
    return bytes(data)


def test_copy_query_to_stdout_text(client):
    client.query("INSERT INTO t (id, name, active) VALUES (1, 'alice', true), (2, 'bob', false)")
    data = _copy_out(client, "COPY (SELECT id, name FROM t WHERE active ORDER BY id) TO STDOUT")
    assert data.decode() == "1\talice\n"


def test_copy_query_to_stdout_csv_header(client):
    client.query("INSERT INTO t (id, name, active) VALUES (1, 'alice', true), (2, 'bob', false)")
    data = _copy_out(
        client,
        "COPY (SELECT id, name FROM t ORDER BY id) TO STDOUT WITH CSV HEADER",
    )
    # The header uses the query's output column names.
    assert data.decode() == "id,name\n1,alice\n2,bob\n"


def test_copy_query_aggregate_to_stdout(client):
    client.query(
        "INSERT INTO t (id, name, active) VALUES (1, 'a', true), (2, 'a', true), (3, 'b', false)"
    )
    data = _copy_out(
        client,
        "COPY (SELECT name, count(*) AS c FROM t GROUP BY name ORDER BY name) TO STDOUT",
    )
    assert data.decode() == "a\t2\nb\t1\n"


def test_copy_query_from_stdin_rejected(client):
    # COPY (query) FROM is a syntax error — you can't load into a query.
    client.send(pgwire.build_query("COPY (SELECT 1) FROM STDIN"))
    m = client.read_message()
    assert m.type == "E"
    assert pgwire.parse_error_response(m.payload)["C"] == "42601"
    client.read_until_ready()


# --------------------------------------------------------------------------- #
# Copy frames arriving outside a COPY operation are discarded (pgx streams
# CopyData concurrently with the COPY command, so the frames land after the
# command already failed). Real PG accepts and ignores stray CopyData /
# CopyDone / CopyFail per the protocol spec; routing them into the extended
# protocol raised 08P01 and poisoned the connection.


def test_copy_data_after_failed_copy_is_discarded(client):
    # pgx's CopyFrom shape: the command and the data are pumped without
    # waiting for CopyInResponse. The COPY fails (42P01), then the stray
    # frames must be dropped and the connection stay usable.
    client.send(pgwire.build_query("COPY nosuchtable FROM STDIN"))
    client.send(pgwire.copy_data(b"id\t0\n"))
    client.send(pgwire.copy_done())
    msgs = client.read_until_ready()
    errs = [m for m in msgs if m.type == "E"]
    assert len(errs) == 1
    assert pgwire.parse_error_response(errs[0].payload)["C"] == "42P01"
    tags = [m.type for m in client.query("SELECT 1")]
    assert "D" in tags and tags[-1] == "Z"


def test_copy_data_after_syntax_error_is_discarded(client):
    # The pgx TestConnCopyFromQuerySyntaxError shape: the "COPY" command is
    # not even SQL, and 1000 rows are streamed regardless.
    client.send(pgwire.build_query("cropy t FROM STDIN WITH (FORMAT csv)"))
    for i in range(1000):
        client.send(pgwire.copy_data(f'{i},"foo {i} bar"\n'.encode()))
    client.send(pgwire.copy_done())
    msgs = client.read_until_ready()
    errs = [m for m in msgs if m.type == "E"]
    assert len(errs) == 1
    assert pgwire.parse_error_response(errs[0].payload)["C"] == "42601"
    tags = [m.type for m in client.query("SELECT 1")]
    assert "D" in tags and tags[-1] == "Z"


def test_stray_copy_fail_is_discarded(client):
    client.send(pgwire.build_query("COPY nosuchtable FROM STDIN"))
    client.send(pgwire.copy_fail("client gave up"))
    msgs = client.read_until_ready()
    assert [m.type for m in msgs if m.type == "E"] == ["E"]
    tags = [m.type for m in client.query("SELECT 1")]
    assert "D" in tags and tags[-1] == "Z"


def test_failed_copy_then_valid_copy_succeeds(client):
    # The pgx TestConnCopyFromDataWriteAfterErrorAndReturn shape: a failed
    # COPY (with data still streaming in) followed by a valid COPY on the
    # same connection.
    client.send(pgwire.build_query("COPY nosuchtable FROM STDIN"))
    client.send(pgwire.copy_data(b"id\t0\n"))
    client.send(pgwire.copy_done())
    client.read_until_ready()
    msgs = _copy_in(client, "COPY t FROM STDIN", b"7\tcarol\tt\n")
    assert _tag(msgs) == "COPY 1"
    rows = [m for m in client.query("SELECT id, name FROM t WHERE id = 7") if m.type == "D"]
    assert len(rows) == 1
