"""Parse-time operator resolution for DECLARED parameter types.

Postgres resolves a comparison's operator during parse analysis, so a parameter
declared as a type with no operator against the column it is compared with is an
error at Parse — `PREPARE s (uuid) AS SELECT id FROM t WHERE varchar_col = $1`
raises `42883 operator does not exist: character varying = uuid` before any row
is read. The plan-time check already covered literals; a parameter was treated as
undecidable, so these comparisons silently matched nothing.

Every message here was probed against a real PostgreSQL 14.
"""

from __future__ import annotations

import uuid

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


@pytest.fixture
def conn(server):
    with connect(server) as c:
        c.execute("CREATE TABLE t (id UUID PRIMARY KEY, v VARCHAR, n INT8)")
        c.commit()
        yield c


def error_of(conn, sql, params):
    with pytest.raises(psycopg.Error) as exc:
        conn.execute(sql, params).fetchall()
    conn.rollback()
    return exc.value


#: psycopg declares a parameter's type OID from the Python object it dumps, so
#: passing a ``uuid.UUID`` is what puts 2950 in Parse's ParameterOIDs — the same
#: thing pgtest's `typing` corpus does by hand.
SAMPLE_UUID = uuid.UUID("9AC39CE2-0623-4632-A965-9A51C95682D4")


def test_varchar_column_against_a_uuid_parameter(conn):
    err = error_of(conn, "SELECT id FROM t WHERE v = %s", (SAMPLE_UUID,))
    assert err.sqlstate == "42883"
    # Verbatim PostgreSQL 14: the DECLARED type is named, not the storage tag
    # (varchar folds to text internally, which would have read "text = uuid").
    assert "operator does not exist: character varying = uuid" in str(err)


def test_varchar_column_against_a_boolean_parameter(conn):
    err = error_of(conn, "SELECT id FROM t WHERE v = %s", (True,))
    assert err.sqlstate == "42883"
    assert "operator does not exist: character varying = boolean" in str(err)


def test_the_connection_survives_the_error(conn):
    error_of(conn, "SELECT id FROM t WHERE v = %s", (SAMPLE_UUID,))
    assert conn.execute("SELECT 1").fetchone() == (1,)


def test_a_compatible_parameter_type_still_runs(conn):
    conn.execute("INSERT INTO t VALUES (%s, 'abc', 7)", (SAMPLE_UUID,))
    conn.commit()
    assert conn.execute("SELECT v FROM t WHERE v = %s", ("abc",)).fetchall() == [("abc",)]
    assert conn.execute("SELECT v FROM t WHERE n = %s", (7,)).fetchall() == [("abc",)]


def test_an_untyped_parameter_stays_unjudged(conn):
    # oid 0 is Postgres' `unknown`: it takes the other operand's type rather
    # than making the comparison an error. Erroring here would break every
    # client that leaves parameters untyped.
    conn.execute("INSERT INTO t VALUES (%s, 'abc', 7)", (SAMPLE_UUID,))
    conn.commit()
    assert conn.execute("SELECT v FROM t WHERE v = %s", ("abc",)).fetchall() == [("abc",)]


def test_a_uuid_column_against_a_uuid_parameter_is_fine(conn):
    conn.execute("INSERT INTO t VALUES (%s, 'abc', 7)", (SAMPLE_UUID,))
    conn.commit()
    rows = conn.execute("SELECT v FROM t WHERE id = %s", (SAMPLE_UUID,)).fetchall()
    assert rows == [("abc",)]


class TestUnitLevel:
    """The analysis itself, without a server in the way."""

    def _tables(self, tmp_path):
        from secantus.sql import run_sql
        from secantus.sql.catalog import Catalog
        from secantus.sql.session import Session

        st = Storage(str(tmp_path))
        run_sql(
            st,
            "db",
            "CREATE TABLE t (id UUID PRIMARY KEY, v VARCHAR)",
            session=Session(database="db"),
        )
        return st, Catalog(st)

    def test_declared_oids_decide_the_comparison(self, tmp_path):
        import sqlglot

        from secantus.sql import typecheck
        from secantus.sql.errors import SQLError

        st, catalog = self._tables(tmp_path)
        try:
            stmt = sqlglot.parse_one("SELECT id FROM t WHERE v = $1", read="postgres")
            # Without the declared OIDs the parameter is unknown — no verdict.
            typecheck.check_statement(stmt, catalog, "db")
            # 2950 is uuid.
            with pytest.raises(SQLError) as exc:
                typecheck.check_statement(stmt, catalog, "db", param_oids=[2950])
            assert exc.value.sqlstate == "42883"
        finally:
            st.close()

    def test_an_undeclared_or_unmodelled_oid_says_nothing(self, tmp_path):
        import sqlglot

        from secantus.sql import typecheck

        st, catalog = self._tables(tmp_path)
        try:
            stmt = sqlglot.parse_one("SELECT id FROM t WHERE v = $1", read="postgres")
            typecheck.check_statement(stmt, catalog, "db", param_oids=[0])  # unknown
            typecheck.check_statement(stmt, catalog, "db", param_oids=[])  # not supplied
            typecheck.check_statement(stmt, catalog, "db", param_oids=[999999])  # unmodelled
        finally:
            st.close()
