"""PREPARE / EXECUTE / DEALLOCATE (#121): SQL-level prepared statements on the
session. Parameter binding, argument-count checks, name lifecycle, and running
prepared writes. (End-to-end wire coverage is in test_pgserver_pg8000.py.)
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def fresh(tmp_path):
    st = Storage(str(tmp_path))
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE t (id int, name text)", session=sess)
    run_sql(
        st,
        DB,
        "INSERT INTO t VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
        session=sess,
    )
    try:
        yield st, sess
    finally:
        st.close()


def _run(st, sess, sql):
    return run_sql(st, DB, sql, session=sess)[-1]


# --------------------------------------------------------------------------- #
# PREPARE
# --------------------------------------------------------------------------- #


def test_prepare_no_params_and_execute(fresh):
    st, sess = fresh
    r = _run(st, sess, "PREPARE p0 AS SELECT id, name FROM t ORDER BY id")
    assert r.command_tag == "PREPARE"
    assert "p0" in sess.prepared
    r = _run(st, sess, "EXECUTE p0")
    assert r.rows == [(1, "alice"), (2, "bob"), (3, "carol")]


def test_prepare_with_typed_params(fresh):
    st, sess = fresh
    _run(st, sess, "PREPARE p1 (int) AS SELECT name FROM t WHERE id = $1")
    assert _run(st, sess, "EXECUTE p1 (2)").rows == [("bob",)]
    assert _run(st, sess, "EXECUTE p1 (3)").rows == [("carol",)]


def test_prepare_multiple_params_mixed_types(fresh):
    st, sess = fresh
    _run(
        st,
        sess,
        "PREPARE p2 (int, text) AS SELECT id FROM t WHERE id > $1 AND name = $2",
    )
    assert _run(st, sess, "EXECUTE p2 (1, 'carol')").rows == [(3,)]
    assert _run(st, sess, "EXECUTE p2 (5, 'carol')").rows == []


def test_prepare_untyped_params(fresh):
    # The optional (types) list may be omitted entirely.
    st, sess = fresh
    _run(st, sess, "PREPARE p AS SELECT id FROM t WHERE id = $1")
    assert _run(st, sess, "EXECUTE p (2)").rows == [(2,)]


def test_execute_prepared_insert(fresh):
    st, sess = fresh
    _run(st, sess, "PREPARE ins (int, text) AS INSERT INTO t VALUES ($1, $2)")
    r = _run(st, sess, "EXECUTE ins (4, 'dave')")
    assert r.command_tag == "INSERT 0 1"
    assert _run(st, sess, "SELECT count(*) FROM t").rows == [(4,)]


def test_execute_reuses_prepared_plan_repeatedly(fresh):
    st, sess = fresh
    _run(st, sess, "PREPARE up (text, int) AS UPDATE t SET name = $1 WHERE id = $2")
    _run(st, sess, "EXECUTE up ('ALICE', 1)")
    _run(st, sess, "EXECUTE up ('BOB', 2)")
    rows = _run(st, sess, "SELECT name FROM t WHERE id IN (1, 2) ORDER BY id").rows
    assert rows == [("ALICE",), ("BOB",)]


# --------------------------------------------------------------------------- #
# Errors
# --------------------------------------------------------------------------- #


def test_duplicate_prepare_name_rejected(fresh):
    st, sess = fresh
    _run(st, sess, "PREPARE p (int) AS SELECT $1")
    with pytest.raises(errors.SQLError) as exc:
        _run(st, sess, "PREPARE p (int) AS SELECT $1")
    assert exc.value.sqlstate == "42P05"


def test_execute_unknown_statement_rejected(fresh):
    st, sess = fresh
    with pytest.raises(errors.SQLError) as exc:
        _run(st, sess, "EXECUTE nope (1)")
    assert exc.value.sqlstate == "26000"


def test_execute_wrong_argument_count_rejected(fresh):
    st, sess = fresh
    _run(st, sess, "PREPARE p (int) AS SELECT name FROM t WHERE id = $1")
    with pytest.raises(errors.SQLError) as exc:
        _run(st, sess, "EXECUTE p (1, 2)")
    assert exc.value.sqlstate == "08P01"
    with pytest.raises(errors.SQLError) as exc:
        _run(st, sess, "EXECUTE p")
    assert exc.value.sqlstate == "08P01"


# --------------------------------------------------------------------------- #
# DEALLOCATE
# --------------------------------------------------------------------------- #


def test_deallocate_one(fresh):
    st, sess = fresh
    _run(st, sess, "PREPARE p (int) AS SELECT $1")
    r = _run(st, sess, "DEALLOCATE p")
    assert r.command_tag == "DEALLOCATE"
    assert "p" not in sess.prepared
    with pytest.raises(errors.SQLError) as exc:
        _run(st, sess, "EXECUTE p (1)")
    assert exc.value.sqlstate == "26000"


def test_deallocate_all(fresh):
    st, sess = fresh
    _run(st, sess, "PREPARE a AS SELECT 1")
    _run(st, sess, "PREPARE b AS SELECT 2")
    r = _run(st, sess, "DEALLOCATE ALL")
    # Postgres tags the ALL form "DEALLOCATE ALL" (checked against 14.13), and
    # drivers key off that exact string to know their statement cache is gone.
    assert r.command_tag == "DEALLOCATE ALL"
    assert sess.prepared == {}


def test_deallocate_unknown_is_noop(fresh):
    # libpq/psycopg fire speculative DEALLOCATEs during cleanup — tolerate them.
    st, sess = fresh
    r = _run(st, sess, "DEALLOCATE never_prepared")
    assert r.command_tag == "DEALLOCATE"


def test_deallocate_quoted_name(fresh):
    st, sess = fresh
    _run(st, sess, 'PREPARE "MixedCase" AS SELECT 1')
    assert "MixedCase" in sess.prepared
    _run(st, sess, 'DEALLOCATE "MixedCase"')
    assert "MixedCase" not in sess.prepared


# --------------------------------------------------------------------------- #
# Isolation: prepared statements are per-session
# --------------------------------------------------------------------------- #


def test_prepared_statements_are_per_session(tmp_path):
    st = Storage(str(tmp_path))
    try:
        a = Session(database=DB)
        b = Session(database=DB)
        run_sql(st, DB, "PREPARE p AS SELECT 1", session=a)
        assert "p" in a.prepared
        with pytest.raises(errors.SQLError) as exc:
            run_sql(st, DB, "EXECUTE p", session=b)
        assert exc.value.sqlstate == "26000"
    finally:
        st.close()


def test_discard_tag_echoes_target(fresh):
    # PG echoes the DISCARD target in the CommandComplete tag.
    st, sess = fresh
    assert _run(st, sess, "DISCARD ALL").command_tag == "DISCARD ALL"
    assert _run(st, sess, "DISCARD PLANS").command_tag == "DISCARD PLANS"
    assert _run(st, sess, "DISCARD SEQUENCES").command_tag == "DISCARD SEQUENCES"
    assert _run(st, sess, "DISCARD TEMP").command_tag == "DISCARD TEMP"
    assert _run(st, sess, "DISCARD TEMPORARY").command_tag == "DISCARD TEMP"
