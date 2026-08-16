"""plpgsql OPEN <cursor> FOR <query> / refcursor returns — the RefCursorTest /
RefCursorFetchTest blocker (pgjdbc calls the function, then FETCHes from the
returned portal name)."""

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
def sess(st):
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE testrs (id integer primary key)", session=sess)
    for i in (1, 2, 3):
        run_sql(st, DB, f"INSERT INTO testrs VALUES ({i})", session=sess)
    run_sql(
        st,
        DB,
        "CREATE OR REPLACE FUNCTION getref() RETURNS refcursor AS "
        "'declare v_resset refcursor; begin "
        "open v_resset for select id from testrs order by id; "
        "return v_resset; end;' LANGUAGE plpgsql",
        session=sess,
    )
    return sess


class TestRefcursorReturn:
    def test_call_returns_portal_name_typed_refcursor(self, st, sess):
        res = run_sql(st, DB, "SELECT getref()", session=sess)[-1]
        assert res.columns[0].pg_oid == 1790
        (name,) = res.rows[0]
        assert name.startswith("<unnamed portal ")

    def test_fetch_all_from_returned_portal(self, st, sess):
        (name,) = run_sql(st, DB, "SELECT getref()", session=sess)[-1].rows[0]
        rows = run_sql(st, DB, f'FETCH ALL IN "{name}"', session=sess)[-1].rows
        assert rows == [(1,), (2,), (3,)]

    def test_from_call_shape(self, st, sess):
        # pgjdbc rewrites {? = call f()} into ``select * from f() as result``.
        res = run_sql(st, DB, "SELECT * FROM getref() AS result", session=sess)[-1]
        assert res.columns[0].pg_oid == 1790
        (name,) = res.rows[0]
        rows = run_sql(st, DB, f'FETCH FORWARD 2 FROM "{name}"', session=sess)[-1].rows
        assert rows == [(1,), (2,)]

    def test_empty_cursor(self, st, sess):
        run_sql(
            st,
            DB,
            "CREATE OR REPLACE FUNCTION getempty() RETURNS refcursor AS "
            "'declare v refcursor; begin "
            "open v for select id from testrs where id < 1 order by id; "
            "return v; end;' LANGUAGE plpgsql",
            session=sess,
        )
        (name,) = run_sql(st, DB, "SELECT getempty()", session=sess)[-1].rows[0]
        rows = run_sql(st, DB, f'FETCH ALL IN "{name}"', session=sess)[-1].rows
        assert rows == []

    def test_distinct_portals_per_call(self, st, sess):
        (n1,) = run_sql(st, DB, "SELECT getref()", session=sess)[-1].rows[0]
        (n2,) = run_sql(st, DB, "SELECT getref()", session=sess)[-1].rows[0]
        assert n1 != n2
        assert run_sql(st, DB, f'FETCH ALL IN "{n1}"', session=sess)[-1].rows == [
            (1,),
            (2,),
            (3,),
        ]

    def test_open_sees_function_args(self, st, sess):
        run_sql(
            st,
            DB,
            "CREATE OR REPLACE FUNCTION getgt(lim int) RETURNS refcursor AS "
            "'declare v refcursor; begin "
            "open v for select id from testrs where id > lim order by id; "
            "return v; end;' LANGUAGE plpgsql",
            session=sess,
        )
        (name,) = run_sql(st, DB, "SELECT getgt(1)", session=sess)[-1].rows[0]
        rows = run_sql(st, DB, f'FETCH ALL IN "{name}"', session=sess)[-1].rows
        assert rows == [(2,), (3,)]

    def test_close_in_function(self, st, sess):
        run_sql(
            st,
            DB,
            "CREATE OR REPLACE FUNCTION openclose() RETURNS int AS "
            "'declare v refcursor; begin "
            "open v for select id from testrs; close v; return 7; end;' LANGUAGE plpgsql",
            session=sess,
        )
        assert run_sql(st, DB, "SELECT openclose()", session=sess)[-1].rows == [(7,)]
        assert not sess.cursors

    def test_fetch_unknown_portal_errors(self, st, sess):
        with pytest.raises(errors.SQLError):
            run_sql(st, DB, 'FETCH ALL IN "<unnamed portal 99>"', session=sess)
