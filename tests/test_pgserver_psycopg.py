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
