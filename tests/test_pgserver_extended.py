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
    # ``age > $1`` types the parameter from the COLUMN (int4 = 23), like PG's
    # parse analysis. A parameter with no such context still resolves to text
    # (25) rather than 0 — clients re-dump their parameters per this reply,
    # and echoing 0 back left binary unknown-type params undecodable.
    assert pgwire.parse_parameter_description(pd.payload) == [23]
    assert row_description(msgs) == ["id", "name"]
    # No column context (the parameter feeds a concatenation) → text.
    nc = client.exchange(
        pgwire.build_parse("nc", "SELECT id FROM users WHERE name = $1 || 'x'"),
        pgwire.build_describe("S", "nc"),
    )
    pd = next(m for m in nc if m.type == "t")
    assert pgwire.parse_parameter_description(pd.payload) == [25]


def test_unknown_param_oids_resolve_from_target_columns(client):
    # A client that declares its parameters as ``unknown`` (oid 705) — as some
    # drivers do — gets them resolved from the INSERT target columns, like PG's
    # parse analysis, not echoed back as 705 (pgtest ``unknown``).
    _create_users(client)
    msgs = client.exchange(
        pgwire.build_parse(
            "u", "INSERT INTO users VALUES ($1, $2, $3)", param_oids=[705, 705, 705]
        ),
        pgwire.build_describe("S", "u"),
    )
    pd = next(m for m in msgs if m.type == "t")
    # id bigint (20), name text (25), age int4 (23).
    assert pgwire.parse_parameter_description(pd.payload) == [20, 25, 23]


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
    # PG portals live until their transaction ends, so resuming across a
    # Sync needs an explicit block (outside one the portal dies at Sync and
    # a later Execute is 34000 — pgtest multiple_active_portals).
    client.exchange(
        pgwire.build_parse("", "BEGIN"),
        pgwire.build_bind("", "", []),
        pgwire.build_execute(""),
    )
    msgs = client.exchange(
        pgwire.build_parse("sel", "SELECT id FROM users ORDER BY id"),
        pgwire.build_bind("pg", "sel", []),
        pgwire.build_describe("P", "pg"),
        pgwire.build_execute("pg", max_rows=2),
    )
    assert rows(msgs) == [[b"1"], [b"2"]]
    assert any(m.type == "s" for m in msgs)  # PortalSuspended
    # A second Execute on the same portal drains the rest. Its
    # CommandComplete counts the rows THAT Execute delivered (one), not the
    # portal's total — PG's per-Execute count, pinned by pgtest portals.
    more = client.exchange(pgwire.build_execute("pg", max_rows=2))
    assert rows(more) == [[b"3"]]
    assert command_tag(more) == "SELECT 1"
    client.exchange(
        pgwire.build_parse("", "COMMIT"),
        pgwire.build_bind("", "", []),
        pgwire.build_execute(""),
    )
    # Outside a block the portal is gone after Sync.
    dead = client.exchange(pgwire.build_execute("pg", max_rows=2))
    assert error_code(dead) == "34000"


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


# --------------------------------------------------------------------------- #
# Aborted-transaction pipeline semantics (the pgtest aborted_txn corpus):
# extended-protocol steps in an ABORTED explicit transaction fail 25P02 —
# except the transaction-exit statements (COMMIT/ROLLBACK), PG's
# IsTransactionExitStmt carve-out — and an errored pipeline discards even
# interleaved simple Query messages until Sync (PG's ignore_till_sync).


def _abort_txn(c):
    c.exchange(
        pgwire.build_parse("", "BEGIN", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = c.exchange(
        pgwire.build_parse("", "SELECT 1/0", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) is not None


def test_parse_in_aborted_txn_is_25P02(client):
    _abort_txn(client)
    msgs = client.exchange(pgwire.build_parse("s1", "SELECT 1", []))
    assert error_code(msgs) == "25P02"
    client.exchange(
        pgwire.build_parse("", "ROLLBACK", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    ok = client.exchange(
        pgwire.build_parse("", "SELECT 1", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert rows(ok) == [[b"1"]]


def test_parse_rollback_allowed_in_aborted_txn(client):
    _abort_txn(client)
    msgs = client.exchange(
        pgwire.build_parse("rb", "ROLLBACK", []),
        pgwire.build_bind("", "rb", []),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) is None
    ok = client.exchange(
        pgwire.build_parse("", "SELECT 1", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert rows(ok) == [[b"1"]]


def test_errored_pipeline_discards_interleaved_simple_query(client):
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE uq_pipe (n int unique)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("", "INSERT INTO uq_pipe VALUES (1)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    # Duplicate insert errors mid-pipeline; the interleaved simple Query must
    # be discarded entirely (no DataRow, no extra ReadyForQuery) — PG's
    # ignore_till_sync, pinned by the pgtest aborted_txn corpus.
    client.sock.sendall(
        pgwire.build_parse("", "INSERT INTO uq_pipe VALUES (1)", [])
        + pgwire.build_bind("", "", [])
        + pgwire.build_execute("", 0)
        + pgwire.build_query("SELECT 99")
        + pgwire.build_sync()
    )
    msgs = client._read_until_ready()
    assert error_code(msgs) == "23505"
    tps = types(msgs)
    assert "D" not in tps
    assert tps.count("Z") == 1


def test_malformed_binary_array_param_is_08P01(client):
    # The pgtest array corpus shape: a binary INTERVAL[] parameter whose
    # header carries a bogus element oid and no element data. PG answers
    # 08P01 (insufficient data left in message); this leaked XX000.
    bogus = bytes.fromhex("0000000100000000010101010000000100000000")
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::INTERVAL[]", []),
        pgwire.build_bind("", "", [bogus], param_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "08P01"


def test_malformed_array_literal_cast_is_22P02(client):
    # The pgtest json_array corpus shape: an empty-string text parameter
    # cast to JSON[]. PG answers 22P02 (malformed array literal); this
    # leaked XX000.
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::JSON[]", []),
        pgwire.build_bind("", "", [b""]),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "22P02"


def test_multidim_array_keeps_base_type(client):
    # ARRAY[ARRAY[1], ARRAY[2]] is int4[] — PG has ONE array oid per element
    # type regardless of dimensionality (pgtest array:53 reads the binary
    # element oid; we typed nested constructors text[]).
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT ARRAY[ARRAY[1], ARRAY[2]]", []),
        pgwire.build_describe("S", ""),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    rd = next(m for m in msgs if m.type == "T")
    import struct as _s

    end = rd.payload.index(b"\x00", 2)
    typoid = _s.unpack_from("!i", rd.payload, end + 7)[0]
    assert typoid == 1007  # int4[]


def test_plain_json_array_keeps_199(client):
    # $1::JSON[] keeps plain-json identities: parameter oid 199, row oid 199
    # (the collapsed tag would report jsonb's 3807 — pgtest json_array:92).
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::JSON[]", []),
        pgwire.build_describe("S", ""),
        pgwire.build_bind("", "", [b'{"{}"}']),
        pgwire.build_execute("", 0),
    )
    pd = next(m for m in msgs if m.type == "t")
    import struct as _s

    assert _s.unpack_from("!i", pd.payload, 2)[0] == 199
    rd = next(m for m in msgs if m.type == "T")
    end = rd.payload.index(b"\x00", 2)
    assert _s.unpack_from("!i", rd.payload, end + 7)[0] == 199


def test_jsonb_binary_param_version_checks(client):
    # pgtest json corpus: an empty binary JSONB payload (no version byte) and
    # an unknown version byte are both rejected, never silently accepted.
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::JSONB", []),
        pgwire.build_bind("", "", [b""], param_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "08P01"
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::JSONB", []),
        pgwire.build_bind("", "", [b"\x02\x22\x22"], param_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "08P01"


def test_plain_json_echoes_verbatim(client):
    # PG's json preserves input bytes: SELECT $1::JSON round-trips the
    # client's own spacing (jsonb would normalise) — pgtest json:102.
    spaced = b'{"key": "val"}'
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::JSON", []),
        pgwire.build_bind("", "", [spaced]),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[spaced]]


def test_plain_json_array_elements_verbatim(client):
    # Array elements keep their text too (pgtest json_array:124).
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::JSON[]", []),
        pgwire.build_bind("", "", [b'{"{\\"a\\": {}}"}']),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b'{"{\\"a\\": {}}"}']]


def test_binary_array_wrong_known_elem_oid_is_42804(client):
    # A jsonb[] payload (elem oid 3802) bound as json[] (declared 199) is
    # PG's 42804 datatype mismatch (pgtest json_array:246)…
    payload = bytes.fromhex("000000000000000000000eda")
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::JSON[]", [199]),
        pgwire.build_bind("", "", [payload], param_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "42804"
    # …while a GARBAGE embedded oid stays the structural 08P01 (array:8).
    bogus = bytes.fromhex("0000000100000000010101010000000100000000")
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::INTERVAL[]", []),
        pgwire.build_bind("", "", [bogus], param_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "08P01"


def test_regclass_param_describes_as_oid_2205(client):
    # pgtest bind_and_resolve:29 — a $1::REGCLASS parameter must describe
    # as regclass (oid 2205), not fall through to text.
    msgs = client.exchange(
        pgwire.build_parse("s_reg", "SELECT $1::REGCLASS::INT8", []),
        pgwire.build_describe("S", "s_reg"),
    )
    pd = next(m for m in msgs if m.type == "t")
    import struct as _s

    assert _s.unpack_from("!i", pd.payload, 2)[0] == 2205


def test_portal_bound_in_txn_snapshots_before_later_ddl(client):
    # pgtest bind_and_resolve:132 — a portal bound inside an explicit txn
    # captures its snapshot at Bind: a later same-txn ALTER ... RENAME is
    # invisible, so the held portal still reads the old relname.
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE snapt (a int)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("", "BEGIN", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("s_snap", "SELECT relname FROM pg_class WHERE oid = $1::regclass", []),
        pgwire.build_bind("p_snap", "s_snap", [b"snapt"]),
    )
    client.exchange(
        pgwire.build_parse("", "ALTER TABLE snapt RENAME TO snapt2", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(pgwire.build_execute("p_snap", 0))
    assert rows(msgs) == [[b"snapt"]]
    client.exchange(
        pgwire.build_parse("", "COMMIT", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )


def test_failing_select_in_txn_errors_at_execute_not_bind(client):
    # The eager bind-time snapshot run must not surface errors at Bind — PG
    # reports execution errors at Execute, after BindComplete.
    client.exchange(
        pgwire.build_parse("", "BEGIN", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT 1/0", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    tps = types(msgs)
    assert "1" in tps and "2" in tps  # ParseComplete + BindComplete first
    assert error_code(msgs) == "22012"
    # The block is poisoned only by the Execute-time failure.
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT 1", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "25P02"
    client.exchange(
        pgwire.build_parse("", "ROLLBACK", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )


def test_quoted_char_cast_reports_oid_18(client):
    # pgtest char:42 — the QUOTED "char" spelling is PG's internal one-byte
    # type: column named char, oid 18, size 1 (unquoted char stays bpchar).
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT 'a'::\"char\"", []),
        pgwire.build_bind("", "", []),
        pgwire.build_describe("P", ""),
        pgwire.build_execute("", 0),
    )
    rd = next(m for m in msgs if m.type == "T")
    import struct as _s

    end = rd.payload.index(b"\x00", 2)
    assert rd.payload[2:end] == b"char"
    assert _s.unpack_from("!i", rd.payload, end + 7)[0] == 18
    assert _s.unpack_from("!h", rd.payload, end + 11)[0] == 1  # size 1
    assert rows(msgs) == [[b"a"]]


def test_quoted_char_column_truncates_and_nulls(client):
    # pgtest char corpus: a "char" column truncates input to ONE character,
    # and empty / zero-byte input stores NULL.
    client.exchange(
        pgwire.build_parse("", 'CREATE TABLE chart (a int, b "char")', []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    ins = pgwire.build_parse("ins_c", "INSERT INTO chart VALUES ($1, $2)", [])
    msgs = client.exchange(ins, pgwire.build_describe("S", "ins_c"))
    pd = next(m for m in msgs if m.type == "t")
    import struct as _s

    assert _s.unpack_from("!i", pd.payload, 2 + 4)[0] == 18  # $2 -> "char"
    for i, val in ((1, b"eee"), (2, b""), (3, b"\xe2\x98\x83")):  # snowman
        client.exchange(
            pgwire.build_bind("", "ins_c", [str(i).encode(), val]),
            pgwire.build_execute("", 0),
        )
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT b FROM chart ORDER BY a", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b"e"], [None], ["☃".encode()]]


def test_quoted_char_zero_cast_binary_result(client):
    # pgtest char:201 — 0::"char" is the zero byte; binary result format
    # carries it as one 0x00 byte (not SQL NULL).
    msgs = client.exchange(
        pgwire.build_parse("", 'SELECT 0::"char"', []),
        pgwire.build_bind("", "", [], result_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b"\x00"]]


def test_citext_reports_oid_90008(client):
    # pgtest citext corpus: citext rides crdb's stable placeholder oid 90008
    # (the extension has no fixed catalog oid) in ParameterDescription and
    # RowDescription; INSERT targets AND comparisons against a citext column
    # both infer it; equality stays case-insensitive; binary params are the
    # text bytes.
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE cit (id int4 PRIMARY KEY, t citext)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("ins_cit", "INSERT INTO cit (t, id) VALUES ($1, $2)", []),
        pgwire.build_describe("S", "ins_cit"),
        pgwire.build_bind("", "ins_cit", [b"Hi", b"\x00\x00\x00\x01"], param_formats=[1, 1]),
        pgwire.build_execute("", 0),
    )
    pd = next(m for m in msgs if m.type == "t")
    assert pgwire.parse_parameter_description(pd.payload) == [90008, 23]
    msgs = client.exchange(
        pgwire.build_parse("sel_cit", "SELECT id, t FROM cit WHERE t = $1", []),
        pgwire.build_describe("S", "sel_cit"),
        pgwire.build_bind("", "sel_cit", [b"hi"]),
        pgwire.build_execute("", 0),
    )
    pd = next(m for m in msgs if m.type == "t")
    assert pgwire.parse_parameter_description(pd.payload) == [90008]
    rd = next(m for m in msgs if m.type == "T")
    import struct as _s

    end = rd.payload.index(b"\x00", 2)  # first column: id
    end2 = rd.payload.index(b"\x00", end + 19)  # second column: t
    assert _s.unpack_from("!i", rd.payload, end2 + 7)[0] == 90008
    assert rows(msgs) == [[b"1", b"Hi"]]


def test_copy_to_stdout_via_extended_protocol(client):
    # pgtest copy corpus: COPY (query) TO STDOUT through Parse/Bind/Describe/
    # Execute — NoData at Describe, then CopyOutResponse + CopyData + CopyDone
    # + CommandComplete in the Execute reply.
    msgs = client.exchange(
        pgwire.build_parse("", "COPY (select 1) TO STDOUT", []),
        pgwire.build_bind("", "", []),
        pgwire.build_describe("P", ""),
        pgwire.build_execute("", 0),
    )
    tps = types(msgs)
    assert tps == ["1", "2", "n", "H", "d", "c", "C", "Z"]
    d = next(m for m in msgs if m.type == "d")
    assert d.payload == b"1\n"
    assert command_tag(msgs) == "COPY 1"


def test_copy_bind_with_parameters_is_08P01(client):
    # PG's parse analysis gives COPY zero parameters; binding any is 08P01
    # with the statement-summary detail (pgtest copy corpus, keepErrMessage).
    msgs = client.exchange(
        pgwire.build_parse("", "COPY (select $1::int) TO STDOUT", []),
        pgwire.build_bind("", "", [b"1"]),
        pgwire.build_execute("", 0),
    )
    err = next(m for m in msgs if m.type == "E")
    fields = pgwire.parse_error_response(err.payload)
    assert fields.get("C") == "08P01"
    assert fields.get("M") == "bind message supplies 1 parameters, but requires 0"
    assert fields.get("D") == 'statement summary "COPY (SELECT) TO STDOUT"'


def test_copy_unbound_placeholder_is_42P02(client):
    msgs = client.exchange(
        pgwire.build_parse("", "COPY (select $1::int) TO STDOUT", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert types(msgs)[:2] == ["1", "2"]  # BindComplete precedes the error
    err = next(m for m in msgs if m.type == "E")
    fields = pgwire.parse_error_response(err.payload)
    assert fields.get("C") == "42P02"
    assert fields.get("M") == "there is no parameter $1"


def test_binary_numeric_zero_with_many_zero_groups_renders_0(client):
    # pgtest decimal:29 — 8192 all-zero base-10000 digit groups with dscale 0
    # must render "0", not 0.000…0 (scaleb of a zero keeps the huge negative
    # exponent unless the decoder quantizes to dscale unconditionally).
    import struct as _s

    payload = _s.pack("!HhHH", 8192, 0, 0, 0) + b"\x00\x00" * 8192
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::decimal", []),
        pgwire.build_bind("", "", [payload], param_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b"0"]]


def test_binary_numeric_invalid_dscale_is_22P03(client):
    # pgtest decimal:121 — dscale 0xFFF0 (a negative int16) is outside PG's
    # NUMERIC_DSCALE_MASK; numeric_recv rejects it with 22P03 at Bind.
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1::decimal", []),
        pgwire.build_bind("", "", [bytes.fromhex("000100010000FFF00001")], param_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "22P03"
    assert "2" not in types(msgs)  # error fires AT Bind — no BindComplete


def test_enum_oid_range_matches_catalog_bases():
    # pgwire.row_description reports typlen 4 for oids in [65000, 66000) —
    # the minted user-ENUM range. Pin the duplicated constants to catalog's.
    from secantus.sql.catalog import DOMAIN_TYPE_OID_BASE, ENUM_TYPE_OID_BASE

    assert ENUM_TYPE_OID_BASE == 65000
    assert DOMAIN_TYPE_OID_BASE == 66000


def test_enum_cast_names_column_and_typlen_4(client):
    # pgtest enum:64 — SELECT 'hi'::te names the column after the enum type
    # and reports DataTypeSize 4 (PG stores enum values as 4-byte oids).
    client.exchange(
        pgwire.build_parse("", "CREATE TYPE te AS ENUM ('hi', 'hello')", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT 'hi'::te", []),
        pgwire.build_bind("", "", []),
        pgwire.build_describe("P", ""),
        pgwire.build_execute("", 0),
    )
    rd = next(m for m in msgs if m.type == "T")
    import struct as _s

    end = rd.payload.index(b"\x00", 2)
    assert rd.payload[2:end] == b"te"
    assert _s.unpack_from("!h", rd.payload, end + 11)[0] == 4  # typlen
    assert rows(msgs) == [[b"hi"]]


def test_invalid_format_code_is_08P01_at_bind(client):
    # pgtest errors:95 — format codes other than 0/1 are a protocol
    # violation, rejected at Bind before BindComplete.
    import struct as _s

    payload = (
        b"p0\x00s0\x00"
        + _s.pack("!h", 1)
        + _s.pack("!h", 0)
        + _s.pack("!H", 1)
        + _s.pack("!i", 1)
        + b"x"
        + _s.pack("!h", 3)
        + _s.pack("!h", 0)
        + _s.pack("!h", 2)
        + _s.pack("!h", 5)
    )
    from secantus.sql.pgwire import _msg

    msgs = client.exchange(
        pgwire.build_parse("s0", "select $1", []),
        _msg("B", payload),
        pgwire.build_execute("p0", 0),
    )
    assert error_code(msgs) == "08P01"
    assert "2" not in types(msgs)  # no BindComplete


def test_execute_of_sql_prepare_describes_underlying_shape(client):
    # pgtest execute:70 — Describe(P) of a wire-parsed ``EXECUTE name(...)``
    # reports the UNDERLYING prepared SELECT's RowDescription, not NoData.
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE t0 (c0 int8)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("", "INSERT INTO t0 VALUES (1)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("", "PREPARE sq (int8) AS SELECT * FROM t0 WHERE c0 = $1", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("sq_stmt", "EXECUTE sq(1)", []),
        pgwire.build_bind("sq_portal", "sq_stmt", []),
        pgwire.build_describe("P", "sq_portal"),
        pgwire.build_execute("sq_portal", 0),
    )
    rd = next(m for m in msgs if m.type == "T")
    import struct as _s

    end = rd.payload.index(b"\x00", 2)
    assert rd.payload[2:end] == b"c0"
    assert _s.unpack_from("!i", rd.payload, end + 7)[0] == 20  # int8
    assert rows(msgs) == [[b"1"]]


def test_float4_renders_shortest_single_precision(client):
    # pgtest float corpus — float4out is the shortest SINGLE-precision
    # round-trip; float8 keeps the double form. Arrays follow element tags.
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT (1/3.0)::float4, (1/3.0)::float8", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b"0.33333334", b"0.3333333333333333"]]
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT ARRAY[(1/3.0)::float4, 'inf'::float4]", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b"{0.33333334,Infinity}"]]


def test_negative_extra_float_digits_reduces_precision(client):
    # SET extra_float_digits = -N reduces %g precision (PG's float_out);
    # the negative value itself must survive SET parsing (Neg node).
    client.exchange(
        pgwire.build_parse("", "SET extra_float_digits = -1", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT (1/3.0)::float4, (1/3.0)::float8", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b"0.33333", b"0.33333333333333"]]


def test_nested_begin_warns_25001_with_pg_fields(client):
    # pgtest implicit_txn:49 — BEGIN inside an explicit block completes with
    # the BEGIN tag but emits a WARNING NoticeResponse carrying PG's exact
    # identity fields (25001, xact.c, BeginTransactionBlock); the block
    # survives.
    client.exchange(
        pgwire.build_parse("", "BEGIN", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("", "BEGIN", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    notice = next(m for m in msgs if m.type == "N")
    fields = pgwire.parse_error_response(notice.payload)
    assert fields.get("S") == "WARNING"
    assert fields.get("C") == "25001"
    assert fields.get("M") == "there is already a transaction in progress"
    assert fields.get("F") == "xact.c"
    assert fields.get("R") == "BeginTransactionBlock"
    assert command_tag(msgs) == "BEGIN"
    assert msgs[-1].payload == b"T"  # still in the block
    client.exchange(
        pgwire.build_parse("", "ROLLBACK", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )


def test_binary_inet_error_classes(client):
    # pgtest inet corpus — a truncated binary inet payload is 08P01; a bad
    # family or address length is 22P03 (PG's inet_recv classes; XX000
    # leaked before).
    for payload, code in (
        (b"", "08P01"),
        (bytes.fromhex("020000000000"), "22P03"),
        (bytes.fromhex("030000000000"), "22P03"),
        (bytes.fromhex("060000000000"), "22P03"),
    ):
        msgs = client.exchange(
            pgwire.build_parse("", "SELECT $1::INET", []),
            pgwire.build_bind("", "", [payload], param_formats=[1]),
            pgwire.build_execute("", 0),
        )
        assert error_code(msgs) == code, (payload.hex(), error_code(msgs))


def test_int2vector_binary_result_is_int2_array(client):
    # pgtest int2vector corpus (crdb #111907): the binary wire form of
    # int2vector is an int2 ARRAY — elemoid 21, 2-byte elements, lower
    # bound 1. (The corpus file's expected indoption VALUE is crdb's 2;
    # PG and we report 0 — recorded in EXPECTED_DIVERGENCES.)
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE i2v (a int primary key, b text)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse(
            "",
            "select i.indoption from pg_index i join pg_class c "
            "on i.indrelid = c.oid where c.relname = 'i2v'",
            [],
        ),
        pgwire.build_bind("", "", [], result_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[bytes.fromhex("0000000100000000000000150000000100000001000000020000")]]


def test_jsonpath_type_and_canonical_text(client):
    # pgtest jsonpath corpus — oid 4072, canonical member quoting, 42601 on
    # an empty path, and PG-true binary (version byte + UNQUOTED text; the
    # corpus's quoted expectation is crdb's, in EXPECTED_DIVERGENCES).
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT '$.abc'::JSONPATH", []),
        pgwire.build_bind("", "", []),
        pgwire.build_describe("P", ""),
        pgwire.build_execute("", 0),
    )
    rd = next(m for m in msgs if m.type == "T")
    import struct as _s

    end = rd.payload.index(b"\x00", 2)
    assert rd.payload[2:end] == b"jsonpath"
    assert _s.unpack_from("!i", rd.payload, end + 7)[0] == 4072
    assert rows(msgs) == [[b'$."abc"']]
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT ''::JSONPATH", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "42601"
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT '$'::JSONPATH", []),
        pgwire.build_bind("", "", [], result_formats=[1]),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b"\x01$"]]


def test_jsonb_path_query_names_column_and_returns_jsonb(client):
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT jsonb_path_query('{\"a\": true}', '$.a')", []),
        pgwire.build_bind("", "", []),
        pgwire.build_describe("P", ""),
        pgwire.build_execute("", 0),
    )
    rd = next(m for m in msgs if m.type == "T")
    import struct as _s

    end = rd.payload.index(b"\x00", 2)
    assert rd.payload[2:end] == b"jsonb_path_query"
    assert _s.unpack_from("!i", rd.payload, end + 7)[0] == 3802
    assert rows(msgs) == [[b"true"]]


def test_ltree_reports_oid_90010(client):
    # pgtest ltree corpus: ltree rides crdb's stable placeholder oid 90010
    # (no fixed catalog oid, like citext/hstore); INSERT targets and
    # comparisons against an ltree column infer it; the binary format is a
    # version byte + the label-path text.
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE lt (id int4 PRIMARY KEY, t ltree)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("ins_lt", "INSERT INTO lt (t, id) VALUES ($1, $2)", []),
        pgwire.build_describe("S", "ins_lt"),
        pgwire.build_bind("", "ins_lt", [b"A.B", b"1"]),
        pgwire.build_execute("", 0),
    )
    pd = next(m for m in msgs if m.type == "t")
    assert pgwire.parse_parameter_description(pd.payload) == [90010, 23]
    msgs = client.exchange(
        pgwire.build_parse("sel_lt", "SELECT id, t FROM lt WHERE t = $1", []),
        pgwire.build_describe("S", "sel_lt"),
        pgwire.build_bind("", "sel_lt", [b"\x01A.B"], param_formats=[1]),
        pgwire.build_execute("", 0),
    )
    pd = next(m for m in msgs if m.type == "t")
    assert pgwire.parse_parameter_description(pd.payload) == [90010]
    assert rows(msgs) == [[b"1", b"A.B"]]


def test_duplicate_named_portal_in_txn_is_42P03(client):
    # pgtest multiple_active_portals — re-binding a NAMED portal still live
    # in the same explicit transaction is PG's 42P03, with crdb's detail
    # shape; the block poisons and COMMIT reports ROLLBACK.
    client.exchange(
        pgwire.build_parse("", "BEGIN", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("q1", "SELECT 1", []),
        pgwire.build_bind("dp", "q1", []),
    )
    msgs = client.exchange(
        pgwire.build_parse("q2", "SELECT 2", []),
        pgwire.build_bind("dp", "q2", []),
        pgwire.build_execute("dp", 0),
    )
    err = next(m for m in msgs if m.type == "E")
    fields = pgwire.parse_error_response(err.payload)
    assert fields.get("C") == "42P03"
    assert fields.get("M") == 'portal "dp" already exists'
    assert 'statement name "q2"' in fields.get("D", "")
    msgs = client.exchange(
        pgwire.build_parse("", "COMMIT", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert command_tag(msgs) == "ROLLBACK"


def test_portal_dies_at_implicit_txn_end(client):
    # pgtest multiple_active_portals — a portal suspended outside an
    # explicit block dies at Sync; a later Execute is 34000 with crdb's
    # message shape.
    client.exchange(
        pgwire.build_parse("qs", "SELECT * FROM generate_series(1, 5)", []),
        pgwire.build_bind("ps", "qs", []),
        pgwire.build_execute("ps", 1),
    )
    msgs = client.exchange(pgwire.build_execute("ps", 1))
    err = next(m for m in msgs if m.type == "E")
    fields = pgwire.parse_error_response(err.payload)
    assert fields.get("C") == "34000"
    assert fields.get("M") == 'unknown portal "ps"'


def test_drop_table_with_active_portal_is_55006(client):
    # pgtest multiple_active_portals — DROP TABLE while a suspended portal
    # in the same session still reads the table refuses with PG's 55006.
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE pin_t (x int)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("", "INSERT INTO pin_t VALUES (1),(2)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("", "BEGIN", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("qp", "SELECT * FROM pin_t", []),
        pgwire.build_bind("pp", "qp", []),
        pgwire.build_execute("pp", 1),
    )
    msgs = client.exchange(
        pgwire.build_parse("", "DROP TABLE pin_t", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) == "55006"
    client.exchange(
        pgwire.build_parse("", "ROLLBACK", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )


def test_drop_table_own_portal_does_not_self_pin(client):
    # Regression: the DROP TABLE portal carries the table as its own target, so
    # the active-cursor guard used to count the DROP being executed as a "query
    # using the table" and refuse it (55006), breaking pgjdbc's
    # DatabaseMetaDataTest setup which drops via the extended protocol. Only an
    # active READ cursor pins — a write portal (the DROP itself) does not.
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE selfpin (x int)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("", "DROP TABLE IF EXISTS selfpin CASCADE", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) is None


def test_drop_table_after_drained_portal_succeeds(client):
    # A fully-fetched SELECT portal no longer pins the table.
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE drained (x int)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("", "INSERT INTO drained VALUES (1)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("dq", "SELECT * FROM drained", []),
        pgwire.build_bind("dp", "dq", []),
        pgwire.build_execute("dp", 0),  # 0 = fetch all → drained
    )
    msgs = client.exchange(
        pgwire.build_parse("", "DROP TABLE drained", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert error_code(msgs) is None


def test_reg_pseudotypes_binary_and_typlen(client):
    # pgtest oid corpus — the reg* pseudo-types ride the oid wire form: a
    # 4-byte unsigned int, typlen 4 in RowDescription; a wrong-length
    # payload is 08P01 (PG's oidrecv), not raw bytes echoed back.
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $1, $2, $3, $4, $5", [2205, 4089, 24, 2206, 26]),
        pgwire.build_describe("S", ""),
        pgwire.build_bind(
            "",
            "",
            [
                bytes.fromhex("01000029"),
                bytes.fromhex("0100002a"),
                bytes.fromhex("0100002b"),
                bytes.fromhex("0100002c"),
                bytes.fromhex("ffffffff"),
            ],
            param_formats=[1, 1, 1, 1, 1],
        ),
        pgwire.build_execute("", 0),
    )
    assert rows(msgs) == [[b"16777257", b"16777258", b"16777259", b"16777260", b"4294967295"]]
    rd = next(m for m in msgs if m.type == "T")
    import struct as _s

    off = 2
    for _ in range(5):
        end = rd.payload.index(b"\x00", off)
        assert _s.unpack_from("!h", rd.payload, end + 11)[0] == 4  # typlen
        off = end + 19
    for bad in (bytes.fromhex("0029"), bytes.fromhex("010000290000")):
        msgs = client.exchange(
            pgwire.build_parse("", "SELECT $1", [2205]),
            pgwire.build_bind("", "", [bad], param_formats=[1]),
            pgwire.build_execute("", 0),
        )
        assert error_code(msgs) == "08P01"


def _params(msgs):
    out = []
    for m in msgs:
        if m.type == "S":
            parts = m.payload.split(b"\x00")
            out.append((parts[0].decode(), parts[1].decode()))
    return out


def test_parameter_status_follows_command_complete(client):
    # pgtest param_status:7 — PG reports GUC changes AFTER the command's
    # CommandComplete, just before ReadyForQuery.
    msgs = client.exchange(
        pgwire.build_parse("", "SET application_name = 'pgtest'", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    tps = [m.type for m in msgs if m.type in ("C", "S", "Z")]
    assert tps == ["C", "S", "Z"]
    assert _params(msgs) == [("application_name", "pgtest")]


def test_numeric_time_zone_reports_posix_spec(client):
    # pgtest param_status — a numeric offset reports PG's POSIX zone spec,
    # with the sign inverted after the label.
    msgs = client.exchange(
        pgwire.build_parse("", "SET TIME ZONE +6", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert _params(msgs) == [("TimeZone", "<+06>-06")]
    msgs = client.exchange(
        pgwire.build_parse("", "SET TIME ZONE -11.5", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert _params(msgs) == [("TimeZone", "<-11:30>+11:30")]


def test_datestyle_and_intervalstyle_reported_canonically(client):
    msgs = client.exchange(
        pgwire.build_parse("", "SET DateStyle = 'YMD, ISO'", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert _params(msgs) == [("DateStyle", "ISO, YMD")]
    msgs = client.exchange(
        pgwire.build_parse("", "SET IntervalStyle = 'ISO_8601'", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert _params(msgs) == [("IntervalStyle", "iso_8601")]


def test_savepoint_rollback_reverts_and_reports_gucs(client):
    # pgtest param_status — GUCs set after a savepoint revert with it and the
    # reverted GUC_REPORT ones are re-reported, ordered case-insensitively.
    for sql in ("BEGIN", "SET LOCAL TIME ZONE 'Australia/Adelaide'", "SAVEPOINT s1"):
        client.exchange(
            pgwire.build_parse("", sql, []),
            pgwire.build_bind("", "", []),
            pgwire.build_execute("", 0),
        )
    client.exchange(
        pgwire.build_parse("", "SET LOCAL TIME ZONE 'Australia/Perth'", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("", "ROLLBACK TO SAVEPOINT s1", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    assert ("TimeZone", "Australia/Adelaide") in _params(msgs)
    client.exchange(
        pgwire.build_parse("", "ROLLBACK", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )


def test_cast_of_column_keeps_the_column_name(client):
    # pgtest parameter_description:14 — PG's FigureColname recurses into a
    # cast's operand first, so ``n::int4`` is named ``n`` (a literal cast like
    # ``2::int8`` still yields the typname).
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT n::int4 FROM generate_series(0, 1) n", []),
        pgwire.build_bind("", "", []),
        pgwire.build_describe("P", ""),
        pgwire.build_execute("", 0),
    )
    rd = next(m for m in msgs if m.type == "T")
    assert rd.payload[2 : rd.payload.index(b"\x00", 2)] == b"n"
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT 2::int8", []),
        pgwire.build_bind("", "", []),
        pgwire.build_describe("P", ""),
        pgwire.build_execute("", 0),
    )
    rd = next(m for m in msgs if m.type == "T")
    assert rd.payload[2 : rd.payload.index(b"\x00", 2)] == b"int8"


def test_column_typed_parameters_in_update(client):
    # pgtest parameter_description — ``SET col = $N`` and ``WHERE col = $N``
    # type the parameter as the COLUMN's type.
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE ptab (a uuid PRIMARY KEY, b timestamptz)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_parse("u1", "UPDATE ptab SET b = $1 WHERE a = $2", []),
        pgwire.build_describe("S", "u1"),
    )
    pd = next(m for m in msgs if m.type == "t")
    assert pgwire.parse_parameter_description(pd.payload) == [1184, 2950]


def test_conflicting_parameter_type_is_42883(client):
    # One type per parameter: ``$1::int`` pins int4, so ``lower($1)`` cannot
    # resolve (pgtest parameter_description:40).
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT lower($1), $1::int", []),
        pgwire.build_describe("S", ""),
    )
    assert error_code(msgs) == "42883"


def test_parameter_numbering_gap_is_42P18(client):
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT $2 > 0", []),
        pgwire.build_describe("S", ""),
    )
    assert error_code(msgs) == "42P18"


def test_bare_parameter_case_result_is_42P18(client):
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT 3 + CASE (4) WHEN 4 THEN $1 END", []),
        pgwire.build_describe("S", ""),
    )
    assert error_code(msgs) == "42P18"
    # A typed sibling branch resolves the CASE, so this one is fine.
    msgs = client.exchange(
        pgwire.build_parse("", "SELECT CASE WHEN true THEN $1 ELSE 'x' END", []),
        pgwire.build_describe("S", ""),
    )
    assert error_code(msgs) is None


def test_exact_max_rows_suspends_then_reports_zero(client):
    # pgtest portals — PG cannot know a portal is exhausted until an Execute
    # fetches past the last row: an Execute delivering EXACTLY MaxRows always
    # suspends, and the next one reports CommandComplete with the rows IT
    # delivered (SELECT 0).
    client.exchange(
        pgwire.build_parse("", "BEGIN", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("pq", "SELECT * FROM generate_series(1, 2)", []),
        pgwire.build_bind("pp", "pq", []),
    )
    first = client.exchange(pgwire.build_execute("pp", 1))
    assert rows(first) == [[b"1"]] and any(m.type == "s" for m in first)
    second = client.exchange(pgwire.build_execute("pp", 1))
    # The second row exhausts the data, but PG still suspends here.
    assert rows(second) == [[b"2"]] and any(m.type == "s" for m in second)
    third = client.exchange(pgwire.build_execute("pp", 1))
    assert rows(third) == []
    assert command_tag(third) == "SELECT 0"
    client.exchange(
        pgwire.build_parse("", "ROLLBACK", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )


def test_bind_parameter_count_must_match_declared(client):
    # pgtest prepare — Bind must supply exactly as many parameters as the
    # prepared statement has, and DECLARED oids count even when the query uses
    # fewer placeholders (three declared, one used → a one-parameter Bind is
    # 08P01).
    msgs = client.exchange(
        pgwire.build_parse("s3", "SELECT $1", [1043, 1043, 1043]),
        pgwire.build_bind("p3", "s3", [b"a", b"b", b"c"]),
        pgwire.build_execute("p3", 0),
    )
    assert rows(msgs) == [[b"a"]]
    msgs = client.exchange(
        pgwire.build_bind("p3", "s3", [b"a"]),
        pgwire.build_execute("p3", 0),
    )
    err = next(m for m in msgs if m.type == "E")
    fields = pgwire.parse_error_response(err.payload)
    assert fields.get("C") == "08P01"
    assert fields.get("M") == "bind message supplies 1 parameters, but requires 3"
    # A COPY still reports its own 08P01, with PG's statement-summary Detail.
    msgs = client.exchange(
        pgwire.build_parse("", "COPY (select $1::int) TO STDOUT", []),
        pgwire.build_bind("", "", [b"1"]),
        pgwire.build_execute("", 0),
    )
    fields = pgwire.parse_error_response(next(m for m in msgs if m.type == "E").payload)
    assert fields.get("C") == "08P01"
    assert "statement summary" in fields.get("D", "")


def test_cached_plan_revalidation_fires_at_bind(client):
    # pgtest prepared_stmt_invalidation:87 — a named statement whose result
    # shape changed under DDL raises 0A000 INSTEAD of BindComplete, so no
    # portal is created. (The aborted_txn corpus ignores BindComplete, so
    # Bind-time satisfies both files.)
    client.exchange(
        pgwire.build_parse("", "CREATE TABLE dropc (f1 int, f2 text)", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    client.exchange(
        pgwire.build_parse("", "INSERT INTO dropc VALUES (1, 'hello')", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    first = client.exchange(
        pgwire.build_parse("s1", "SELECT * FROM dropc WHERE f1 = $1", []),
        pgwire.build_bind("p1", "s1", [b"1"]),
        pgwire.build_execute("p1", 0),
    )
    assert rows(first) == [[b"1", b"hello"]]
    client.exchange(
        pgwire.build_parse("", "ALTER TABLE dropc DROP COLUMN f1", []),
        pgwire.build_bind("", "", []),
        pgwire.build_execute("", 0),
    )
    msgs = client.exchange(
        pgwire.build_bind("p1", "s1", [b"1"]),
        pgwire.build_execute("p1", 0),
    )
    err = next(m for m in msgs if m.type == "E")
    fields = pgwire.parse_error_response(err.payload)
    assert fields.get("C") == "0A000"
    assert fields.get("R") == "RevalidateCachedQuery"
    assert "2" not in types(msgs)  # no BindComplete — the error replaces it
