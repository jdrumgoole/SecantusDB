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

import contextlib
import datetime as _dt
from decimal import Decimal

import bson
import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


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


def test_binary_result_format(server):
    # ``binary=True`` makes psycopg request results in the BINARY format, so the
    # server must encode each value per its type OID (the inverse of binary params).
    with connect(server, autocommit=True) as conn:
        conn.execute(
            "CREATE TABLE r (id bigint primary key, n int, x float8, price numeric, "
            "flag boolean, label text, at timestamptz)"
        )
        when = _dt.datetime(2021, 3, 4, 5, 6, 7, tzinfo=_dt.timezone.utc)
        conn.execute(
            "INSERT INTO r VALUES (%s, %s, %s, %s, %s, %s, %s)",
            (1, 42, 3.5, Decimal("19.99"), True, "hi", when),
        )
        row = conn.execute("SELECT id, n, x, price, flag, label, at FROM r", binary=True).fetchone()
        assert row == (1, 42, 3.5, Decimal("19.99"), True, "hi", when)


def test_binary_numeric_edge_cases(server):
    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TABLE n (id bigint primary key, v numeric)")
        for i, raw in enumerate(["0", "100", "10000", "0.5", "-12.34", "123456.789", "-0.001"]):
            conn.execute("INSERT INTO n (id, v) VALUES (%s, %s)", (i, Decimal(raw)))
        rows = conn.execute("SELECT v FROM n ORDER BY id", binary=True).fetchall()
        assert [r[0] for r in rows] == [
            Decimal("0"),
            Decimal("100"),
            Decimal("10000"),
            Decimal("0.5"),
            Decimal("-12.34"),
            Decimal("123456.789"),
            Decimal("-0.001"),
        ]


def test_set_operations(server):
    # Set operations through libpq's extended protocol — exercises Describe
    # resolving the result shape from the first arm.
    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TABLE a (id bigint primary key, n int)")
        conn.execute("CREATE TABLE b (id bigint primary key, n int)")
        for i, v in enumerate([1, 2, 2, 3], 1):
            conn.execute("INSERT INTO a (id, n) VALUES (%s, %s)", (i, v))
        for i, v in enumerate([2, 3, 4], 1):
            conn.execute("INSERT INTO b (id, n) VALUES (%s, %s)", (i, v))
        assert conn.execute("SELECT n FROM a UNION SELECT n FROM b ORDER BY n").fetchall() == [
            (1,),
            (2,),
            (3,),
            (4,),
        ]
        assert conn.execute("SELECT n FROM a INTERSECT SELECT n FROM b ORDER BY n").fetchall() == [
            (2,),
            (3,),
        ]
        assert conn.execute("SELECT n FROM a EXCEPT SELECT n FROM b").fetchall() == [(1,)]


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


def test_idle_in_transaction_session_timeout(server):
    """A connection left idle inside a transaction longer than
    idle_in_transaction_session_timeout is terminated with 25P03."""
    import time

    conn = connect(server)  # autocommit off; closed manually (it dies mid-test)
    try:
        conn.execute("SET idle_in_transaction_session_timeout = 100")
        conn.execute("SELECT 1")  # opens the transaction block
        time.sleep(0.6)
        # The typed IdleInTransactionSessionTimeout on platforms that deliver
        # the FATAL message before the close; on Windows the socket abort can
        # race it, surfacing a plain OperationalError (same as psycopg's own
        # test_right_exception_on_session_timeout win32 branch).
        with pytest.raises(
            (psycopg.errors.IdleInTransactionSessionTimeout, psycopg.OperationalError)
        ):
            conn.execute("SELECT 1")
    finally:
        with contextlib.suppress(psycopg.Error):
            conn.close()
    # An idle transaction well inside the server default (120s) is left alone.
    with connect(server) as conn:
        conn.execute("SELECT 1")
        time.sleep(0.3)
        assert conn.execute("SELECT 2").fetchone() == (2,)


def test_idle_in_txn_server_default_applies(tmp_path):
    """The server-level idle_in_transaction_timeout_s applies to sessions that
    never SET the GUC: an abandoned open transaction is aborted and the
    connection terminated (25P03), and its writes roll back. This is the
    guard against a leaked in-transaction connection pinning WT's oldest
    snapshot and degrading every later write (the pgjdbc-gauge lane hang)."""
    import time

    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st, idle_in_transaction_timeout_s=0.1)
    srv.start()
    try:
        conn = connect(srv)  # autocommit off; dies mid-test
        try:
            conn.execute("CREATE TABLE leak_t (v int)")
            conn.commit()
            conn.execute("INSERT INTO leak_t VALUES (1)")  # open txn with a write
            time.sleep(0.6)
            with pytest.raises(
                (psycopg.errors.IdleInTransactionSessionTimeout, psycopg.OperationalError)
            ):
                conn.execute("SELECT 1")
        finally:
            with contextlib.suppress(psycopg.Error):
                conn.close()
        with connect(srv) as conn2:
            assert conn2.execute("SELECT count(*) FROM leak_t").fetchone() == (0,)
    finally:
        srv.stop()
        st.close()


def test_idle_in_txn_server_default_overridable(tmp_path):
    """SET idle_in_transaction_session_timeout = 0 opts a session out of the
    server default (PG GUC precedence: session SET beats server config)."""
    import time

    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st, idle_in_transaction_timeout_s=0.1)
    srv.start()
    try:
        with connect(srv) as conn:
            conn.execute("SET idle_in_transaction_session_timeout = 0")
            conn.execute("SELECT 1")  # opens the transaction block
            time.sleep(0.6)
            assert conn.execute("SELECT 2").fetchone() == (2,)
    finally:
        srv.stop()
        st.close()


def test_idle_in_txn_show_and_reset_reflect_server_default(server):
    """SHOW reports the server-config value when the session never SET it, and
    RESET falls back to the server config, not the built-in 0."""
    with connect(server, autocommit=True) as conn:
        default = conn.execute("SHOW idle_in_transaction_session_timeout").fetchone()[0]
        assert default == "120000"
        conn.execute("SET idle_in_transaction_session_timeout = 5000")
        assert conn.execute("SHOW idle_in_transaction_session_timeout").fetchone()[0] == "5000"
        conn.execute("RESET idle_in_transaction_session_timeout")
        assert conn.execute("SHOW idle_in_transaction_session_timeout").fetchone()[0] == "120000"


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


def test_cross_type_comparison_sqlstate(server):
    """Comparing a text column against an integer is a parse-analysis failure in
    Postgres, so psycopg must see ``42883 undefined_function`` — not an empty
    result set. ``= '42'`` (an untyped literal) must keep working."""
    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TABLE ct (id bigint primary key, name text, age int)")
        conn.execute("INSERT INTO ct (id, name, age) VALUES (1, '42', 42)")
        with pytest.raises(psycopg.errors.UndefinedFunction) as ei:
            conn.execute("SELECT id FROM ct WHERE name = 42")
        assert ei.value.sqlstate == "42883"
        assert "operator does not exist: text = integer" in str(ei.value)
    with connect(server, autocommit=True) as conn:
        assert conn.execute("SELECT id FROM ct WHERE name = '42'").fetchall() == [(1,)]
        assert conn.execute("SELECT id FROM ct WHERE age = 42").fetchall() == [(1,)]
        # A bound parameter is typed from the column, so this resolves too.
        assert conn.execute("SELECT id FROM ct WHERE name = %s", ("42",)).fetchall() == [(1,)]


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


# -- RowDescription type-OID fidelity ----------------------------------------- #


@pytest.fixture
def real_server(tmp_path):
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=storage)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        storage.close()


def test_row_description_oids_for_computed_columns(real_server):
    """Computed / derived result columns must describe with real Postgres OIDs —
    psycopg's typed loaders key off them (tests/types/* in the psycopg gauge)."""
    with connect(real_server, autocommit=True) as conn:
        conn.execute("CREATE TABLE oidt (a int4, sm int2, f real, b boolean, arr int4[])")
        conn.execute("INSERT INTO oidt VALUES (2, 1, 1.5, true, '{1,2}')")
        cases = [
            ("select true", 16),
            ("select 1", 23),
            ("select 1::int2", 21),
            ("select 1.5::float4", 700),
            ("select 1.5", 1700),  # unadorned decimal constant is numeric
            ("select '{1,2}'::int4[]", 1007),
            ("select array[1,2,3]", 1007),
            ("select 1 + 1", 23),
            ("select case when true then 1 else 2 end", 23),
            ("select count(*) from oidt", 20),
            ("select sum(a) from oidt", 20),  # sum(int) -> bigint
            ("select avg(a) from oidt", 1700),  # avg(int) -> numeric
            ("select a * 2 from oidt", 23),
            ("select sm from oidt", 21),
            ("select f from oidt", 700),
            ("select arr from oidt", 1007),
            ("select a > 1 from oidt", 16),
            ("select case when a > 1 then a * 2 else a end from oidt", 23),
        ]
        for sql, want in cases:
            got = conn.execute(sql).description[0].type_code
            assert got == want, f"{sql}: expected OID {want}, got {got}"


def test_row_description_oid_for_bound_parameter(real_server):
    """``SELECT $1`` describes with the OID the client declared in Parse."""
    with connect(real_server, autocommit=True) as conn:
        cur = conn.execute("select %s", (True,))
        assert cur.description[0].type_code == 16
        assert cur.fetchone() == (True,)
        cur = conn.execute("select %s", (1,))
        # psycopg declares the smallest fitting int type (int2 for 1).
        assert cur.description[0].type_code == 21
        assert cur.fetchone() == (1,)


def test_binary_result_format_arrays(real_server):
    """Array results requested in binary format decode correctly — the array OID
    engages psycopg's binary array parser, so the wire bytes must be the real
    binary array layout, not text bytes."""
    with connect(real_server, autocommit=True) as conn:
        assert conn.execute("select '{1,2,3}'::int4[]", binary=True).fetchone() == ([1, 2, 3],)
        assert conn.execute("select '{a,b}'::text[]", binary=True).fetchone() == (["a", "b"],)
        assert conn.execute("select '{}'::int4[]", binary=True).fetchone() == ([],)
        assert conn.execute("select '{1,NULL}'::int8[]", binary=True).fetchone() == ([1, None],)


def test_binary_array_parameter_roundtrip(real_server):
    """A list bound as a binary parameter decodes into a real array value."""
    with connect(real_server, autocommit=True) as conn:
        conn.execute("CREATE TABLE arrp (a int4[])")
        conn.execute("INSERT INTO arrp VALUES (%s)", ([1, 2, 3],))
        assert conn.execute("SELECT a FROM arrp").fetchone() == ([1, 2, 3],)


def test_pg_typeof_over_the_wire(real_server):
    """psycopg's type tests assert ``select pg_typeof(%s::T) = 'T'::regtype``."""
    with connect(real_server, autocommit=True) as conn:
        cur = conn.execute("select pg_typeof(%s::int2) = 'smallint'::regtype", (1,))
        assert cur.fetchone() == (True,)
        assert conn.execute("select pg_typeof(1.5)").fetchone() == ("numeric",)
        cur = conn.execute("select pg_typeof(now())")
        assert cur.fetchone() == ("timestamp with time zone",)


def test_executemany_returning_describes_columns(real_server):
    """DML with RETURNING must answer Describe with a RowDescription — NoData
    followed by DataRows is a protocol violation (psycopg's pipelined
    executemany crashes with 'server sent data without prior row description')."""
    with connect(real_server, autocommit=True) as conn:
        conn.execute("CREATE TABLE em (num int4, data text)")
        cur = conn.cursor()
        cur.executemany(
            "insert into em(num, data) values (%s, %s) returning num",
            [(10, "hello"), (20, "world")],
        )
        cur.executemany(
            "insert into em(num, data) values (%s, %s) returning num",
            [(30, "a"), (40, "b")],
            returning=True,
        )
        assert cur.fetchone() == (30,)
        assert cur.nextset()
        assert cur.fetchone() == (40,)
        rows = conn.execute("select num from em order by num").fetchall()
        assert rows == [(10,), (20,), (30,), (40,)]


@pytest.mark.parametrize("fmt_in", ["s", "t", "b"])
@pytest.mark.parametrize("fmt_out", [False, True])
def test_text_array_full_charset_roundtrip(real_server, fmt_in, fmt_out):
    """chr(1)..chr(255) + '€' round-trip through every param/result format combo.
    Guards three bugs: binary array params stringified via Python repr in
    substitute_parameters; array elements lost to over-broad whitespace
    stripping in the literal parser (\\x1c is isspace() to Python, data to
    Postgres); unquoted whitespace emitted by the array renderer."""
    a = list(map(chr, range(1, 256))) + ["€"]
    with connect(real_server, autocommit=True) as conn:
        cur = conn.cursor(binary=fmt_out)
        (res,) = cur.execute(f"select %{fmt_in}::text[]", (a,)).fetchone()
        assert res == a


@pytest.mark.parametrize("fmt_in", ["s", "t", "b"])
@pytest.mark.parametrize("fmt_out", [False, True])
def test_bytea_array_roundtrip(real_server, fmt_in, fmt_out):
    a = [bytes(range(0, 256))]
    with connect(real_server, autocommit=True) as conn:
        cur = conn.cursor(binary=fmt_out)
        (res,) = cur.execute(f"select %{fmt_in}::bytea[]", (a,)).fetchone()
        assert res == a


@pytest.mark.parametrize(
    ("enc", "probe"),
    [("latin1", "ä"), ("latin9", "€"), ("utf8", "€漢")],
)
def test_client_encoding_roundtrip(real_server, enc, probe):
    """SET client_encoding converts query text, text/binary params, and text/
    binary results at the wire boundary; ParameterStatus reports the canonical
    Postgres spelling so psycopg switches its own codec."""
    with connect(real_server, autocommit=True) as conn:
        conn.execute(f"set client_encoding to '{enc}'")
        assert (
            conn.info.parameter_status("client_encoding")
            == {
                "latin1": "LATIN1",
                "latin9": "LATIN9",
                "utf8": "UTF8",
            }[enc]
        )
        assert conn.execute(f"select '{probe}'").fetchone() == (probe,)
        assert conn.execute("select %s::text", (probe,)).fetchone() == (probe,)
        assert conn.execute("select %b::text", (probe,)).fetchone() == (probe,)
        cur = conn.cursor(binary=True)
        assert cur.execute("select %s::text", (probe,)).fetchone() == (probe,)


def test_client_encoding_invalid_value(real_server):
    with (
        connect(real_server, autocommit=True) as conn,
        pytest.raises(psycopg.errors.InvalidParameterValue),
    ):
        conn.execute("set client_encoding to 'klingon'")


def test_client_encoding_startup_parameter(real_server):
    host, port = real_server.address
    with psycopg.connect(
        host=host, port=port, dbname="db", user="joe", client_encoding="latin1"
    ) as conn:
        assert conn.info.parameter_status("client_encoding") == "LATIN1"
        assert conn.execute("select 'ä'").fetchone() == ("ä",)


def test_stream_set_returning_function(real_server):
    """cur.stream() (libpq single-row mode) describes before executing — the
    Describe path must resolve a set-returning row source's shape instead of
    evaluating the SRF as a scalar (which errored every stream test)."""
    with connect(real_server, autocommit=True) as conn:
        cur = conn.cursor()
        assert list(cur.stream("select generate_series(1, 5) as a")) == [
            (1,),
            (2,),
            (3,),
            (4,),
            (5,),
        ]
        assert list(cur.stream("select generate_series(2, 1) as a")) == []
        assert list(cur.stream("select * from generate_series(1, 3)")) == [(1,), (2,), (3,)]


def test_text_param_nul_byte_rejected(real_server):
    """Postgres rejects NUL in text values (22021 -> DataError)."""
    with connect(real_server, autocommit=True) as conn, pytest.raises(psycopg.DataError):
        conn.execute("select %b::text", ("foo\x00bar",))


def test_untranslatable_character_errors(real_server):
    """A result character with no equivalent in client_encoding raises 22P05
    (DataError), matching Postgres — not a silent '?' substitution."""
    with connect(real_server, autocommit=True) as conn:
        conn.execute("set client_encoding to latin1")
        with pytest.raises(psycopg.DataError):
            conn.execute("select chr(%s)::text", (8364,))  # '€' not in latin1


def test_numeric_special_values_binary(real_server):
    """NaN and ±Infinity ride the binary numeric format (signs 0xC000/0xD000/
    0xF000) in both directions."""
    with connect(real_server, autocommit=True) as conn:
        cur = conn.cursor(binary=True)
        assert cur.execute("select %b::numeric", (Decimal("Infinity"),)).fetchone() == (
            Decimal("Infinity"),
        )
        assert cur.execute("select %b::numeric", (Decimal("-Infinity"),)).fetchone() == (
            Decimal("-Infinity"),
        )
        (nan,) = cur.execute("select %b::numeric", (Decimal("NaN"),)).fetchone()
        assert nan.is_nan()


def test_quoted_builtin_type_names_in_ddl(real_server):
    """psycopg's faker fixture emits CREATE TABLE with sql.Identifier(type)
    columns ('"cidr"'), which must resolve as the built-in, not an enum."""
    from ipaddress import IPv4Network

    with connect(real_server, autocommit=True) as conn:
        conn.execute('create table qt (c "cidr", a "text"[], n "numeric")')
        conn.execute("insert into qt values (%s, %s, %s)", ("10.0.0.0/24", ["x"], Decimal("1.5")))
        # psycopg decodes via the reported OIDs: cidr -> IPv4Network.
        assert conn.execute("select c, a, n from qt").fetchone() == (
            IPv4Network("10.0.0.0/24"),
            ["x"],
            Decimal("1.5"),
        )


def test_copy_bare_options_spelling(server):
    """psycopg emits ``COPY … TO STDOUT (FORMAT csv)`` without WITH — sqlglot
    only parses the WITH form, so parse() inserts it. Both the table and the
    query form take options, and the query form evaluates expressions."""
    with connect(server, autocommit=True) as conn:
        conn.execute("create table bo (a int4, b text)")
        conn.execute("insert into bo values (1, 'x'), (2, 'y')")
        cur = conn.cursor()
        with cur.copy("copy bo to stdout (format csv, header)") as copy:
            rows = [bytes(r) for r in copy]
        assert b"".join(rows) == b"a,b\n1,x\n2,y\n"
        with cur.copy("copy (select b from bo order by a desc) to stdout (format text)") as copy:
            rows = [bytes(r) for r in copy]
        assert b"".join(rows) == b"y\nx\n"
        with cur.copy("copy (select chr(8364)) to stdout (format text)") as copy:
            copy.set_types(["text"])
            assert copy.read_row() == ("€",)
            assert copy.read_row() is None


def test_oid_type_roundtrip(server):
    """``%s::oid`` describes with OID 26 and round-trips as an unsigned
    int4-like integer, in both the text and binary result formats."""
    with connect(server, autocommit=True) as conn:
        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            cur.execute("select %s::oid", (26,))
            assert cur.description[0].type_code == 26
            row = cur.fetchone()
            assert row == (26,)
            assert type(row[0]) is int
            # oid is unsigned: the full 32-bit range survives the round-trip.
            cur.execute("select %s::oid", (4294967295,))
            assert cur.fetchone() == (4294967295,)
            cur.execute("select '26'::oid")
            assert cur.fetchone() == (26,)
            # arrays: element OID 26, array OID 1028.
            cur.execute("select array[%s::oid]", (21,))
            assert cur.description[0].type_code == 1028
            assert cur.fetchone() == ([21],)


def test_oid_column_in_table(server):
    with connect(server, autocommit=True) as conn:
        conn.execute("create table oids (c oid)")
        conn.execute("insert into oids values (%s)", (26,))
        cur = conn.execute("select c from oids")
        assert cur.description[0].type_code == 26
        assert cur.fetchone() == (26,)


def test_pg_typeof_param_cast_to_oid(server):
    """psycopg's test_repr_wrapper pattern: ``pg_typeof(%s)::oid`` resolves to
    the declared parameter type's OID (including oid itself)."""
    from psycopg.types.numeric import Int2, Int8, Oid

    with connect(server, autocommit=True) as conn:
        assert conn.execute("select pg_typeof(%s)::oid", [Int2(0)]).fetchone() == (21,)
        assert conn.execute("select pg_typeof(%s)::oid", [Int8(0)]).fetchone() == (20,)
        assert conn.execute("select pg_typeof(%s)::oid", [Oid(0)]).fetchone() == (26,)


def test_regtype_from_oid_over_wire(server):
    from psycopg.types.numeric import Int4

    with connect(server, autocommit=True) as conn:
        assert conn.execute("select 21::regtype").fetchone() == ("smallint",)
        assert conn.execute("select pg_typeof(%s) = %s::regtype", [Int4(1), 23]).fetchone() == (
            True,
        )
        with pytest.raises(psycopg.errors.UndefinedObject):
            conn.execute("select 99999::regtype")


def test_declared_param_oid_drives_result_encoding(server):
    """The RowDescription for a bare ``select $1`` reports the OID the client
    declared in Parse (Int2 -> 21, Float4 -> 700, ...). The DataRow encoding
    must match that claim for every param wire format x result format combo —
    the execute path used to encode the engine-inferred type instead, sending
    e.g. 4-byte int4 (or raw text) bytes in a column described as binary int2."""
    from psycopg.types.numeric import Float4, Float8, Int2, Int4, Int8, Oid

    cases = [(Int2, 21), (Int4, 21), (Int8, 21), (Oid, 21), (Float4, 1.5), (Float8, 1.5)]
    with connect(server, autocommit=True) as conn:
        for fmt in ("s", "t", "b"):
            for binary in (False, True):
                for wrapper, val in cases:
                    cur = conn.cursor(binary=binary)
                    got = cur.execute(f"select %{fmt}", (wrapper(val),)).fetchone()[0]
                    assert got == val, (fmt, binary, wrapper.__name__)


def test_text_format_numeric_param_binary_result(server):
    """A text-format Decimal param (declared numeric in Parse) selected under a
    binary cursor: the server used to send the text bytes with fmt=1, which the
    client rejects as a malformed binary numeric ('bad value for numeric sign')."""
    with connect(server, autocommit=True) as conn:
        cur = conn.cursor(binary=True)
        assert cur.execute("select %t", (Decimal("21"),)).fetchone() == (Decimal("21"),)
        assert cur.execute("select %t", (Decimal("-19.99"),)).fetchone() == (Decimal("-19.99"),)


def test_string_cast_converts_value(server):
    """``'42'::int`` is the integer 42 — as a comparison operand and on the
    wire (a str passing through the cast used to be sent as text bytes in a
    column whose RowDescription claims a binary numeric OID)."""
    with connect(server, autocommit=True) as conn:
        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            assert cur.execute("select '42'::smallint, '1'::int, '1.5'::float8").fetchone() == (
                42,
                1,
                1.5,
            )
            assert cur.execute("select '19.99'::numeric").fetchone() == (Decimal("19.99"),)
            assert cur.execute("select 't'::boolean, 'f'::boolean").fetchone() == (True, False)
        assert conn.execute("select '42'::smallint = %s", (42,)).fetchone() == (True,)


def test_declared_param_type_governs_text_format(server):
    """A text-format param IS a value of its declared type, exactly like its
    binary twin: ``'…'::numeric = $1`` with a text-format Decimal must be true
    (the str used to survive into the comparison and compare as text)."""
    big = 2**63  # psycopg dumps ints beyond int8 as numeric
    with connect(server, autocommit=True) as conn:
        assert conn.execute(f"select '{big}'::numeric = %t", (big,)).fetchone() == (True,)
        assert conn.execute("select '1'::int = %t", (1,)).fetchone() == (True,)
        assert conn.execute("select 1.5 = %t", (1.5,)).fetchone() == (True,)


def test_float_special_values_roundtrip(server):
    """inf/-inf/nan floats as parameters in every wire format (``repr(inf)``
    is not a parseable SQL literal; the substituted node must carry the
    Postgres spelling through a float8 cast)."""
    import math

    with connect(server, autocommit=True) as conn:
        for fmt in ("s", "t", "b"):
            for val in (float("inf"), float("-inf")):
                assert conn.execute(f"select %{fmt} = '{val:F}'::float8", (val,)).fetchone() == (
                    True,
                ), (fmt, val)
            (nan,) = conn.execute(f"select %{fmt}", (float("nan"),)).fetchone()
            assert math.isnan(nan), fmt


def test_wide_numeric_binary_param(server):
    """Binary numeric params wider than the default Decimal context precision
    (28 significant digits) must decode exactly, not raise / round."""
    wide = [
        Decimal("9999999999999999999999999999.9"),
        Decimal("1000000000000000000000000000.001"),
        Decimal("-123456789012345678901234567890.123456789"),
    ]
    with connect(server, autocommit=True) as conn:
        for d in wide:
            assert conn.execute("select %b", (d,)).fetchone() == (d,), d
            assert conn.execute("select %b::text", (d,)).fetchone() == (str(d),), d


def test_copy_runs_inside_the_open_transaction(server):
    """COPY must run inside the session's transaction block: it sees
    same-transaction DDL (psycopg's fixtures do CREATE TABLE + COPY in one
    block), reads pending rows on the way out, and its writes roll back with
    the block — previously the COPY path ran outside ``use_user_transaction``,
    so the CREATE was invisible and copied rows survived a ROLLBACK."""
    with connect(server) as conn:
        cur = conn.cursor()
        # CREATE + COPY IN + read back, all in one uncommitted block.
        cur.execute("create table ctx (id serial primary key, t text)")
        with cur.copy("copy ctx (t) from stdin") as copy:
            copy.write_row(("alpha",))
        cur.execute("select t from ctx")
        assert cur.fetchall() == [("alpha",)]
        conn.commit()

        # COPY OUT sees rows inserted earlier in the same block.
        cur.execute("insert into ctx (t) values ('beta')")
        with cur.copy("copy ctx (t) to stdout") as copy:
            data = b"".join(bytes(c) for c in copy)
        assert data == b"alpha\nbeta\n"
        conn.rollback()

        # Rows copied inside an aborted block vanish with it.
        with cur.copy("copy ctx (t) from stdin") as copy:
            copy.write_row(("gamma",))
        conn.rollback()
        cur.execute("select t from ctx")
        assert cur.fetchall() == [("alpha",)]
        conn.commit()


def test_server_side_cursor_lifecycle(server):
    """psycopg's ServerCursor: DECLARE via the extended protocol, then a wire
    Describe('P', name) — a DECLAREd cursor IS a portal — then FETCH
    statements, pg_cursors visibility, parameterized declarations (the $N
    lives inside the raw Command tail), and Close."""
    with connect(server) as conn:
        setup = conn.cursor()
        setup.execute("create table sc (a int4)")
        setup.execute("insert into sc values (1), (2), (3)")
        conn.commit()

        with conn.cursor(name="foo") as cur:
            cur.execute("select a from sc order by a")
            assert cur.fetchone() == (1,)
            assert cur.fetchmany(2) == [(2,), (3,)]
            setup.execute("select name, is_holdable, statement from pg_cursors")
            (name, hold, statement) = setup.fetchone()
            assert (name, hold) == ("foo", False)
            assert "select a from sc" in statement

        # Parameterized DECLARE: psycopg sends the $1 inside the DECLARE text.
        with conn.cursor(name="pc") as cur:
            cur.execute("select %s::text", ("hello",))
            assert cur.fetchall() == [("hello",)]

        # Iteration in itersize batches (FETCH FORWARD n loops).
        with conn.cursor(name="itc") as cur:
            cur.itersize = 2
            cur.execute("select a from sc order by a")
            assert list(cur) == [(1,), (2,), (3,)]

        setup.execute("select count(*) from pg_cursors")
        assert setup.fetchone() == (0,)
        conn.commit()


def test_pg_prepared_statements_lists_wire_prepared(server):
    with connect(server) as conn:
        cur = conn.cursor()
        # psycopg prepares after prepare_threshold executions; force it.
        for _ in range(2):
            cur.execute("select 1", prepare=True)
        cur.execute("select name, from_sql from pg_prepared_statements")
        rows = cur.fetchall()
        assert rows and all(fs is False for _n, fs in rows)


def test_connection_teardown_releases_wt_session(server):
    """Every pg connection thread caches a WT session; the handler's teardown
    must release it (like the Mongo server does). Left leaked, dead threads'
    positioned cursors pin cache pages until WT eviction livelocks with an
    application thread stuck in __wt_cache_eviction_worker holding the storage
    RLock — the full psycopg gauge wedged at ~400 connections, 3 runs of 3."""
    import time as _time

    storage = server.storage
    with connect(server) as conn:
        conn.execute("create table wt_leak (a int4)")
        conn.commit()
    baseline = len(storage._all_sessions)
    for _ in range(8):
        with connect(server) as conn:
            # Must WRITE — reads use per-call sessions; it's the write path
            # that caches the per-thread WT session (verified: 8 writer
            # connections leaked 8 sessions before the teardown fix).
            conn.execute("insert into wt_leak values (1)")
            conn.commit()
    # Teardown runs on the handler thread after the client closes; give it a beat.
    deadline = _time.monotonic() + 10
    while _time.monotonic() < deadline:
        if len(storage._all_sessions) <= baseline + 1:
            break
        _time.sleep(0.05)
    assert len(storage._all_sessions) <= baseline + 1, (
        f"WT sessions leaked: {baseline} -> {len(storage._all_sessions)} after 8 connections"
    )


def test_enum_result_oid_registers_with_psycopg(server):
    """RowDescription reports an enum result column with the enum's pg_type oid,
    so psycopg's catalog-driven ``EnumInfo.fetch`` + ``register_enum`` flow works
    end-to-end: the fetched oid matches ``cursor.description.type_code`` and the
    registered loader turns result values into Python enum members."""
    import enum

    from psycopg.types.enum import EnumInfo, register_enum

    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')")
        conn.execute("CREATE TABLE moods (id bigint primary key, m mood)")
        conn.execute("INSERT INTO moods VALUES (1, 'happy')")

        info = EnumInfo.fetch(conn, "mood")
        assert info is not None
        assert info.labels == ["sad", "ok", "happy"]

        cur = conn.execute("SELECT m FROM moods")
        assert cur.description[0].type_code == info.oid
        assert cur.fetchone() == ("happy",)  # unregistered: the label as str

        Mood = enum.Enum("Mood", {label: label for label in info.labels})
        register_enum(info, conn, Mood)
        assert conn.execute("SELECT m FROM moods").fetchone() == (Mood.happy,)
        assert conn.execute("INSERT INTO moods VALUES (2, 'sad') RETURNING m").fetchone() == (
            Mood.sad,
        )


def test_enum_cast_and_param_validation(server):
    """The cast/param side of enum conformance: ``%s::mood`` describes with the
    enum oid so a registered loader fires on cast results; a parameter declared
    with an enum oid (a registered dumper's Bind) is label-validated (22P02);
    and ``%s::mood[]`` round-trips as a list through the minted array oid."""
    import enum

    from psycopg.types.enum import EnumInfo, register_enum

    class Mood(str, enum.Enum):
        sad = "sad"
        ok = "ok"
        happy = "happy"

    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')")
        info = EnumInfo.fetch(conn, "mood")
        assert info.array_oid > 0
        register_enum(info, conn, Mood)

        for binary in (False, True):
            cur = conn.execute("SELECT %s::mood", ["happy"], binary=binary)
            assert cur.description[0].type_code == info.oid
            assert cur.fetchone() == (Mood.happy,)

        cur = conn.execute("SELECT %s::mood[]", [["ok", "sad"]])
        assert cur.description[0].type_code == info.array_oid
        assert cur.fetchone() == ([Mood.ok, Mood.sad],)

        with pytest.raises(psycopg.errors.DataError):
            conn.execute("SELECT 'nope'::mood")
        # A registered dumper binds Mood params with the enum oid; a label the
        # type doesn't have is rejected at Bind like real Postgres.
        conn.execute("CREATE TYPE other AS ENUM ('X')")
        other = EnumInfo.fetch(conn, "other")
        register_enum(other, conn, Mood)  # deliberately wrong mapping
        with pytest.raises(psycopg.errors.DataError):
            conn.execute("SELECT %s", [Mood.ok])


def test_enum_oid_through_plan_shapes_and_enum_arrays(server):
    """The enum oid survives GROUP BY / JOIN / DISTINCT plan shapes (a registered
    loader fires on those results too, in the simple and extended protocols),
    and ``mood[]`` table columns store label arrays, validate elements, and
    report the minted array oid so a registered loader returns enum members."""
    import enum

    from psycopg.types.enum import EnumInfo, register_enum

    class Mood(str, enum.Enum):
        sad = "sad"
        ok = "ok"
        happy = "happy"

    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')")
        conn.execute("CREATE TABLE people (name text primary key, m mood, ms mood[])")
        conn.execute("INSERT INTO people VALUES ('a', 'sad', '{sad}'), ('b', 'ok', '{ok,happy}')")
        info = EnumInfo.fetch(conn, "mood")
        register_enum(info, conn, Mood)

        cur = conn.execute("SELECT m, count(*) FROM people GROUP BY m ORDER BY m")
        assert cur.description[0].type_code == info.oid
        assert cur.fetchall() == [(Mood.sad, 1), (Mood.ok, 1)]

        cur = conn.execute("SELECT DISTINCT m FROM people ORDER BY m")
        assert cur.fetchall() == [(Mood.sad,), (Mood.ok,)]

        conn.execute("CREATE TABLE teams (name text primary key, lead text)")
        conn.execute("INSERT INTO teams VALUES ('x', 'b')")
        cur = conn.execute("SELECT t.name, p.m FROM teams t JOIN people p ON p.name = t.lead")
        assert cur.description[1].type_code == info.oid
        assert cur.fetchall() == [("x", Mood.ok)]

        # Extended protocol (a bound parameter forces Parse/Describe/Execute).
        cur = conn.execute("SELECT m FROM people WHERE name = %s GROUP BY m", ["a"], binary=True)
        assert cur.description[0].type_code == info.oid
        assert cur.fetchall() == [(Mood.sad,)]

        # mood[] columns: minted array oid + registered loader → enum members.
        cur = conn.execute("SELECT ms FROM people ORDER BY name")
        assert cur.description[0].type_code == info.array_oid
        assert cur.fetchall() == [([Mood.sad],), ([Mood.ok, Mood.happy],)]

        with pytest.raises(psycopg.errors.DataError):
            conn.execute("INSERT INTO people VALUES ('c', 'ok', '{furious}')")


def test_typmod_in_row_description(server):
    """A modifier-bearing cast describes with its PG type modifier: psycopg's
    ``cursor.description`` derives ``display_size``/``precision``/``scale`` and
    ``type_display`` from the wire typmod, and varchar/bpchar keep their
    distinct oids (1043/1042) instead of folding onto text's 25."""
    with connect(server, autocommit=True) as conn:
        c = conn.execute("SELECT null::varchar(42)").description[0]
        assert (c.type_code, c.display_size) == (1043, 42)
        assert c.type_display == "varchar(42)"
        c = conn.execute("SELECT null::varchar").description[0]
        assert (c.type_code, c.display_size) == (1043, None)
        c = conn.execute("SELECT 3.14::numeric(10,2)").description[0]
        assert (c.precision, c.scale) == (10, 2)
        c = conn.execute("SELECT null::numeric(2,-3)").description[0]
        assert (c.precision, c.scale) == (2, -3)
        c = conn.execute("SELECT null::numeric(10,3)[]").description[0]
        assert c.type_display == "numeric(10,3)[]"
        c = conn.execute("SELECT null::timestamptz(6)").description[0]
        assert (c.type_code, c.precision) == (1184, 6)
        c = conn.execute("SELECT null::bit(8)").description[0]
        assert (c.type_code, c.display_size) == (1560, 8)
        c = conn.execute("SELECT null::varbit(9)").description[0]
        assert (c.type_code, c.display_size) == (1562, 9)
        # Extended protocol (Describe) reports the same modifier.
        c = conn.execute("SELECT null::varchar(7) WHERE 1 = %s", [1]).description[0]
        assert (c.type_code, c.display_size) == (1043, 7)


def test_escape_string_literal_in_insert(server):
    """psycopg's ClientCursor interpolates any string containing a backslash as
    an ``E'…'`` escape-string literal; sqlglot lexes that as a ByteString, which
    the INSERT value path must unescape (it previously raised 0A000 -- the
    test_leak flap)."""
    with connect(server, autocommit=True, cursor_factory=psycopg.ClientCursor) as conn:
        conn.execute("CREATE TABLE esc (id int primary key, t text)")
        conn.execute("INSERT INTO esc VALUES (%s, %s)", [1, "a\\b\nc"])
        conn.execute("INSERT INTO esc VALUES (2, E'x\\\\y')")
        assert conn.execute("SELECT t FROM esc ORDER BY id").fetchall() == [
            ("a\\b\nc",),
            ("x\\y",),
        ]


def test_transaction_characteristics(server):
    """BEGIN/SET TRANSACTION characteristics apply for the transaction and are
    reported via the transaction_* GUCs, which mirror their session defaults
    (default_transaction_*) until overridden; psycopg's set_isolation_level /
    set_read_only / set_deferrable drive exactly this machinery."""
    with connect(server) as conn:
        conn.set_isolation_level(psycopg.IsolationLevel.SERIALIZABLE)
        conn.set_read_only(True)
        cur = conn.execute(
            "select current_setting('transaction_isolation'),"
            " current_setting('transaction_read_only')"
        )
        assert cur.fetchone() == ("serializable", "on")
        conn.rollback()
        conn.set_isolation_level(None)
        conn.set_read_only(None)
        # Characteristics revert at transaction end; defaults mirror through.
        conn.execute("select set_config('default_transaction_isolation', 'repeatable read', false)")
        conn.commit()
        cur = conn.execute("select current_setting('transaction_isolation')")
        assert cur.fetchone() == ("repeatable read",)
        conn.rollback()
    with connect(server, autocommit=True) as conn:
        conn.execute("BEGIN ISOLATION LEVEL READ UNCOMMITTED READ WRITE")
        assert conn.execute("select current_setting('transaction_isolation')").fetchone() == (
            "read uncommitted",
        )
        conn.execute("SET TRANSACTION DEFERRABLE")
        assert conn.execute("select current_setting('transaction_deferrable')").fetchone() == (
            "on",
        )
        conn.execute("ROLLBACK")
        assert conn.execute("select current_setting('transaction_deferrable')").fetchone() == (
            "off",
        )
        conn.execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
        assert conn.execute("select current_setting('transaction_read_only')").fetchone() == ("on",)


def test_binary_copy_roundtrip(server):
    """``COPY … (FORMAT binary)`` both directions: OUT emits the PGCOPY stream
    (header bundled with the first row, per-row CopyData, int16 -1 trailer)
    with per-type binary field encodings; IN parses the same layout and decodes
    each field by the target column's type."""
    with connect(server, autocommit=True) as conn:
        conn.execute(
            "CREATE TABLE bc (id int primary key, n int, t text, f float8, "
            "b bool, by bytea, ts timestamptz)"
        )
        when = _dt.datetime(2021, 3, 4, 5, 6, 7, tzinfo=_dt.timezone.utc)
        rows = [
            (1, 42, "hello", 3.5, True, b"\x00\x01", when),
            (2, None, None, None, None, None, None),
        ]
        cur = conn.cursor()
        with cur.copy("COPY bc FROM STDIN (FORMAT binary)") as copy:
            copy.set_types(["int4", "int4", "text", "float8", "bool", "bytea", "timestamptz"])
            for row in rows:
                copy.write_row(row)
        got = []
        with cur.copy("COPY bc TO STDOUT (FORMAT binary)") as copy:
            copy.set_types(["int4", "int4", "text", "float8", "bool", "bytea", "timestamptz"])
            for row in copy.rows():
                got.append(row)
        assert got == rows
        # Query-form binary COPY OUT rides the same encoders.
        with cur.copy("COPY (SELECT n FROM bc WHERE id = 1) TO STDOUT (FORMAT binary)") as copy:
            copy.set_types(["int4"])
            assert list(copy.rows()) == [(42,)]


def test_group_by_ordinal_over_wire(server):
    with connect(server, autocommit=True) as conn:
        conn.execute("CREATE TABLE g (id int primary key, k text)")
        conn.execute("INSERT INTO g VALUES (1, 'a'), (2, 'a'), (3, 'b')")
        rows = conn.execute("SELECT k, count(*) FROM g GROUP BY 1 ORDER BY 1").fetchall()
        assert rows == [("a", 2), ("b", 1)]


def test_do_block_raise_and_notices(server):
    """Minimal plpgsql DO blocks: RAISE NOTICE/WARNING surface as psycopg
    notices, RAISE EXCEPTION raises with its USING ERRCODE, and EXECUTE
    format(…) runs dynamic SQL whose errors keep their real SQLSTATE."""
    with connect(server, autocommit=True) as conn:
        messages = []
        conn.add_notice_handler(lambda d: messages.append((d.severity, d.message_primary)))
        conn.execute("do $$begin raise notice 'hello %', 42; end$$ language plpgsql")
        assert ("NOTICE", "hello 42") in messages
        with pytest.raises(psycopg.errors.RaiseException):
            conn.execute("do $$begin raise exception 'boom'; end$$ language plpgsql")
        with pytest.raises(psycopg.Error) as ei:
            conn.execute("do $$begin raise exception 'custom' using errcode = 'PXX99'; end$$")
        assert ei.value.sqlstate == "PXX99"
        with pytest.raises(psycopg.errors.UndefinedTable):
            conn.execute("do $$begin execute format('insert into %I values (1)', 'nope'); end$$")


def test_pg_terminate_backend_via_wire(server):
    # The kill effect (socket teardown) is exercised by the gauge's
    # isolated-subprocess connection tests; here we pin the deterministic
    # return-value logic (a live backend -> True, a bogus pid -> False),
    # which is race-free in the in-process shared-worker fixture.
    with connect(server, autocommit=True) as victim, connect(server, autocommit=True) as killer:
        pid = victim.execute("select pg_backend_pid()").fetchone()[0]
        assert killer.execute(f"select pg_terminate_backend({pid})").fetchone() == (True,)
        assert killer.execute("select pg_terminate_backend(999999)").fetchone() == (False,)


def test_prepared_statement_introspection(server):
    """pg_prepared_statements reports the ORIGINAL query text, real
    prepare_time, and the parameter types; DEALLOCATE ALL clears the
    wire-prepared registry."""
    with connect(server, autocommit=True) as conn:
        conn.execute("select %s::date", ("2021-01-01",), prepare=True)
        rows = conn.execute(
            "select statement, parameter_types from pg_prepared_statements"
        ).fetchall()
        assert rows == [("select %s::date".replace("%s", "$1"), ["date"])]
        conn.execute("deallocate all")
        assert conn.execute("select count(*) from pg_prepared_statements").fetchone() == (0,)


def test_jsonb_roundtrip_and_navigation(server):
    """jsonb values parse at ingress: a cast or Json/Jsonb-wrapped parameter
    loads back as a Python dict/list (not double-encoded text), ``->>``
    navigates it, and 22P02 surfaces for malformed json."""
    from psycopg.types.json import Json, Jsonb

    with connect(server, autocommit=True) as conn:
        assert conn.execute("""select '{"a": 100}'::jsonb""").fetchone() == ({"a": 100},)
        assert conn.execute("""select '{"foo":"bar"}'::jsonb ->> 'foo'""").fetchone() == ("bar",)
        for wrapper in (Json, Jsonb):
            got = conn.execute("select %s", [wrapper({"foo": "bar"})]).fetchone()[0]
            assert got == {"foo": "bar"}
        assert conn.execute("select %s::jsonb", ["[1, 2]"]).fetchone() == ([1, 2],)
        with pytest.raises(psycopg.errors.DataError):
            conn.execute("select 'nope'::json")


def test_datetime_session_gucs_over_wire(server):
    """TimeZone governs timestamptz input/output; DateStyle reformats date and
    timestamp text; set_config on a reportable GUC emits ParameterStatus."""
    with connect(server, autocommit=True) as conn:
        conn.execute("set timezone to '-02:00'")  # POSIX: UTC+2
        got = conn.execute("select '2000-01-01'::timestamptz").fetchone()[0]
        assert got == _dt.datetime(1999, 12, 31, 22, tzinfo=_dt.timezone.utc)

        conn.execute("set datestyle = German, YMD")
        assert conn.pgconn.parameter_status(b"DateStyle") == b"German, YMD"
        assert conn.execute("select '2000-01-02'::date").fetchone()[0] == _dt.date(2000, 1, 2)

        conn.execute("select set_config('TimeZone', 'UTC', false)")
        assert conn.pgconn.parameter_status(b"TimeZone") == b"UTC"


def test_temporal_params_keep_their_type(server):
    """A bound datetime/interval/date parameter compares equal to the same value
    written as a cast literal (it used to substitute as bare text and silently
    compare false)."""
    with connect(server, autocommit=True) as conn:
        when = _dt.datetime(2000, 1, 2, 3, 4, 5, 6)
        assert conn.execute(
            "select '2000-01-02 03:04:05.000006'::timestamp = %s", [when]
        ).fetchone() == (True,)
        assert conn.execute(
            "select '1 day'::interval = %s", [_dt.timedelta(days=1)]
        ).fetchone() == (True,)
        assert conn.execute(
            "select '2020-05-06'::date + 1 = %s", [_dt.date(2020, 5, 7)]
        ).fetchone() == (True,)
        assert conn.execute("select '1 sec'::interval").fetchone() == (_dt.timedelta(seconds=1),)


def test_composite_registration_roundtrip(server):
    """Composite types materialize end-to-end: the minted (allocation-stable)
    OID rides RowDescription so a registered psycopg loader returns named
    tuples; row(...) builds anonymous records; casts parse record literals;
    field access types by the declared field."""
    from psycopg.types.composite import CompositeInfo, register_composite

    with connect(server, autocommit=True) as conn:
        conn.execute("create type testcomp as (foo text, bar int8, baz float8)")
        info = CompositeInfo.fetch(conn, "testcomp")
        assert tuple(info.field_names) == ("foo", "bar", "baz")
        register_composite(info, conn)

        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            row = cur.execute("select row('hello', 10, 20)::testcomp").fetchone()[0]
            assert (row.foo, row.bar, row.baz) == ("hello", 10, 20.0)
            assert isinstance(row.baz, float)

        got = conn.execute("select '(foo,42,3.14)'::testcomp").fetchone()[0]
        assert (got.foo, got.bar, got.baz) == ("foo", 42, 3.14)

        # A registered dumper binds params with the minted oid; pg_typeof sees
        # the type and field access types by the declared field.
        t = info.python_type("x", 7, 1.5)
        assert conn.execute("select pg_typeof(%s)::text, (%s).bar", [t, t]).fetchone() == (
            "testcomp",
            7,
        )
        # Anonymous records: psycopg's text loader yields strings; the binary
        # record layout carries per-field oids — an int literal comes back
        # typed and an UNTYPED string literal is unknown (705), which psycopg
        # loads as bytes, exactly like real Postgres.
        assert conn.execute("select row(1, 'a')").fetchone()[0] == ("1", "a")
        with conn.cursor(binary=True) as bcur:
            assert bcur.execute("select row(1, 'a')").fetchone()[0] == (1, b"a")
            assert bcur.execute("select row(1, 'a'::text)").fetchone()[0] == (1, "a")


def test_create_type_as_range(server):
    """CREATE TYPE … AS RANGE: RangeInfo/MultirangeInfo fetch through pg_type +
    pg_range (stable minted oids, companion multirange), registered loaders
    fire on casts and constructors, and params round-trip."""
    from psycopg.types.multirange import Multirange, MultirangeInfo, register_multirange
    from psycopg.types.range import Range, RangeInfo, register_range

    with connect(server, autocommit=True) as conn:
        conn.execute('create type textrange as range (subtype = text, collation = "C")')
        info = RangeInfo.fetch(conn, "textrange")
        assert info is not None and info.oid > 0
        assert info.oid != info.array_oid > 0
        assert info.subtype_oid == 25
        register_range(info, conn)

        got = conn.execute("select '[a,z)'::textrange").fetchone()[0]
        assert got == Range("a", "z", "[)")
        assert conn.execute(
            "select textrange('a', 'z', '[)') = %s", [Range("a", "z")]
        ).fetchone() == (True,)

        mr_info = MultirangeInfo.fetch(conn, "textmultirange")
        assert mr_info is not None and mr_info.range_oid == info.oid
        register_multirange(mr_info, conn)
        got = conn.execute("select '{[a,b)}'::textmultirange").fetchone()[0]
        assert got == Multirange([Range("a", "b", "[)")])

        conn.execute("drop type textrange cascade")
        assert RangeInfo.fetch(conn, "textrange") is None
