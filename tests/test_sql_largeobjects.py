"""PostgreSQL Large Object API (pgjdbc's ``LargeObjectManager``).

The surface pgjdbc drives: the Fastpath sub-protocol ('F' FunctionCall /
'V' FunctionCallResponse) dispatching the ``lo_*`` built-ins by their real
``pg_proc`` OIDs, a chunked sparse byte store in per-database collections,
SQL-callable ``lo_creat``/``lo_create``/``lo_unlink``, and the pieces the
pgjdbc CallableStatement/Blob tests need around it: UDF calls in FROM
position typed by the function's declared return type, describe-only shape
derivation (Describe must never run a side-effecting body), the VOID bind
convention for the JDBC OUT-parameter slot, plpgsql ``RAISE``, and the
``lo_manage`` trigger accommodations. Everything runs against the real
WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import socket
import struct

import pytest

from secantus.sql import errors, largeobjects, pgwire, planner, run_sql
from secantus.sql.largeobjects import INV_READ, INV_WRITE, LO_PROC_OIDS
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def lo(storage, session, name, *args):
    packed = [a if isinstance(a, bytes) else struct.pack(">i", a) for a in args]
    return largeobjects.call(LO_PROC_OIDS[name], packed, storage=storage, db=DB, session=session)


def _i32(b: bytes) -> int:
    return struct.unpack(">i", b)[0]


def _i64(b: bytes) -> int:
    return struct.unpack(">q", b)[0]


# --------------------------------------------------------------------------- #
# Store semantics (direct Fastpath dispatch)
# --------------------------------------------------------------------------- #


def test_create_write_read_roundtrip(storage, session):
    oid = _i32(lo(storage, session, "lo_creat", -1))
    assert oid >= 16384
    fd = _i32(lo(storage, session, "lo_open", oid, INV_READ | INV_WRITE))
    assert _i32(lo(storage, session, "lowrite", fd, b"hello large world")) == 17
    lo(storage, session, "lo_lseek", fd, 0, 0)
    assert lo(storage, session, "loread", fd, 5) == b"hello"
    assert lo(storage, session, "loread", fd, 100) == b" large world"
    assert lo(storage, session, "loread", fd, 100) == b""  # EOF
    assert _i32(lo(storage, session, "lo_close", fd)) == 0


def test_lo_create_explicit_oid_and_duplicate(storage, session):
    assert _i32(lo(storage, session, "lo_create", 4321)) == 4321
    with pytest.raises(errors.SQLError) as e:
        lo(storage, session, "lo_create", 4321)
    assert e.value.sqlstate == "23505"
    # oid 0 means "assign one"
    assert _i32(lo(storage, session, "lo_create", 0)) >= 16384


def test_seek_tell_and_64bit_variants(storage, session):
    oid = _i32(lo(storage, session, "lo_creat", -1))
    fd = _i32(lo(storage, session, "lo_open", oid, INV_READ | INV_WRITE))
    lo(storage, session, "lowrite", fd, b"0123456789")
    assert _i32(lo(storage, session, "lo_lseek", fd, -4, 2)) == 6  # SEEK_END
    assert _i32(lo(storage, session, "lo_tell", fd)) == 6
    assert lo(storage, session, "loread", fd, 2) == b"67"
    seek64 = lo(storage, session, "lo_lseek64", fd, struct.pack(">q", 1), struct.pack(">i", 0))
    assert _i64(seek64) == 1
    assert _i64(lo(storage, session, "lo_tell64", fd)) == 1
    assert lo(storage, session, "loread", fd, 3) == b"123"


def test_sparse_write_past_end_zero_fills(storage, session):
    oid = _i32(lo(storage, session, "lo_creat", -1))
    fd = _i32(lo(storage, session, "lo_open", oid, INV_READ | INV_WRITE))
    lo(storage, session, "lo_lseek", fd, 1_000_000, 0)  # cross chunk boundaries
    lo(storage, session, "lowrite", fd, b"tail")
    lo(storage, session, "lo_lseek", fd, 0, 0)
    head = lo(storage, session, "loread", fd, 8)
    assert head == b"\x00" * 8
    lo(storage, session, "lo_lseek", fd, 999_998, 0)
    assert lo(storage, session, "loread", fd, 6) == b"\x00\x00tail"


def test_truncate_shrinks_and_extends(storage, session):
    oid = _i32(lo(storage, session, "lo_creat", -1))
    fd = _i32(lo(storage, session, "lo_open", oid, INV_READ | INV_WRITE))
    lo(storage, session, "lowrite", fd, b"abcdef")
    lo(storage, session, "lo_truncate", fd, 3)
    lo(storage, session, "lo_lseek", fd, 0, 0)
    assert lo(storage, session, "loread", fd, 10) == b"abc"
    lo(storage, session, "lo_truncate64", fd, struct.pack(">q", 6))
    lo(storage, session, "lo_lseek", fd, 0, 0)
    assert lo(storage, session, "loread", fd, 10) == b"abc\x00\x00\x00"


def test_write_requires_write_mode(storage, session):
    oid = _i32(lo(storage, session, "lo_creat", -1))
    fd = _i32(lo(storage, session, "lo_open", oid, INV_READ))
    with pytest.raises(errors.SQLError):
        lo(storage, session, "lowrite", fd, b"x")


def test_unlink_and_open_missing(storage, session):
    oid = _i32(lo(storage, session, "lo_creat", -1))
    assert _i32(lo(storage, session, "lo_unlink", oid)) == 1
    with pytest.raises(errors.SQLError) as e:
        lo(storage, session, "lo_open", oid, INV_READ)
    assert e.value.sqlstate == "42704"


def test_bad_descriptor_and_unknown_oid(storage, session):
    with pytest.raises(errors.SQLError):
        lo(storage, session, "lo_close", 999)
    with pytest.raises(errors.SQLError) as e:
        largeobjects.call(1, [], storage=storage, db=DB, session=session)
    assert e.value.sqlstate == "42883"


def test_data_survives_reopen_within_store(storage, session):
    oid = _i32(lo(storage, session, "lo_creat", -1))
    fd = _i32(lo(storage, session, "lo_open", oid, INV_READ | INV_WRITE))
    lo(storage, session, "lowrite", fd, b"persist me")
    lo(storage, session, "lo_close", fd)
    fd2 = _i32(lo(storage, session, "lo_open", oid, INV_READ))
    assert lo(storage, session, "loread", fd2, 100) == b"persist me"


# --------------------------------------------------------------------------- #
# SQL-callable management functions
# --------------------------------------------------------------------------- #


def test_sql_lo_creat_and_unlink(storage, session):
    oid = q(storage, session, "SELECT lo_creat(-1)").rows[0][0]
    assert oid >= 16384
    assert q(storage, session, f"SELECT lo_unlink({oid})").rows[0][0] == 1


def test_sql_lo_creat_inside_insert(storage, session):
    q(storage, session, "CREATE TABLE t (id int, lob oid)")
    q(storage, session, "BEGIN")
    q(storage, session, "INSERT INTO t VALUES (1, lo_creat(-1))")
    q(storage, session, "COMMIT")
    lob = q(storage, session, "SELECT lob FROM t").rows[0][0]
    fd = _i32(lo(storage, session, "lo_open", int(lob), INV_READ))
    assert lo(storage, session, "loread", fd, 10) == b""


# --------------------------------------------------------------------------- #
# Fastpath wire protocol
# --------------------------------------------------------------------------- #


def test_parse_function_call_roundtrip():
    body = struct.pack(">i", 957)  # lo_creat
    body += struct.pack(">h", 1) + struct.pack(">h", 1)  # one format code
    body += struct.pack(">h", 2)
    body += struct.pack(">i", 4) + struct.pack(">i", -1)  # arg 1
    body += struct.pack(">i", -1)  # arg 2: NULL
    body += struct.pack(">h", 1)  # result format
    fn_oid, args = pgwire.parse_function_call(body)
    assert fn_oid == 957
    assert args == [struct.pack(">i", -1), b""]


def test_fastpath_over_socket(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        s = socket.create_connection((host, port), timeout=5)
        try:
            s.sendall(pgwire.build_startup_message({"user": "joe", "database": DB}))
            while pgwire.read_message(s).type != "Z":
                pass
            body = struct.pack(">i", LO_PROC_OIDS["lo_creat"])
            body += struct.pack(">h", 0) + struct.pack(">h", 1)
            body += struct.pack(">i", 4) + struct.pack(">i", -1)
            body += struct.pack(">h", 1)
            s.sendall(b"F" + struct.pack(">i", len(body) + 4) + body)
            m = pgwire.read_message(s)
            assert m.type == "V"
            length, oid = struct.unpack_from(">ii", m.payload)
            assert length == 4 and oid >= 16384
            assert pgwire.read_message(s).type == "Z"
        finally:
            s.close()
    finally:
        srv.stop()
        st.close()


# --------------------------------------------------------------------------- #
# plpgsql RAISE
# --------------------------------------------------------------------------- #


def test_raise_notice_flows_to_result(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION noisy() RETURNS int AS "
        "'BEGIN RAISE NOTICE ''hello''; RAISE NOTICE ''val %'', 7; RETURN 1; END;' "
        "LANGUAGE plpgsql",
    )
    res = q(storage, session, "SELECT noisy()")
    assert res.rows == [(1,)]
    assert [m for _sev, m in res.notices] == ["hello", "val 7"]
    assert all(sev == "NOTICE" for sev, _m in res.notices)


def test_raise_exception(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION boom() RETURNS int AS "
        "'BEGIN RAISE EXCEPTION ''bad %'', 42; END;' LANGUAGE plpgsql",
    )
    with pytest.raises(errors.SQLError) as e:
        q(storage, session, "SELECT boom()")
    assert e.value.sqlstate == "P0001"
    assert "bad 42" in str(e.value)


# --------------------------------------------------------------------------- #
# UDF in FROM position (pgjdbc CallableStatement shape)
# --------------------------------------------------------------------------- #


def test_udf_in_from_typed_by_return_tag(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION getd(float) RETURNS float AS 'BEGIN RETURN 42.42; END;' LANGUAGE plpgsql",
    )
    res = q(storage, session, "select * from getd(3.04) as result")
    assert res.rows == [(42.42,)]
    assert res.columns[0].type_tag == "float8"


def test_builtin_func_nodes_in_from(storage, session):
    res = q(storage, session, "select * from now() as result")
    assert res.columns[0].type_tag == "timestamptz"
    assert res.rows[0][0].tzinfo is not None
    res = q(storage, session, "select * from version() as v")
    assert res.columns[0].type_tag == "text"
    assert "PostgreSQL" in res.rows[0][0]


def test_void_bind_dropped_from_call(session):
    import sqlglot

    stmt = sqlglot.parse_one("select * from f($1, $2) as result", read="postgres")
    bound = planner.substitute_parameters(stmt, [planner.VOID_BIND, 7])
    assert bound.sql(dialect="postgres") == "SELECT * FROM F(7) AS result"


# --------------------------------------------------------------------------- #
# Describe must not execute a side-effecting UDF (double-execution bug)
# --------------------------------------------------------------------------- #


def test_extended_protocol_udf_executes_once(tmp_path):
    psycopg = pytest.importorskip("psycopg")
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        with psycopg.connect(host=host, port=port, dbname=DB, user="joe", autocommit=True) as conn:
            cur = conn.cursor()
            cur.execute("CREATE TABLE int_table (id int)")
            cur.execute(
                "CREATE FUNCTION ins(int) RETURNS int AS "
                "'BEGIN INSERT INTO int_table(id) VALUES ($1); RETURN 1; END;' "
                "LANGUAGE plpgsql"
            )
            cur.executemany("select * from ins(%s) as result", [(1,), (2,), (3,)])
            cur.execute("SELECT id FROM int_table ORDER BY id")
            assert cur.fetchall() == [(1,), (2,), (3,)]
            # the declared return type reaches the wire as int4 (oid 23)
            cur.execute("select * from ins(%s) as result", (9,))
            assert cur.description[0].type_code == 23
    finally:
        srv.stop()
        st.close()


# --------------------------------------------------------------------------- #
# lo_manage trigger accommodations (pgjdbc BlobTest setup DDL)
# --------------------------------------------------------------------------- #


def test_lo_manage_function_and_trigger_accepted(storage, session):
    # Verbatim BlobTransactionTest setup DDL (the unqualified ``RETURNS
    # trigger`` spelling isn't parseable by sqlglot; pgjdbc always sends the
    # qualified form).
    q(
        storage,
        session,
        "CREATE OR REPLACE FUNCTION lo_manage() RETURNS pg_catalog.trigger "
        "AS '$libdir/lo' LANGUAGE C",
    )
    q(storage, session, "CREATE TABLE testblob (id text, lo oid)")
    res = q(
        storage,
        session,
        "CREATE TRIGGER testblob_lomanage BEFORE UPDATE OR DELETE ON testblob "
        "FOR EACH ROW EXECUTE PROCEDURE lo_manage(lo)",
    )
    assert res.command_tag == "CREATE TRIGGER"


def test_other_triggers_still_rejected(storage, session):
    q(storage, session, "CREATE TABLE tt (id int)")
    with pytest.raises(errors.SQLError):
        q(
            storage,
            session,
            "CREATE TRIGGER tr BEFORE INSERT ON tt FOR EACH ROW EXECUTE PROCEDURE some_fn()",
        )
