"""End-to-end tests for the PostgreSQL-wire server (P1).

A pure-Python PG v3 client drives a real ``SecantusPGServer`` over a loopback
socket — the deterministic "a real client connected and got rows" proof. The
server runs over an injected real WT-backed ``Storage``; the wire framing and
handshake are exercised for real.
"""

from __future__ import annotations

import contextlib
import logging
import socket
import struct

import pytest

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage


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


def test_startup_reports_interval_style(server):
    """``IntervalStyle`` is in the startup set, like real postgres.

    It's one of postgres's ``GUC_REPORT`` parameters, and a client that
    decodes intervals itself reads the style from here rather than
    assuming one. psycopg's pure-Python backend does exactly that: with
    the parameter absent it sees a style of ``unknown`` and raises
    ``NotImplementedError`` on any ``interval`` result, while its C
    backend papers over the gap because libpq tracks the value
    internally. Asserted at the wire level so the guarantee doesn't
    depend on which psycopg implementation happens to be installed —
    the binary backend is what's pinned, and it would not catch a
    regression here.
    """
    host, port = server.address
    c = PGClient(host, port)
    try:
        params = {}
        for m in c.startup():
            if m.type == "S":
                name, value, _ = m.payload.split(b"\x00", 2)
                params[name.decode()] = value.decode()
        assert params["IntervalStyle"] == "postgres"
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


def test_connection_cap_rejects_over_limit(tmp_path):
    """Over the max_connections cap, an accepted socket is closed immediately
    rather than served (issue #194)."""
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st, max_connections=1)
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
        st.close()


def test_stop_drains_handler_threads_before_storage_close(tmp_path, caplog):
    # A client abandoned mid-transaction (no Terminate, socket left open) must
    # not leave its handler thread using a WT session while the embedder runs
    # ``stop()`` + ``storage.close()`` — that concurrent close corrupts the WT
    # session handle ("WT session close failed during close" in the log).
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    host, port = srv.address
    c = PGClient(host, port)
    c.startup()
    c.query("CREATE TABLE t (id bigint primary key, n int)")
    c.query("BEGIN")
    c.query("INSERT INTO t (id, n) VALUES (1, 10)")  # txn left open, socket abandoned
    with caplog.at_level(logging.ERROR, logger="secantus.storage.close"):
        srv.stop()
        assert not [t for t in srv._handler_threads if t.is_alive()]
        st.close()
    assert not [r for r in caplog.records if "close failed" in r.message]
    c.sock.close()


# --------------------------------------------------------------------------- #
# Statement-shaped garbage is a syntax error, and a mid-batch error still
# streams the earlier statements' results — both PG parse/exec error shapes
# pinned by pgx (PrepareSyntaxError / PipelinePrepareError /
# ExecMultipleQueriesError).


def test_bare_expression_is_a_syntax_error(client):
    # sqlglot parses "SYNTAX ERROR" as an aliased column expression; a bare
    # expression is not a statement and real PG rejects it with 42601.
    msgs = client.query("SYNTAX ERROR")
    errs = [m for m in msgs if m.type == "E"]
    assert len(errs) == 1
    assert pgwire.parse_error_response(errs[0].payload)["C"] == "42601"
    res = parse_results(client.query("SELECT 1"))
    assert res["results"][0]["rows"] == [[b"1"]]


def test_multi_statement_error_streams_completed_results(client):
    # select 1 runs and its rows arrive; select 1/0 errors 22012; the third
    # statement never executes — exactly real PG's simple-protocol shape.
    msgs = client.query("select 1; select 1/0; select 2")
    types = [m.type for m in msgs]
    data = [m for m in msgs if m.type == "D"]
    errs = [m for m in msgs if m.type == "E"]
    assert len(data) == 1 and pgwire.parse_data_row(data[0].payload) == [b"1"]
    assert len(errs) == 1
    assert pgwire.parse_error_response(errs[0].payload)["C"] == "22012"
    # The first statement completed (its CommandComplete precedes the error);
    # nothing from the third statement follows the ErrorResponse.
    assert types.index("C") < types.index("E")
    assert types[-1] == "Z"


# --------------------------------------------------------------------------- #
# Protocol negotiation: a client asking for a newer minor protocol (pgx's
# MaxProtocolVersion "3.2" sends 196610) gets NegotiateProtocolVersion FIRST
# — the newest minor we speak plus any unrecognized _pq_.* options — and the
# handshake continues at 3.0, exactly like real PG.


def _startup_with_protocol(server, protocol, extra=None):
    import struct as _struct

    host, port = server.address
    c = PGClient(host, port)
    params = {"user": "secantus", "database": "testdb", **(extra or {})}
    c.sock.sendall(pgwire.build_startup_message(params, protocol=protocol))
    first = pgwire.read_message(c.sock)
    assert first.type == "v", f"expected NegotiateProtocolVersion, got {first.type}"
    (newest,) = _struct.unpack_from("!i", first.payload, 0)
    (count,) = _struct.unpack_from("!i", first.payload, 4)
    names = first.payload[8:].split(b"\x00")[:-1] if count else []
    c._read_until_ready()
    return c, newest, count, [n.decode() for n in names]


def test_protocol_32_negotiates_down_to_30(server):
    c, newest, count, names = _startup_with_protocol(server, (3 << 16) | 2)
    try:
        assert newest == 196608 and count == 0 and names == []
        res = parse_results(c.query("SELECT 1"))
        assert res["results"][0]["rows"] == [[b"1"]]
    finally:
        c.close()


def test_unknown_pq_option_is_reported(server):
    c, newest, count, names = _startup_with_protocol(
        server, (3 << 16) | 2, extra={"_pq_.fancy_feature": "on"}
    )
    try:
        assert newest == 196608
        assert names == ["_pq_.fancy_feature"]
    finally:
        c.close()


def test_protocol_30_gets_no_negotiation(client):
    # The plain-3.0 handshake shape is pinned by every other test in this
    # file; just confirm a fresh query round-trip stays clean.
    res = parse_results(client.query("SELECT 1"))
    assert res["results"][0]["rows"] == [[b"1"]]


def test_show_server_version_num(client):
    res = parse_results(client.query("SHOW server_version_num"))
    assert res["results"][0]["rows"] == [[b"150000"]]


def test_startup_parameter_applies_any_guc(server):
    # Real PG accepts any run-time GUC as a startup parameter. pgx's
    # target_session_attrs=read-write probe ships
    # default_transaction_read_only=on at startup and expects SHOW
    # transaction_read_only to reflect it (and writes to fail 25006).
    host, port = server.address
    c = PGClient(host, port)
    c.sock.sendall(
        pgwire.build_startup_message(
            {
                "user": "secantus",
                "database": "testdb",
                "default_transaction_read_only": "on",
            }
        )
    )
    c._read_until_ready()
    try:
        res = parse_results(c.query("SHOW transaction_read_only"))
        assert res["results"][0]["rows"] == [[b"on"]]
        msgs = c.query("CREATE TABLE ro_probe (a int4)")
        errs = [m for m in msgs if m.type == "E"]
        assert errs and pgwire.parse_error_response(errs[0].payload)["C"] == "25006"
    finally:
        c.close()


# --------------------------------------------------------------------------- #
# A multi-statement simple query is ONE implicit transaction (real PG; the
# pgtest batch_stmt corpus pins every shape below): a mid-batch error rolls
# back the earlier statements' writes; BEGIN inside the batch takes the
# transaction over (its characteristics included); COMMIT ends it and the
# remainder starts a fresh implicit transaction.


def test_batch_error_rolls_back_earlier_writes(client):
    client.query("CREATE TABLE batch_a (n int4)")
    msgs = client.query("INSERT INTO batch_a VALUES(1); SELECT 1/0; INSERT INTO batch_a VALUES(2);")
    tags = [pgwire.parse_command_complete(m.payload) for m in msgs if m.type == "C"]
    assert tags == ["INSERT 0 1"]  # first statement's result streamed
    errs = [m for m in msgs if m.type == "E"]
    assert pgwire.parse_error_response(errs[0].payload)["C"] == "22012"
    res = parse_results(client.query("SELECT count(*) FROM batch_a"))
    assert res["results"][0]["rows"] == [[b"0"]]  # rolled back


def test_commit_in_batch_splits_transactions(client):
    client.query("CREATE TABLE batch_b (n int4)")
    client.query("INSERT INTO batch_b VALUES(1); COMMIT; SELECT 1/0;")
    res = parse_results(client.query("SELECT count(*) FROM batch_b"))
    assert res["results"][0]["rows"] == [[b"1"]]  # committed before the error


def test_begin_read_only_takeover_in_batch(client):
    client.query("CREATE TABLE batch_c (n int4)")
    msgs = client.query(
        "INSERT INTO batch_c VALUES(6); BEGIN READ ONLY; INSERT INTO batch_c VALUES(7); COMMIT;"
    )
    errs = [m for m in msgs if m.type == "E"]
    assert pgwire.parse_error_response(errs[0].payload)["C"] == "25006"
    assert msgs[-1].payload == b"E"  # ReadyForQuery: failed transaction block
    client.query("COMMIT")  # ends the failed block (rolls back)
    res = parse_results(client.query("SELECT count(*) FROM batch_c"))
    assert res["results"][0]["rows"] == [[b"0"]]  # INSERT 6 rolled back too
