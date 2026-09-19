"""``oid[]`` / ``regclass[]`` / ``regproc[]`` / ``regtype[]`` parse as types.

sqlglot reads those four names as an ``ObjectIdentifier``, not a data type, and
cannot follow one with ``[]``: ``'{1,2}'::oid[]`` was a syntax error
(``Required keyword: 'expressions' missing for ... Bracket``) for SQL Postgres
accepts. psycopg renders an ``oid[]`` parameter exactly that way on a
client-side-binding cursor, which is how its random-data ``test_leak`` suites
hit it in two gauge runs (47 of 3,000 random schemas, once reproduced).

Also pinned: a ``regclass`` / ``regproc`` column was an XX000 (a ``.name`` on
the plain string sqlglot keeps for those types). The server does not model
those column types; it now says so (0A000) instead of crashing.
"""

from __future__ import annotations

import pytest

from secantus.sql import planner

psycopg = pytest.importorskip("psycopg")

from psycopg.types.numeric import Oid  # noqa: E402

from secantus.sql import engine  # noqa: E402
from secantus.sql.errors import SQLError  # noqa: E402
from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402


@pytest.mark.parametrize(
    "sql",
    [
        "select '{1,2}'::oid[]",
        "select cast('{}' as regclass[])",
        "select '{}'::regproc[]",
        "select '{int4}'::regtype[]",
        "select x::oid[] from t",
        "select array[1,2]::oid[]",
        "select '{1}'::oid [ ]",
        "select '{{1}}'::oid[][]",
        "create table t (a oid[], b regtype[])",
    ],
)
def test_parses(sql: str) -> None:
    planner.parse(sql)


def test_a_string_literal_is_left_alone() -> None:
    stmt = planner.parse("select 'x::oid[]' as s")[0]
    assert stmt.sql(dialect="postgres") == "SELECT 'x::oid[]' AS s"


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        c = psycopg.connect(
            host=host, port=port, user="postgres", dbname="postgres", autocommit=True
        )
        try:
            yield c
        finally:
            c.close()
    finally:
        srv.stop()
        st.close()


def test_casts_answer_with_the_array_type(conn) -> None:
    cur = conn.execute("select '{1,2}'::oid[], pg_typeof('{1}'::oid[])::text")
    assert cur.fetchone() == ([1, 2], "oid[]")
    assert cur.description[0].type_code == 1028


def test_client_side_binding_of_an_oid_list(conn) -> None:
    """The shape psycopg's leak tests hit: an ``oid[]`` value inlined by a
    client-side-binding cursor."""
    conn.execute("create table t (id int primary key, a oid[])")
    cur = psycopg.ClientCursor(conn)
    cur.execute("insert into t values (%s, %s)", (1, [Oid(5), Oid(6)]))
    assert conn.execute("select a from t").fetchone() == ([5, 6],)


@pytest.mark.parametrize("typ", ["regclass", "regproc", "regclass[]", "regproc[]"])
def test_unmodelled_column_types_are_not_supported_not_a_crash(tmp_path, typ: str) -> None:
    st = Storage(str(tmp_path))
    try:
        with pytest.raises(SQLError) as exc:
            engine.run_sql(st, "postgres", f"create table c (v {typ})")
        assert exc.value.sqlstate == "0A000"
    finally:
        st.close()
