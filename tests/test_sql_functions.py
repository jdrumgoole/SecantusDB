"""SQL functions — CREATE / DROP FUNCTION ... LANGUAGE sql (#124): defining
server-side SQL functions and invoking them in queries (named + positional
params, scalar + aggregate bodies, over tables, in WHERE, nested calls).
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"

_STORAGES: list = []


def _new_storage():
    import tempfile

    d = tempfile.mkdtemp()
    st = Storage(d)
    _STORAGES.append((st, d))
    return st


@pytest.fixture(autouse=True)
def _close_storages():
    import shutil

    yield
    while _STORAGES:
        st, d = _STORAGES.pop()
        st.close()
        shutil.rmtree(d, ignore_errors=True)


def _sess():
    return _new_storage(), Session(database=DB)


def _run(st, sess, sql):
    return run_sql(st, DB, sql, session=sess)[-1]


def _val(st, sess, sql):
    return _run(st, sess, sql).rows[0][0]


# --------------------------------------------------------------------------- #
# CREATE + call
# --------------------------------------------------------------------------- #


def test_named_param_scalar_function():
    st, sess = _sess()
    r = _run(
        st, sess, "CREATE FUNCTION add(a int, b int) RETURNS int AS $$ SELECT a + b $$ LANGUAGE sql"
    )
    assert r.command_tag == "CREATE FUNCTION"
    assert _val(st, sess, "SELECT add(2, 3)") == 5


def test_positional_param_function():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION mul(int, int) RETURNS int LANGUAGE sql AS 'SELECT $1 * $2'")
    assert _val(st, sess, "SELECT mul(4, 5)") == 20


def test_zero_arg_function():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION answer() RETURNS int AS $$ SELECT 42 $$ LANGUAGE sql")
    assert _val(st, sess, "SELECT answer()") == 42


def test_text_returning_function():
    st, sess = _sess()
    _run(
        st,
        sess,
        "CREATE FUNCTION greet(name text) RETURNS text AS $$ SELECT 'hi ' || name $$ LANGUAGE sql",
    )
    assert _val(st, sess, "SELECT greet('bob')") == "hi bob"


def test_or_replace():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION f(a int) RETURNS int AS $$ SELECT a $$ LANGUAGE sql")
    _run(
        st,
        sess,
        "CREATE OR REPLACE FUNCTION f(a int) RETURNS int AS $$ SELECT a + 100 $$ LANGUAGE sql",
    )
    assert _val(st, sess, "SELECT f(1)") == 101


# --------------------------------------------------------------------------- #
# Bodies with a FROM / aggregate, over tables, in WHERE
# --------------------------------------------------------------------------- #


def _seed(st, sess):
    _run(st, sess, "CREATE TABLE t (id int, v int)")
    _run(st, sess, "INSERT INTO t VALUES (1, 5), (2, 10), (3, 15)")


def test_function_body_with_aggregate():
    st, sess = _sess()
    _seed(st, sess)
    _run(st, sess, "CREATE FUNCTION total() RETURNS int AS $$ SELECT sum(v) FROM t $$ LANGUAGE sql")
    assert _val(st, sess, "SELECT total()") == 30


def test_function_in_select_list_over_table():
    st, sess = _sess()
    _seed(st, sess)
    _run(st, sess, "CREATE FUNCTION dbl(n int) RETURNS int AS $$ SELECT n * 2 $$ LANGUAGE sql")
    rows = _run(st, sess, "SELECT id, dbl(v) FROM t ORDER BY id").rows
    assert rows == [(1, 10), (2, 20), (3, 30)]


def test_function_in_where():
    st, sess = _sess()
    _seed(st, sess)
    _run(st, sess, "CREATE FUNCTION dbl(n int) RETURNS int AS $$ SELECT n * 2 $$ LANGUAGE sql")
    assert [r[0] for r in _run(st, sess, "SELECT id FROM t WHERE dbl(v) = 20").rows] == [2]


def test_nested_function_calls():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION dbl(n int) RETURNS int AS $$ SELECT n * 2 $$ LANGUAGE sql")
    _run(
        st, sess, "CREATE FUNCTION quad(n int) RETURNS int AS $$ SELECT dbl(dbl(n)) $$ LANGUAGE sql"
    )
    assert _val(st, sess, "SELECT quad(3)") == 12


# --------------------------------------------------------------------------- #
# Overloading by arity + persistence
# --------------------------------------------------------------------------- #


def test_overload_by_arity():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION f(a int) RETURNS int AS $$ SELECT a $$ LANGUAGE sql")
    _run(st, sess, "CREATE FUNCTION f(a int, b int) RETURNS int AS $$ SELECT a + b $$ LANGUAGE sql")
    assert _val(st, sess, "SELECT f(7)") == 7
    assert _val(st, sess, "SELECT f(7, 8)") == 15


def test_function_persists_in_catalog():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql")
    # A fresh session over the same storage still sees the function.
    sess2 = Session(database=DB)
    assert _val(st, sess2, "SELECT f()") == 1


# --------------------------------------------------------------------------- #
# Errors + DROP
# --------------------------------------------------------------------------- #


def test_duplicate_without_replace_rejected():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION f(a int) RETURNS int AS $$ SELECT a $$ LANGUAGE sql")
    with pytest.raises(errors.SQLError) as exc:
        _run(st, sess, "CREATE FUNCTION f(a int) RETURNS int AS $$ SELECT a $$ LANGUAGE sql")
    assert exc.value.sqlstate == "42723"


def test_plpgsql_language_accepted():
    # LANGUAGE plpgsql is now supported (b234) — see tests/test_sql_plpgsql.py.
    st, sess = _sess()
    _run(
        st,
        sess,
        "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END $$ LANGUAGE plpgsql",
    )
    assert _val(st, sess, "SELECT f()") == 1


def test_unknown_language_rejected():
    st, sess = _sess()
    with pytest.raises(errors.SQLError) as exc:
        _run(
            st,
            sess,
            "CREATE FUNCTION f() RETURNS int AS $$ return 1 $$ LANGUAGE plpython3u",
        )
    assert exc.value.sqlstate == "0A000"


def test_drop_function():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION f(a int) RETURNS int AS $$ SELECT a $$ LANGUAGE sql")
    assert _run(st, sess, "DROP FUNCTION f(int)").command_tag == "DROP FUNCTION"
    with pytest.raises(errors.SQLError):
        _run(st, sess, "SELECT f(1)")


def test_drop_missing_function_errors():
    st, sess = _sess()
    with pytest.raises(errors.SQLError) as exc:
        _run(st, sess, "DROP FUNCTION nope(int)")
    assert exc.value.sqlstate == "42883"


def test_drop_if_exists_missing_is_noop():
    st, sess = _sess()
    assert _run(st, sess, "DROP FUNCTION IF EXISTS nope(int)").command_tag == "DROP FUNCTION"


def test_drop_without_arglist():
    st, sess = _sess()
    _run(st, sess, "CREATE FUNCTION f(a int) RETURNS int AS $$ SELECT a $$ LANGUAGE sql")
    assert _run(st, sess, "DROP FUNCTION f").command_tag == "DROP FUNCTION"
    with pytest.raises(errors.SQLError):
        _run(st, sess, "SELECT f(1)")
