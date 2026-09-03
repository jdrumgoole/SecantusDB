"""A fourteenth differential sweep — the EXTENDED QUERY PROTOCOL.

This is the path every real driver actually speaks. psycopg, JDBC and most
ORMs send Parse/Bind/Describe/Execute with typed parameters rather than the
interpolated SQL text a literal corpus produces, so it is a different server
path from everything the previous thirteen sweeps exercised.

**The value round-trips came back clean.** 126 checks — every scalar type
through Bind, parameters in predicates, in the select list, in `LIMIT`, in
`RETURNING` — each run three ways (text parameters, a server-side PREPARED
statement, and BINARY result format), with one divergence, and that one is
SecantusDB being more permissive than PostgreSQL about array element types.
Server-side cursors were clean too: `fetchmany`, `SCROLL` with a rewind,
parameterised cursor queries, and a cursor's death at COMMIT all matched.

What diverged was the ERROR surface, in three places where SecantusDB accepted
something PostgreSQL refuses:

**`EXPLAIN` with a parameter violated the wire protocol.** It described as
NoData and then sent DataRows — `server sent data ("D" message) without prior
row description`, which is a client-level crash rather than a wrong answer.
It bites precisely when the explained query carries a parameter, because that
is exactly when a driver stops using the simple protocol, so nothing in the
literal corpora could have found it. `_describe_statement` already had this
guard for FETCH, MOVE, CALL and EXECUTE; EXPLAIN was the one Command missing.

**`DECLARE CURSOR` outside a transaction block was accepted.** PostgreSQL
raises `25P01` because a non-holdable cursor would be discarded the instant
the implicit transaction committed. SecantusDB accepted the DECLARE and then
failed the *following* `FETCH` with `34000 cursor "c" does not exist` —
reporting the problem one statement late and blaming the wrong statement.
`WITH HOLD` is exempt, which is why the check cannot simply be "are we in a
transaction" — and so is the embedded `run_sql` API, which has no implicit
commit to discard the cursor. That last exemption is not cosmetic: at the point
of the check the two session states are INDISTINGUISHABLE (both show
`txn_handle=None, txn_is_implicit=False`, measured), so the wire server has to
say which it is.

**Parameters in DDL were accepted.** PostgreSQL binds them only into
statements whose body goes through the planner. The boundary is not the
obvious one and was measured, not reasoned: `CREATE TABLE t AS SELECT $1` is
accepted while `CREATE VIEW v AS SELECT $1` is rejected, so "carries a query"
is the wrong discriminator.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s14"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    host, port = srv.address
    c = psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True)
    try:
        yield c
    finally:
        c.close()
        srv.stop()
        st.close()


@pytest.fixture
def seeded(conn):
    conn.execute("CREATE TABLE z1 (a int)")
    conn.execute("INSERT INTO z1 SELECT generate_series(1,10)")
    return conn


def sqlstate(exc):
    return getattr(getattr(exc, "diag", None), "sqlstate", None)


# --- EXPLAIN through the extended protocol ---------------------------------- #


def test_explain_with_a_parameter_describes_before_it_sends_rows(conn):
    """The protocol violation: Describe said NoData, Execute sent DataRows.

    A parameter is what forces the extended protocol, so this shape is
    unreachable from a corpus of literal SQL."""
    r = conn.execute("EXPLAIN SELECT %s::int", (1,))
    assert [(d.name, d.type_code) for d in r.description] == [("QUERY PLAN", 25)]
    assert r.fetchall()  # and the rows still arrive


def test_explain_analyze_with_a_parameter(conn):
    r = conn.execute("EXPLAIN ANALYZE SELECT %s::int", (1,))
    assert [d.name for d in r.description] == ["QUERY PLAN"]
    assert r.fetchall()


def test_explain_of_a_parameterised_table_query(seeded):
    r = seeded.execute("EXPLAIN SELECT a FROM z1 WHERE a > %s", (5,))
    assert [d.name for d in r.description] == ["QUERY PLAN"]
    assert r.fetchall()


# --- DECLARE CURSOR needs a transaction block -------------------------------- #


def test_declare_cursor_outside_a_transaction_is_refused(seeded):
    with pytest.raises(psycopg.Error) as ei:
        seeded.execute("DECLARE cx CURSOR FOR SELECT a FROM z1")
    assert sqlstate(ei.value) == "25P01"
    assert "DECLARE CURSOR can only be used in transaction blocks" in str(ei.value)


def test_declare_with_hold_outside_a_transaction_is_allowed(seeded):
    """The exemption that stops the guard being a plain in-transaction check:
    a holdable cursor survives the implicit commit, so PostgreSQL permits it."""
    seeded.execute("DECLARE cy CURSOR WITH HOLD FOR SELECT a FROM z1")
    assert seeded.execute("FETCH 2 FROM cy").fetchall() == [(1,), (2,)]


def test_declare_inside_a_transaction_still_works(seeded):
    with seeded.transaction():
        seeded.execute("DECLARE cz CURSOR FOR SELECT a FROM z1 ORDER BY a")
        assert seeded.execute("FETCH 3 FROM cz").fetchall() == [(1,), (2,), (3,)]


# --- parameters are bound only into planned statements ----------------------- #


@pytest.mark.parametrize(
    "sql",
    [
        "CREATE TABLE zz (a int DEFAULT %s)",
        "CREATE TABLE zz (a int CHECK (a > %s))",
        "CREATE VIEW zv AS SELECT %s::int AS a",
        "CREATE INDEX zi ON z1 ((a + %s))",
        "ALTER TABLE z1 ALTER a SET DEFAULT %s",
    ],
)
def test_parameters_in_ddl_are_refused(seeded, sql):
    with pytest.raises(psycopg.Error) as ei:
        seeded.execute(sql, (1,))
    assert sqlstate(ei.value) == "42P02"
    assert "there is no parameter $1" in str(ei.value)


@pytest.mark.parametrize(
    "sql,args",
    [
        ("SELECT %s::int", (1,)),
        ("INSERT INTO z1 VALUES (%s)", (1,)),
        ("UPDATE z1 SET a = %s WHERE a = 1", (2,)),
        ("DELETE FROM z1 WHERE a = %s", (99,)),
        ("VALUES (%s::int)", (1,)),
        # CREATE TABLE ... AS is planned, so it takes parameters — while
        # CREATE VIEW, which also carries a query, does not.
        ("CREATE TABLE zc2 AS SELECT %s::int AS a", (1,)),
        ("EXPLAIN SELECT %s::int", (1,)),
    ],
)
def test_parameters_in_planned_statements_are_accepted(seeded, sql, args):
    seeded.execute(sql, args)


def test_create_table_as_and_create_view_differ(seeded):
    """Pinned as a pair, because the two look alike and behave differently —
    which is what makes 'carries a query' the wrong rule."""
    seeded.execute("CREATE TABLE ok_ctas AS SELECT %s::int AS a", (1,))
    with pytest.raises(psycopg.Error) as ei:
        seeded.execute("CREATE VIEW bad_view AS SELECT %s::int AS a", (1,))
    assert sqlstate(ei.value) == "42P02"


# --- regression cover for the paths that were already correct ---------------- #


def test_server_side_cursor_fetchmany(seeded):
    with seeded.transaction(), seeded.cursor(name="c1") as cur:
        cur.itersize = 3
        cur.execute("SELECT a FROM z1 ORDER BY a")
        assert cur.fetchmany(3) == [(1,), (2,), (3,)]
        assert cur.fetchmany(3) == [(4,), (5,), (6,)]
        assert cur.fetchall() == [(7,), (8,), (9,), (10,)]


def test_server_side_cursor_scroll_rewind(seeded):
    with seeded.transaction(), seeded.cursor(name="c2", scrollable=True) as cur:
        cur.execute("SELECT a FROM z1 ORDER BY a")
        assert cur.fetchmany(2) == [(1,), (2,)]
        cur.scroll(0, mode="absolute")
        assert cur.fetchmany(2) == [(1,), (2,)]


def test_server_side_cursor_with_parameters(seeded):
    with seeded.transaction(), seeded.cursor(name="c3") as cur:
        cur.execute("SELECT a FROM z1 WHERE a > %s ORDER BY a", (7,))
        assert cur.fetchall() == [(8,), (9,), (10,)]


def test_cursor_does_not_outlive_its_transaction(seeded):
    with seeded.transaction():
        cur = seeded.cursor(name="c4")
        cur.execute("SELECT a FROM z1 ORDER BY a")
        assert cur.fetchmany(2) == [(1,), (2,)]
    with pytest.raises(psycopg.Error):
        cur.fetchmany(2)


@pytest.mark.parametrize("prepare", [False, True])
@pytest.mark.parametrize("binary", [False, True])
def test_parameter_round_trip_in_every_binding_mode(seeded, prepare, binary):
    """Text params, a server-side prepared statement, and binary results are
    three different server paths; two known bugs here were binary-only."""
    cur = seeded.execute("SELECT a FROM z1 WHERE a = %s", (5,), prepare=prepare, binary=binary)
    assert cur.fetchall() == [(5,)]
