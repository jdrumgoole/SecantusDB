"""COMMENT ON FUNCTION and table-rowtype columns — the two setup blockers
behind four pgjdbc metadata classes' initializationErrors."""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def st(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


class TestCommentOnFunction:
    @pytest.fixture
    def sess(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE FUNCTION f1() RETURNS int AS 'SELECT 1' LANGUAGE sql", session=sess)
        return sess

    def test_with_parens(self, st, sess):
        res = run_sql(st, DB, "COMMENT ON FUNCTION f1() IS 'the f1'", session=sess)[-1]
        assert res.command_tag == "COMMENT"

    def test_bare_name(self, st, sess):
        run_sql(st, DB, "COMMENT ON FUNCTION f1 IS 'bare'", session=sess)
        assert run_sql(st, DB, "SELECT f1()", session=sess)[-1].rows == [(1,)]

    def test_unknown_function_raises(self, st, sess):
        with pytest.raises(errors.SQLError) as e:
            run_sql(st, DB, "COMMENT ON FUNCTION nope() IS 'x'", session=sess)
        assert e.value.sqlstate == "42883"

    def test_comment_survives_and_function_callable(self, st, sess):
        run_sql(st, DB, "COMMENT ON FUNCTION f1() IS 'kept'", session=sess)
        from secantus.sql.catalog import Catalog

        doc = Catalog(st).get_function(DB, "f1", 0)
        assert doc is not None and doc.get("comment") == "kept"


class TestTableRowtypeColumn:
    def test_column_typed_by_table_rowtype(self, st):
        sess = Session(database=DB)
        run_sql(
            st, DB, "CREATE TABLE rsmd1 (a int primary key, b text, c decimal(10,2))", session=sess
        )
        run_sql(st, DB, "CREATE TABLE compositetest (col rsmd1)", session=sess)
        run_sql(st, DB, "INSERT INTO compositetest VALUES (ROW(1, 'x', 2.5))", session=sess)
        rows = run_sql(st, DB, "SELECT (col).a, (col).b FROM compositetest", session=sess)[-1].rows
        assert rows == [(1, "x")]

    def test_unknown_type_still_errors(self, st):
        sess = Session(database=DB)
        with pytest.raises(errors.SQLError) as e:
            run_sql(st, DB, "CREATE TABLE t (col no_such_type)", session=sess)
        assert e.value.sqlstate == "42704"


class TestRowtypeColumnDescribesAsStruct:
    def test_rowtype_column_reports_typtype_c_oid(self, st):
        # pgjdbc maps a column oid to java.sql.Types.STRUCT only when its
        # pg_type row has typtype 'c'; generic RECORD (2249) maps to OTHER —
        # ResultSetMetaDataTest's testComposite trio.
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TABLE rsmd1 (a int primary key, b text)", session=sess)
        run_sql(st, DB, "CREATE TABLE compositetest (col rsmd1)", session=sess)
        res = run_sql(st, DB, "SELECT col FROM compositetest", session=sess)[-1]
        oid = res.columns[0].pg_oid
        assert oid != 2249
        rows = run_sql(
            st,
            DB,
            f"SELECT typname, typtype FROM pg_catalog.pg_type WHERE oid = {oid}",
            session=sess,
        )[-1].rows
        assert rows == [("rsmd1", "c")]


class TestResultDescriptorFidelity:
    """RowDescription metadata pgjdbc's ResultSetMetaData derives:
    base-column identity survives aliases and computed siblings, and
    declared type identities (varchar(n) oid+typmod, timestamp(p),
    numeric(p,s)) ride the descriptors."""

    @pytest.fixture
    def sess(self, st):
        sess = Session(database=DB)
        run_sql(
            st,
            DB,
            "CREATE TABLE rsmd1 (a int primary key, b text, c decimal(10,2))",
            session=sess,
        )
        return sess

    def test_base_column_identity_with_computed_sibling(self, st, sess):
        res = run_sql(st, DB, "SELECT a,b,c,a+c AS total, b AS d FROM rsmd1", session=sess)[-1]
        by_name = {c.name: c for c in res.columns}
        assert by_name["a"].attnum == 1 and by_name["a"].table_oid > 0
        assert by_name["b"].attnum == 2
        assert by_name["d"].attnum == 2  # alias resolves to its base column
        assert by_name["total"].attnum == 0 and by_name["total"].table_oid == 0

    def test_describe_carries_base_column_identity(self, st, sess):
        """Extended-protocol Describe (pgjdbc PreparedStatement.getMetaData)
        routes through the pipeline planner for a computed projection — the
        descriptors must attribute bare columns the same as execution."""
        import sqlglot

        from secantus.sql import engine
        from secantus.sql.catalog import Catalog

        stmt = sqlglot.parse_one("SELECT a,b,c,a+c AS total, b AS d FROM rsmd1", read="postgres")
        descs = engine.describe_statement(st, DB, stmt, sess, Catalog(st))
        by_name = {c.name: c for c in descs}
        assert by_name["a"].attnum == 1 and by_name["a"].table_oid > 0
        assert by_name["d"].attnum == 2
        assert by_name["total"].attnum == 0 and by_name["total"].table_oid == 0
        assert by_name["c"].typmod == ((10 << 16) | 2) + 4

    def test_evaluated_result_carries_declared_typmod(self, st, sess):
        """The computed sibling forces the evaluated executor; the numeric
        column's declared precision typmod must survive that path too."""
        res = run_sql(st, DB, "SELECT a, c, a+c AS total FROM rsmd1", session=sess)[-1]
        by_name = {c.name: c for c in res.columns}
        assert by_name["c"].typmod == ((10 << 16) | 2) + 4

    def test_declared_type_identity(self, st, sess):
        run_sql(
            st,
            DB,
            "CREATE TABLE tt (vc varchar(5), ts3 timestamp(3), n numeric(10,2))",
            session=sess,
        )
        res = run_sql(st, DB, "SELECT vc, ts3, n FROM tt", session=sess)[-1]
        vc, ts3, n = res.columns
        assert (vc.pg_oid, vc.typmod) == (1043, 9)  # varchar(5): n+4
        assert (ts3.pg_oid, ts3.typmod) == (1114, 3)
        assert (n.pg_oid, n.typmod) == (1700, ((10 << 16) | 2) + 4)
