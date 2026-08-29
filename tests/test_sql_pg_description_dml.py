"""Direct DML against pg_description + function-comment rows — the
DatabaseMetaDataTest setup blocker (its @BeforeAll moves a function comment
onto a table's oid to manufacture a duplicate-description row)."""

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


@pytest.fixture
def seeded(st):
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE duplicate (x text)", session=sess)
    run_sql(st, DB, "COMMENT ON TABLE duplicate IS 'duplicate table'", session=sess)
    run_sql(
        st,
        DB,
        "CREATE OR REPLACE FUNCTION bar() RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$",
        session=sess,
    )
    run_sql(st, DB, "COMMENT ON FUNCTION bar() IS 'bar function'", session=sess)
    return sess


class TestFunctionCommentRows:
    def test_function_comment_visible_with_pg_proc_classoid(self, st, seeded):
        rows = run_sql(
            st,
            DB,
            "SELECT classoid, objsubid, description FROM pg_description"
            " WHERE description = 'bar function'",
            session=seeded,
        )[-1].rows
        assert rows == [(1255, 0, "bar function")]

    def test_regproc_resolves_user_function_numerically(self, st, seeded):
        rows = run_sql(
            st,
            DB,
            "SELECT description FROM pg_description WHERE objoid = 'bar'::regproc",
            session=seeded,
        )[-1].rows
        assert rows == [("bar function",)]
        # the cast still renders as the bare name (real-PG regproc output)
        assert run_sql(st, DB, "SELECT 'bar'::regproc", session=seeded)[-1].rows == [("bar",)]


class TestPgDescriptionDml:
    def test_update_moves_row_and_classoid_guard_excludes_it(self, st, seeded):
        res = run_sql(
            st,
            DB,
            "UPDATE pg_description SET objoid = 'duplicate'::regclass"
            " WHERE objoid = 'bar'::regproc",
            session=seeded,
        )[-1]
        assert res.command_tag == "UPDATE 1"
        relid = run_sql(
            st, DB, "SELECT oid FROM pg_class WHERE relname = 'duplicate'", session=seeded
        )[-1].rows[0][0]
        rows = run_sql(
            st,
            DB,
            f"SELECT classoid, description FROM pg_description WHERE objoid = {relid}"
            " ORDER BY classoid",
            session=seeded,
        )[-1].rows
        assert rows == [(1255, "bar function"), (1259, "duplicate table")]
        # pgjdbc's getTables comment join carries a classoid guard — the moved
        # row (classoid pg_proc) must not surface as the table's comment.
        guarded = run_sql(
            st,
            DB,
            "SELECT d.description FROM pg_catalog.pg_class c"
            " LEFT JOIN pg_catalog.pg_description d ON (c.oid = d.objoid"
            " AND d.objsubid = 0 and d.classoid = 'pg_class'::regclass)"
            " WHERE c.relname = 'duplicate'",
            session=seeded,
        )[-1].rows
        assert guarded == [("duplicate table",)]

    def test_delete_suppresses_row(self, st, seeded):
        res = run_sql(
            st,
            DB,
            "DELETE FROM pg_description WHERE description = 'bar function'",
            session=seeded,
        )[-1]
        assert res.command_tag == "DELETE 1"
        rows = run_sql(st, DB, "SELECT description FROM pg_description", session=seeded)[-1].rows
        assert rows == [("duplicate table",)]

    def test_delta_persists_across_sessions(self, st, seeded):
        run_sql(
            st,
            DB,
            "UPDATE pg_description SET objoid = 'duplicate'::regclass"
            " WHERE objoid = 'bar'::regproc",
            session=seeded,
        )
        other = Session(database=DB)
        rows = run_sql(
            st,
            DB,
            "SELECT classoid FROM pg_description WHERE description = 'bar function'",
            session=other,
        )[-1].rows
        assert rows == [(1255,)]
        oid_rows = run_sql(
            st,
            DB,
            "SELECT objoid = 'duplicate'::regclass FROM pg_description"
            " WHERE description = 'bar function'",
            session=other,
        )[-1].rows
        assert oid_rows == [(True,)]

    def test_unknown_column_rejected(self, st, seeded):
        with pytest.raises(errors.SQLError) as e:
            run_sql(st, DB, "UPDATE pg_description SET nope = 1", session=seeded)
        assert e.value.sqlstate == "42703"


class TestOperatorDdl:
    """CREATE / DROP OPERATOR — registered DDL (evaluation doesn't consult
    user operators); DatabaseMetaDataTest's setup creates a custom ``&``."""

    @pytest.fixture
    def sess(self, st):
        sess = Session(database=DB)
        run_sql(
            st,
            DB,
            "CREATE OR REPLACE FUNCTION f6(numeric, integer) returns integer as"
            " 'BEGIN return 1;END;' language plpgsql immutable",
            session=sess,
        )
        return sess

    def test_create_drop_roundtrip(self, st, sess):
        tag = run_sql(
            st,
            DB,
            "CREATE OPERATOR & (LEFTARG = numeric, RIGHTARG = integer, PROCEDURE = f6)",
            session=sess,
        )[-1].command_tag
        assert tag == "CREATE OPERATOR"
        assert (
            run_sql(st, DB, "DROP OPERATOR & (numeric, integer)", session=sess)[-1].command_tag
            == "DROP OPERATOR"
        )

    def test_drop_if_exists_missing_is_noop(self, st, sess):
        tag = run_sql(st, DB, "DROP OPERATOR IF EXISTS & (numeric, integer)", session=sess)[
            -1
        ].command_tag
        assert tag == "DROP OPERATOR"

    def test_drop_missing_without_if_exists_errors(self, st, sess):
        with pytest.raises(errors.SQLError) as e:
            run_sql(st, DB, "DROP OPERATOR & (numeric, integer)", session=sess)
        assert e.value.sqlstate == "42883"

    def test_create_with_unknown_procedure_errors(self, st, sess):
        with pytest.raises(errors.SQLError) as e:
            run_sql(
                st,
                DB,
                "CREATE OPERATOR & (LEFTARG = numeric, RIGHTARG = integer, PROCEDURE = nope)",
                session=sess,
            )
        assert e.value.sqlstate == "42883"


class TestCompositeArrayColumns:
    def test_customtable_ddl_and_minted_array_oids(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TYPE custom AS (i int)", session=sess)
        run_sql(st, DB, "CREATE TYPE _custom AS (f float)", session=sess)
        run_sql(
            st,
            DB,
            "CREATE TABLE customtable (c1 custom, c2 _custom, c3 custom[], c4 _custom[])",
            session=sess,
        )
        res = run_sql(st, DB, "SELECT c1, c3, c4 FROM customtable", session=sess)[-1]
        c1, c3, c4 = res.columns
        assert c1.type_tag == "composite"
        assert c3.type_tag == "composite[]" and c3.pg_oid == c1.pg_oid + 100_000
        assert c4.type_tag == "composite[]" and c4.pg_oid != c3.pg_oid

    def test_array_of_unknown_type_still_errors(self, st):
        sess = Session(database=DB)
        with pytest.raises(errors.SQLError) as e:
            run_sql(st, DB, "CREATE TABLE t (c nope[])", session=sess)
        assert e.value.sqlstate == "42704"


class TestOutParamFunctionIdentity:
    def test_out_params_excluded_from_signature(self, st):
        sess = Session(database=DB)
        run_sql(
            st,
            DB,
            "CREATE OR REPLACE FUNCTION f3(IN a int, INOUT b varchar, OUT c timestamptz)"
            " AS $f$ BEGIN b := 'a'; c := now(); return; END; $f$ LANGUAGE plpgsql",
            session=sess,
        )
        tag = run_sql(st, DB, "DROP FUNCTION f3(int, varchar)", session=sess)[-1].command_tag
        assert tag == "DROP FUNCTION"


class TestAddPrimaryKeyUsingIndex:
    def test_promotes_unique_index_to_pk(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TABLE pk_include_column (a INT, b INT, c INT, d INT)", session=sess)
        run_sql(
            st,
            DB,
            "CREATE UNIQUE INDEX IF NOT EXISTS pk_include_column_pkey"
            " ON pk_include_column (b,d) INCLUDE (a)",
            session=sess,
        )
        tag = run_sql(
            st,
            DB,
            "ALTER TABLE pk_include_column ADD PRIMARY KEY USING INDEX pk_include_column_pkey",
            session=sess,
        )[-1].command_tag
        assert tag == "ALTER TABLE"
        run_sql(st, DB, "INSERT INTO pk_include_column VALUES (1,2,3,4)", session=sess)
        with pytest.raises(errors.SQLError):
            run_sql(st, DB, "INSERT INTO pk_include_column VALUES (9,2,9,4)", session=sess)
        rows = run_sql(
            st,
            DB,
            "SELECT conname, contype FROM pg_constraint WHERE conrelid ="
            " (SELECT oid FROM pg_class WHERE relname='pk_include_column')",
            session=sess,
        )[-1].rows
        assert rows == [("pk_include_column_pkey", "p")]

    def test_non_unique_index_rejected(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TABLE t2 (a INT, b INT)", session=sess)
        run_sql(st, DB, "CREATE INDEX t2_b ON t2 (b)", session=sess)
        with pytest.raises(errors.SQLError) as e:
            run_sql(st, DB, "ALTER TABLE t2 ADD PRIMARY KEY USING INDEX t2_b", session=sess)
        assert e.value.sqlstate == "42809"
