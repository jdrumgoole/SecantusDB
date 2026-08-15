"""P3 tests: the extended query protocol (Parse/Bind/Describe/Execute/Sync).

A pure-Python client drives prepared statements + bound ``$1`` parameters over a
real ``SecantusPGServer`` (psycopg uses this protocol, but needs libpq and isn't
importable here; the wire exchange is exercised directly instead).
"""

from __future__ import annotations

import socket

import pytest

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage


class ExtClient:
    def __init__(self, host: str, port: int) -> None:
        self.sock = socket.create_connection((host, port), timeout=5)

    def _read_until_ready(self) -> list[pgwire.Message]:
        msgs: list[pgwire.Message] = []
        while True:
            m = pgwire.read_message(self.sock)
            msgs.append(m)
            if m.type == "Z":
                return msgs

    def startup(self, database: str = "testdb") -> None:
        self.sock.sendall(pgwire.build_startup_message({"user": "joe", "database": database}))
        self._read_until_ready()

    def exchange(self, *messages: bytes) -> list[pgwire.Message]:
        """Send a batch of extended messages + Sync; read up to ReadyForQuery."""
        self.sock.sendall(b"".join(messages) + pgwire.build_sync())
        return self._read_until_ready()

    def close(self) -> None:
        self.sock.close()


def types(msgs: list[pgwire.Message]) -> list[str]:
    return [m.type for m in msgs]


def rows(msgs: list[pgwire.Message]) -> list[list[bytes | None]]:
    return [pgwire.parse_data_row(m.payload) for m in msgs if m.type == "D"]


def row_description(msgs: list[pgwire.Message]) -> list[str] | None:
    for m in msgs:
        if m.type == "T":
            return pgwire.parse_row_description(m.payload)
    return None


def error_code(msgs: list[pgwire.Message]) -> str | None:
    for m in msgs:
        if m.type == "E":
            return pgwire.parse_error_response(m.payload).get("C")
    return None


def command_tag(msgs: list[pgwire.Message]) -> str | None:
    for m in msgs:
        if m.type == "C":
            return pgwire.parse_command_complete(m.payload)
    return None


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
    c = ExtClient(host, port)
    c.startup()
    try:
        yield c
    finally:
        c.close()


def _create_users(client):
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE users (id bigint primary key, name text, age int)"),
        pgwire.build_bind("", "", []),
        pgwire.build_execute(),
    )


# --------------------------------------------------------------------------- #


def test_parse_bind_execute_insert_with_params(client):
    _create_users(client)
    msgs = client.exchange(
        pgwire.build_parse("ins", "INSERT INTO users (id, name, age) VALUES ($1, $2, $3)"),
        pgwire.build_bind("p", "ins", [b"1", b"alice", b"30"]),
        pgwire.build_describe("P", "p"),
        pgwire.build_execute("p"),
    )
    # ParseComplete, BindComplete, NoData (INSERT has no result rows), CommandComplete.
    assert types(msgs) == ["1", "2", "n", "C", "Z"]
    assert command_tag(msgs) == "INSERT 0 1"


def test_select_with_param_returns_typed_rows(client):
    _create_users(client)
    client.exchange(
        pgwire.build_parse("ins", "INSERT INTO users (id, name, age) VALUES ($1, $2, $3)"),
        pgwire.build_bind("p", "ins", [b"1", b"alice", b"30"]),
        pgwire.build_execute("p"),
    )
    msgs = client.exchange(
        pgwire.build_parse("sel", "SELECT id, name FROM users WHERE age > $1 ORDER BY id"),
        pgwire.build_bind("p", "sel", [b"18"]),
        pgwire.build_describe("P", "p"),
        pgwire.build_execute("p"),
    )
    assert row_description(msgs) == ["id", "name"]
    assert rows(msgs) == [[b"1", b"alice"]]
    assert command_tag(msgs) == "SELECT 1"


def test_prepared_statement_reused_with_different_params(client):
    _create_users(client)
    ins = pgwire.build_parse("ins", "INSERT INTO users (id, name, age) VALUES ($1, $2, $3)")
    client.exchange(
        ins, pgwire.build_bind("p", "ins", [b"1", b"alice", b"30"]), pgwire.build_execute("p")
    )
    # Re-bind the SAME prepared statement with new params — no re-Parse.
    msgs = client.exchange(
        pgwire.build_bind("p", "ins", [b"2", b"bob", b"17"]),
        pgwire.build_execute("p"),
    )
    assert types(msgs) == ["2", "C", "Z"]  # BindComplete, CommandComplete
    got = client.exchange(
        pgwire.build_parse("all", "SELECT COUNT(*) FROM users"),
        pgwire.build_bind("c", "all", []),
        pgwire.build_describe("P", "c"),
        pgwire.build_execute("c"),
    )
    assert rows(got) == [[b"2"]]


def test_describe_statement_reports_params_and_columns(client):
    _create_users(client)
    msgs = client.exchange(
        pgwire.build_parse("sel", "SELECT id, name FROM users WHERE age > $1"),
        pgwire.build_describe("S", "sel"),
    )
    pd = next(m for m in msgs if m.type == "t")
    # An undeclared parameter resolves to text (25), matching Postgres' parse
    # analysis — clients re-dump their parameters per this reply, and echoing
    # 0 back left binary unknown-type params undecodable.
    assert pgwire.parse_parameter_description(pd.payload) == [25]
    assert row_description(msgs) == ["id", "name"]


def test_null_parameter_binds_to_sql_null(client):
    _create_users(client)
    client.exchange(
        pgwire.build_parse("ins", "INSERT INTO users (id, name, age) VALUES ($1, $2, $3)"),
        pgwire.build_bind("p", "ins", [b"1", None, b"5"]),
        pgwire.build_execute("p"),
    )
    msgs = client.exchange(
        pgwire.build_parse("sel", "SELECT id FROM users WHERE name IS NULL"),
        pgwire.build_bind("p", "sel", []),
        pgwire.build_describe("P", "p"),
        pgwire.build_execute("p"),
    )
    assert rows(msgs) == [[b"1"]]


def test_error_skips_until_sync_then_recovers(client):
    msgs = client.exchange(
        pgwire.build_parse("bad", "SELECT * FROM does_not_exist"),
        pgwire.build_bind("p", "bad", []),
        pgwire.build_execute("p"),
    )
    assert error_code(msgs) == "42P01"
    assert msgs[-1].type == "Z"  # Sync still produces ReadyForQuery
    # The connection survives — a fresh prepared statement works.
    ok = client.exchange(
        pgwire.build_parse("s", "SELECT 7"),
        pgwire.build_bind("p", "s", []),
        pgwire.build_describe("P", "p"),
        pgwire.build_execute("p"),
    )
    assert rows(ok) == [[b"7"]]


def test_execute_with_max_rows_suspends_portal(client):
    _create_users(client)
    for i in (1, 2, 3):
        client.exchange(
            pgwire.build_parse("ins", "INSERT INTO users (id, name, age) VALUES ($1, $2, $3)"),
            pgwire.build_bind("p", "ins", [str(i).encode(), b"x", b"20"]),
            pgwire.build_execute("p"),
        )
    # Bind a SELECT portal, then Execute it in two slices via max_rows.
    msgs = client.exchange(
        pgwire.build_parse("sel", "SELECT id FROM users ORDER BY id"),
        pgwire.build_bind("pg", "sel", []),
        pgwire.build_describe("P", "pg"),
        pgwire.build_execute("pg", max_rows=2),
    )
    assert rows(msgs) == [[b"1"], [b"2"]]
    assert any(m.type == "s" for m in msgs)  # PortalSuspended
    # A second Execute on the same portal drains the rest.
    more = client.exchange(pgwire.build_execute("pg", max_rows=2))
    assert rows(more) == [[b"3"]]
    assert command_tag(more) == "SELECT 3"


def test_bind_to_closed_statement_errors(client):
    _create_users(client)
    client.exchange(
        pgwire.build_parse("s", "SELECT 1"),
        pgwire.build_close("S", "s"),
    )
    msgs = client.exchange(pgwire.build_bind("p", "s", []))
    assert error_code(msgs) == "26000"


def test_empty_query_in_extended_protocol(client):
    msgs = client.exchange(
        pgwire.build_parse("", ""),
        pgwire.build_bind("", "", []),
        pgwire.build_execute(),
    )
    assert any(m.type == "I" for m in msgs)  # EmptyQueryResponse


# --------------------------------------------------------------------------- #
# 16-bit count fields are unsigned
# --------------------------------------------------------------------------- #


def test_bind_codec_roundtrips_more_than_32767_params():
    """Postgres allows up to 65535 parameters per Bind. Read as *signed*, a count
    above 32767 comes back negative and walks the parse offset backwards
    ("not enough data to unpack 4 bytes at offset -2")."""
    n = 40000
    values: list[bytes | None] = [str(i).encode() for i in range(n)]
    portal, stmt, _fmts, parsed, _res = pgwire.parse_bind(pgwire.build_bind("p", "s", values)[5:])
    assert (portal, stmt) == ("p", "s")
    assert parsed == values


def test_parameter_description_codec_roundtrips_more_than_32767_oids():
    n = 40000
    payload = pgwire.parameter_description([25] * n)[5:]
    assert pgwire.parse_parameter_description(payload) == [25] * n


def test_extended_protocol_with_more_than_32767_params(client):
    """End to end: pgjdbc's rewritten batch inserts bind tens of thousands of
    parameters in one Bind, which used to crash the connection outright."""
    n = 40000
    sql = "SELECT coalesce(" + ",".join(f"${i + 1}" for i in range(n)) + ")"
    # The 5s default is a wire round-trip timeout, not a budget for how long the
    # server may legitimately think. Planning 40k placeholders is sub-second on a
    # dev machine but several seconds on a 2-core CI runner, where the default
    # timed this out.
    client.sock.settimeout(120)
    msgs = client.exchange(
        pgwire.build_parse("big", sql),
        pgwire.build_describe("S", "big"),
        pgwire.build_bind("p", "big", [None] * (n - 1) + [b"tail"]),
        pgwire.build_execute("p"),
    )
    assert "E" not in types(msgs)
    pd = next(m for m in msgs if m.type == "t")
    assert pgwire.parse_parameter_description(pd.payload) == [25] * n
    assert rows(msgs) == [[b"tail"]]


# --------------------------------------------------------------------------- #
# Cached-plan revalidation (PG's "cached plan must not change result type")
# --------------------------------------------------------------------------- #


def test_named_statement_shape_change_raises_0a000(server):
    psycopg = pytest.importorskip("psycopg")
    host, port = server.address
    with psycopg.connect(
        host=host, port=port, dbname="db", user="joe", autocommit=True, prepare_threshold=0
    ) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE cp (a int)")
        cur.execute("INSERT INTO cp VALUES (1)")
        cur.execute("SELECT * FROM cp", prepare=True)
        assert cur.fetchall() == [(1,)]
        cur.execute("ALTER TABLE cp ADD COLUMN b int")
        with pytest.raises(psycopg.errors.FeatureNotSupported) as e:
            cur.execute("SELECT * FROM cp", prepare=True)
        # pgjdbc's transparent re-prepare matches on the ROUTINE field, not
        # the SQLSTATE — it must be present and spelled exactly.
        assert e.value.diag.source_function == "RevalidateCachedQuery"
        assert "cached plan must not change result type" in str(e.value)


def test_unnamed_statement_replans_silently(server):
    psycopg = pytest.importorskip("psycopg")
    host, port = server.address
    with psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE cp2 (a int)")
        cur.execute("INSERT INTO cp2 VALUES (1)")
        # prepare=False → the unnamed statement: PG re-plans per Bind and
        # never raises the cached-plan error.
        cur.execute("SELECT * FROM cp2", prepare=False)
        cur.execute("ALTER TABLE cp2 ADD COLUMN b int")
        cur.execute("SELECT * FROM cp2", prepare=False)
        assert cur.fetchall() == [(1, None)]


def test_revalidation_raises_before_cte_side_effects(server):
    psycopg = pytest.importorskip("psycopg")
    host, port = server.address
    with psycopg.connect(
        host=host, port=port, dbname="db", user="joe", autocommit=True, prepare_threshold=0
    ) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE cp3 (a int)")
        cur.execute("INSERT INTO cp3 VALUES (1)")
        sql = "WITH ins AS (INSERT INTO cp3 (a) VALUES (%s) RETURNING a) SELECT * FROM cp3"
        cur.execute(sql, (99,), prepare=True)
        cur.execute("ALTER TABLE cp3 ADD COLUMN b int")
        with pytest.raises(psycopg.errors.FeatureNotSupported):
            cur.execute(sql, (100,), prepare=True)
        # The revalidation happens at planning time — the CTE's INSERT must
        # NOT have run (PG parity; pgjdbc's AutoRollback matrix counts rows).
        cur.execute("SELECT count(*) FROM cp3")
        assert cur.fetchall() == [(2,)]


# --------------------------------------------------------------------------- #
# Implicit transaction across a pipeline (statements before one Sync)
# --------------------------------------------------------------------------- #


@pytest.mark.skipif(
    __import__("os").environ.get("SECANTUS_PIPELINE_TXN", "0") == "0",
    reason="pipeline implicit txn gated off (SECANTUS_PIPELINE_TXN)",
)
def test_pipeline_error_rolls_back_earlier_statements(server):
    psycopg = pytest.importorskip("psycopg")
    host, port = server.address
    with psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE bu (id text PRIMARY KEY)")
        cur.execute("INSERT INTO bu VALUES ('key-2')")
        # One pipeline, one Sync: PG runs the statements in ONE implicit
        # transaction, so the mid-pipeline dup key discards key-1 too
        # (pgjdbc's BatchFailureTest counts exactly this).
        with pytest.raises(psycopg.errors.UniqueViolation), conn.pipeline():
            c = conn.cursor()
            c.execute("INSERT INTO bu VALUES ('key-1')")
            c.execute("INSERT INTO bu VALUES ('key-2')")
            c.execute("INSERT INTO bu VALUES ('key-3')")
        cur.execute("SELECT id FROM bu ORDER BY id")
        assert cur.fetchall() == [("key-2",)]
        # Autocommit single statements are unaffected.
        cur.execute("INSERT INTO bu VALUES ('key-9')")
        cur.execute("SELECT count(*) FROM bu")
        assert cur.fetchall() == [(2,)]


@pytest.mark.skipif(
    __import__("os").environ.get("SECANTUS_PIPELINE_TXN", "0") == "0",
    reason="pipeline implicit txn gated off (SECANTUS_PIPELINE_TXN)",
)
def test_pipeline_success_commits_at_sync(server):
    psycopg = pytest.importorskip("psycopg")
    host, port = server.address
    with psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE bu2 (id int)")
        with conn.pipeline():
            c = conn.cursor()
            c.execute("INSERT INTO bu2 VALUES (1)")
            c.execute("INSERT INTO bu2 VALUES (2)")
        cur.execute("SELECT count(*) FROM bu2")
        assert cur.fetchall() == [(2,)]


# --------------------------------------------------------------------------- #
# Describe over derived-VALUES joins (pgjdbc's {oj} shapes)
# --------------------------------------------------------------------------- #


def test_describe_derived_values_join(server):
    psycopg = pytest.importorskip("psycopg")
    host, port = server.address
    with psycopg.connect(
        host=host, port=port, dbname="db", user="joe", autocommit=True, prepare_threshold=0
    ) as conn:
        cur = conn.cursor()
        sql = (
            "select t1.id as t1_id, t2.text as t2_text"
            " from (values (1, 'one'), (2, 'two')) as t1 (id, text)"
            " left outer join (values (1, 'a'), (3, 'b')) as t2 (id, text)"
            " on (t1.id = t2.id)"
        )
        cur.execute(sql, prepare=True)
        assert [d.name for d in cur.description] == ["t1_id", "t2_text"]
        assert cur.fetchall() == [(1, "a"), (2, None)]


def test_parameter_description_wraps_int16_count_like_pg():
    # PG's ParameterDescription count field is int16 and WRAPS for >=65536
    # parameters (pq_sendint16); pgproto3 ignores the count and infers it
    # from the message length. 65536 oids must not raise struct.error, and
    # every oid must be present in the body.
    from secantus.sql import pgwire

    msg = pgwire.parameter_description([25] * 65536)
    # 1 type byte + int32 length + int16 (wrapped to 0) + 65536 * int32 oids
    assert len(msg) == 1 + 4 + 2 + 65536 * 4
    assert msg[5:7] == b"\x00\x00"  # 65536 & 0xFFFF == 0
