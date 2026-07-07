"""Row-locking clauses (#132): SELECT … FOR UPDATE / SHARE.

SecantusDB is single-node, so a locking clause is a no-op that simply returns
the rows — but it's *accepted* everywhere a SELECT can appear (so ORMs like
SQLAlchemy's ``with_for_update()`` work), and an ``OF <table>`` target that isn't
a relation in the FROM is a hard error (SQLSTATE 42P01), exactly as Postgres
reports it. Driven over the real ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    sess = Session(database=DB)
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, a int, g text)", session=sess)
    run_sql(s, DB, "CREATE TABLE u (id bigint primary key, t_id int)", session=sess)
    run_sql(s, DB, "INSERT INTO t VALUES (1,10,'x'),(2,20,'x'),(3,30,'y')", session=sess)
    run_sql(s, DB, "INSERT INTO u VALUES (1,1),(2,2)", session=sess)
    try:
        yield s
    finally:
        s.close()


def _rows(storage, sql):
    return run_sql(storage, DB, sql, session=Session(database=DB))[-1].rows


# --------------------------------------------------------------------------- #
# The four lock strengths + wait modifiers, on a plain SELECT.
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    "clause",
    [
        "FOR UPDATE",
        "FOR SHARE",
        "FOR NO KEY UPDATE",
        "FOR KEY SHARE",
        "FOR UPDATE NOWAIT",
        "FOR UPDATE SKIP LOCKED",
        "FOR SHARE OF t",
        "FOR UPDATE OF t NOWAIT",
    ],
)
def test_lock_clause_accepted(storage, clause):
    assert _rows(storage, f"SELECT id FROM t WHERE a >= 20 ORDER BY id {clause}") == [(2,), (3,)]


# --------------------------------------------------------------------------- #
# Accepted across the harder SELECT shapes (join / group / distinct / CTE).
# --------------------------------------------------------------------------- #


def test_lock_with_join(storage):
    assert _rows(
        storage, "SELECT t.id FROM t JOIN u ON u.t_id = t.id ORDER BY t.id FOR UPDATE OF t, u"
    ) == [(1,), (2,)]


def test_lock_with_cte(storage):
    assert _rows(
        storage, "WITH c AS (SELECT id FROM t) SELECT * FROM c ORDER BY id FOR UPDATE"
    ) == [
        (1,),
        (2,),
        (3,),
    ]


def test_lock_with_limit(storage):
    assert _rows(storage, "SELECT id FROM t ORDER BY id LIMIT 1 FOR UPDATE") == [(1,)]


# --------------------------------------------------------------------------- #
# OF-target validation.
# --------------------------------------------------------------------------- #


def test_of_target_must_be_in_from(storage):
    with pytest.raises(SQLError) as ei:
        _rows(storage, "SELECT id FROM t FOR UPDATE OF nope")
    assert ei.value.sqlstate == "42P01"


def test_of_target_honors_alias(storage):
    assert _rows(storage, "SELECT x.id FROM t x WHERE x.a = 10 FOR UPDATE OF x") == [(1,)]
    with pytest.raises(SQLError) as ei:
        _rows(storage, "SELECT x.id FROM t x FOR UPDATE OF t")  # real name masked by alias
    assert ei.value.sqlstate == "42P01"
