"""``LANGUAGE plpgsql`` scalar function bodies (b234).

A compact procedural interpreter (``secantus.sql.plpgsql``) runs the scalar
subset of PL/pgSQL: ``DECLARE`` / assignment / ``IF``…``ELSIF``…``ELSE`` /
``RETURN`` / ``SELECT … INTO`` / embedded write statements / ``RAISE``. Loops,
``RETURN QUERY``/``NEXT``, ``CASE``, cursors, and ``EXCEPTION`` handlers are
out of scope and rejected. Everything runs through the real WiredTiger-backed
``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
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


def val(storage, session, sql):
    return q(storage, session, sql).rows[0][0]


# --------------------------------------------------------------------------- #
# Basic bodies
# --------------------------------------------------------------------------- #


def test_return_expression(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION add2(a int, b int) RETURNS int AS $$ BEGIN RETURN a + b; END $$ "
        "LANGUAGE plpgsql",
    )
    assert val(storage, session, "SELECT add2(3, 4)") == 7


def test_positional_params(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION dn(a int, b int) RETURNS int AS $$ BEGIN RETURN $1 * 10 + $2; END $$ "
        "LANGUAGE plpgsql",
    )
    assert val(storage, session, "SELECT dn(3, 4)") == 34


def test_string_concat_and_declare_init(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION greet(nm text) RETURNS text AS $$ "
        "DECLARE prefix text := 'Hi, '; BEGIN RETURN prefix || nm || '!'; END $$ LANGUAGE plpgsql",
    )
    assert val(storage, session, "SELECT greet('Ada')") == "Hi, Ada!"


# --------------------------------------------------------------------------- #
# Control flow
# --------------------------------------------------------------------------- #


def test_if_elsif_else(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION grade(score int) RETURNS text AS $$ "
        "DECLARE g text; BEGIN "
        "  IF score >= 90 THEN g := 'A'; "
        "  ELSIF score >= 80 THEN g := 'B'; "
        "  ELSE g := 'F'; END IF; "
        "  RETURN g; END $$ LANGUAGE plpgsql",
    )
    assert [val(storage, session, f"SELECT grade({x})") for x in (95, 85, 50)] == ["A", "B", "F"]


def test_nested_if(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION classify(n int) RETURNS text AS $$ BEGIN "
        "  IF n > 0 THEN "
        "    IF n > 100 THEN RETURN 'big'; ELSE RETURN 'small-pos'; END IF; "
        "  END IF; "
        "  RETURN 'nonpos'; END $$ LANGUAGE plpgsql",
    )
    got = [val(storage, session, f"SELECT classify({x})") for x in (500, 5, -3)]
    assert got == ["big", "small-pos", "nonpos"]


def test_return_null_branch(storage, session):
    q(
        storage,
        session,
        "CREATE FUNCTION safe_div(a int, b int) RETURNS int AS $$ BEGIN "
        "  IF b = 0 THEN RETURN NULL; END IF; RETURN a / b; END $$ LANGUAGE plpgsql",
    )
    assert val(storage, session, "SELECT safe_div(10, 2)") == 5
    assert val(storage, session, "SELECT safe_div(10, 0)") is None


# --------------------------------------------------------------------------- #
# Embedded SQL
# --------------------------------------------------------------------------- #


def test_select_into(storage, session):
    q(storage, session, "CREATE TABLE emp (id bigint primary key, name text, sal int)")
    q(storage, session, "INSERT INTO emp (id, name, sal) VALUES (1, 'x', 100), (2, 'y', 200)")
    q(
        storage,
        session,
        "CREATE FUNCTION sal_of(who text) RETURNS int AS $$ "
        "DECLARE s int; BEGIN SELECT sal INTO s FROM emp WHERE name = who; RETURN s; END $$ "
        "LANGUAGE plpgsql",
    )
    assert val(storage, session, "SELECT sal_of('y')") == 200


def test_side_effecting_insert(storage, session):
    q(storage, session, "CREATE TABLE log (id bigint primary key, msg text)")
    q(
        storage,
        session,
        "CREATE FUNCTION note(m text) RETURNS int AS $$ "
        "DECLARE n int; BEGIN "
        "  INSERT INTO log (id, msg) VALUES (1, m); "
        "  SELECT count(*) INTO n FROM log; RETURN n; END $$ LANGUAGE plpgsql",
    )
    assert val(storage, session, "SELECT note('hello')") == 1
    assert q(storage, session, "SELECT msg FROM log").rows == [("hello",)]


def test_call_in_where_clause(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, v int)")
    q(storage, session, "INSERT INTO t (id, v) VALUES (1, 5), (2, 12), (3, 20)")
    q(
        storage,
        session,
        "CREATE FUNCTION dbl(x int) RETURNS int AS $$ BEGIN RETURN x * 2; END $$ LANGUAGE plpgsql",
    )
    # dbl(v) > 20  ⇔  v > 10  → rows with v = 12, 20.
    got = q(storage, session, "SELECT id FROM t WHERE dbl(v) > 20 ORDER BY id").rows
    assert got == [(2,), (3,)]


# --------------------------------------------------------------------------- #
# Unsupported constructs are rejected (0A000), at CREATE time
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    "body",
    [
        "BEGIN WHILE n > 0 LOOP n := n - 1; END LOOP; RETURN n; END",
        "BEGIN LOOP RETURN 1; END LOOP; END",
        "BEGIN FOR i IN 1..3 LOOP RETURN i; END LOOP; END",
    ],
)
def test_unsupported_statements_rejected(storage, session, body):
    with pytest.raises(errors.SQLError) as exc:
        q(
            storage,
            session,
            f"CREATE FUNCTION f(n int) RETURNS int AS $$ {body} $$ LANGUAGE plpgsql",
        )
    assert exc.value.sqlstate == "0A000"


def test_missing_begin_rejected(storage, session):
    with pytest.raises(errors.SQLError) as exc:
        q(
            storage,
            session,
            "CREATE FUNCTION f() RETURNS int AS $$ RETURN 1; $$ LANGUAGE plpgsql",
        )
    assert exc.value.sqlstate == "0A000"
