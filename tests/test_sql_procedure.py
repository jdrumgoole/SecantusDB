"""``CREATE PROCEDURE`` / ``CALL`` / ``DROP PROCEDURE`` (pgtest procedure).

Procedures are stored like plpgsql functions with per-parameter argmodes; CALL
runs the body and returns the OUT / INOUT parameters as the result row. Driven
through the real WiredTiger-backed ``Storage``.
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


def test_create_call_inout_procedure(storage, session):
    q(storage, session, "CREATE TABLE temp(a INT)")
    r = q(
        storage,
        session,
        "CREATE PROCEDURE proc(a INOUT INT) AS "
        "'BEGIN INSERT INTO temp VALUES(a); END;' LANGUAGE plpgsql",
    )
    assert r.command_tag == "CREATE PROCEDURE"
    # CALL runs the body (persisting the INSERT) and returns the INOUT value.
    r = q(storage, session, "CALL proc(1)")
    assert r.command_tag == "CALL"
    assert [c.name for c in r.columns] == ["a"]
    assert r.rows == [(1,)]
    assert q(storage, session, "SELECT * FROM temp").rows == [(1,)]


def test_argmode_before_name(storage, session):
    # Postgres accepts the argmode before the name too (``INOUT a int``).
    q(storage, session, "CREATE TABLE t2(a INT)")
    q(
        storage,
        session,
        "CREATE PROCEDURE p2(INOUT a INT) AS 'BEGIN INSERT INTO t2 VALUES(a); END;' "
        "LANGUAGE plpgsql",
    )
    assert q(storage, session, "CALL p2(7)").rows == [(7,)]


def test_procedure_raise_and_txn_control(storage, session):
    # RAISE NOTICE emits wire notices; COMMIT / ROLLBACK inside the body are
    # accepted and execution continues (all three notices fire).
    q(
        storage,
        session,
        "CREATE OR REPLACE PROCEDURE p() LANGUAGE PLpgSQL AS $$ "
        "BEGIN RAISE NOTICE 'foo'; COMMIT; RAISE NOTICE 'bar'; ROLLBACK; "
        "RAISE NOTICE 'baz'; END $$",
    )
    r = q(storage, session, "CALL p()")
    assert r.command_tag == "CALL"
    assert r.rows == []  # no OUT/INOUT params -> no result row
    assert [msg for _lvl, msg in (r.notices or [])] == ["foo", "bar", "baz"]


def test_create_or_replace(storage, session):
    q(storage, session, "CREATE PROCEDURE r() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$")
    # Without OR REPLACE, redefining the same (name, arity) is 42723.
    with pytest.raises(errors.SQLError) as e:
        q(storage, session, "CREATE PROCEDURE r() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$")
    assert e.value.sqlstate == "42723"
    # OR REPLACE succeeds.
    repl = "CREATE OR REPLACE PROCEDURE r() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$"
    assert q(storage, session, repl).command_tag == "CREATE PROCEDURE"


def test_drop_procedure(storage, session):
    q(storage, session, "CREATE PROCEDURE d() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$")
    assert q(storage, session, "DROP PROCEDURE d").command_tag == "DROP PROCEDURE"
    with pytest.raises(errors.SQLError) as e:
        q(storage, session, "DROP PROCEDURE d")
    assert e.value.sqlstate == "42883"
    # IF EXISTS silences the missing-procedure error.
    assert q(storage, session, "DROP PROCEDURE IF EXISTS d").command_tag == "DROP PROCEDURE"
