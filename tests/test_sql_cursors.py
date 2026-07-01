"""Server-side cursors: DECLARE … CURSOR / FETCH / MOVE / CLOSE.

The query is materialized at DECLARE; FETCH / MOVE walk a scroll position over
the stored rows (forward / backward / absolute / relative), so a cursor is fully
scrollable. WITHOUT HOLD cursors close at COMMIT / ROLLBACK; WITH HOLD survive.
Driven through ``run_sql`` over ``FakeStorage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage():
    s = FakeStorage()
    sess = Session(database=DB)
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int)", session=sess)
    for i in range(1, 6):
        run_sql(s, DB, f"INSERT INTO t (id, n) VALUES ({i}, {i * 10})", session=sess)
    return s


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def fetch_ids(storage, session, sql):
    return [r[0] for r in q(storage, session, sql).rows]


def test_declare_and_fetch_forward(storage, session):
    assert q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id").command_tag == (
        "DECLARE CURSOR"
    )
    assert fetch_ids(storage, session, "FETCH 2 FROM c") == [1, 2]
    assert fetch_ids(storage, session, "FETCH NEXT FROM c") == [3]
    assert fetch_ids(storage, session, "FETCH FORWARD 1 c") == [4]
    assert fetch_ids(storage, session, "FETCH ALL FROM c") == [5]
    assert fetch_ids(storage, session, "FETCH ALL FROM c") == []  # exhausted


def test_fetch_count_tag_and_columns(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id, n FROM t ORDER BY id")
    res = q(storage, session, "FETCH 3 FROM c")
    assert res.command_tag == "FETCH 3"
    assert [col.name for col in res.columns] == ["id", "n"]
    assert res.rows == [(1, 10), (2, 20), (3, 30)]


def test_fetch_backward_and_prior(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    assert fetch_ids(storage, session, "FETCH 4 FROM c") == [1, 2, 3, 4]
    assert fetch_ids(storage, session, "FETCH BACKWARD 2 FROM c") == [3, 2]
    assert fetch_ids(storage, session, "FETCH PRIOR FROM c") == [1]
    assert fetch_ids(storage, session, "FETCH PRIOR FROM c") == []  # before first


def test_fetch_absolute_relative_first_last(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    assert fetch_ids(storage, session, "FETCH ABSOLUTE 3 FROM c") == [3]
    assert fetch_ids(storage, session, "FETCH RELATIVE 2 FROM c") == [5]
    assert fetch_ids(storage, session, "FETCH FIRST FROM c") == [1]
    assert fetch_ids(storage, session, "FETCH LAST FROM c") == [5]
    assert fetch_ids(storage, session, "FETCH ABSOLUTE -2 FROM c") == [4]


def test_move_positions_without_returning_rows(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    res = q(storage, session, "MOVE 2 FROM c")
    assert res.command_tag == "MOVE 2"
    assert res.rows == []  # MOVE returns no result set
    assert fetch_ids(storage, session, "FETCH NEXT FROM c") == [3]
    q(storage, session, "MOVE BACKWARD 2 c")
    assert fetch_ids(storage, session, "FETCH NEXT FROM c") == [2]


def test_cursor_over_join_query(storage, session):
    q(storage, session, "CREATE TABLE u (id bigint primary key, label text)")
    for i in (1, 2, 3):
        q(storage, session, f"INSERT INTO u (id, label) VALUES ({i}, 'x{i}')")
    q(
        storage,
        session,
        "DECLARE c CURSOR FOR SELECT t.id, u.label FROM t JOIN u ON t.id = u.id ORDER BY t.id",
    )
    assert q(storage, session, "FETCH ALL FROM c").rows == [(1, "x1"), (2, "x2"), (3, "x3")]


def test_close_cursor(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t")
    assert q(storage, session, "CLOSE c").command_tag == "CLOSE CURSOR"
    with pytest.raises(SQLError) as ei:
        q(storage, session, "FETCH 1 FROM c")
    assert ei.value.sqlstate == "34000"


def test_close_all(storage, session):
    q(storage, session, "DECLARE a CURSOR FOR SELECT id FROM t")
    q(storage, session, "DECLARE b CURSOR FOR SELECT id FROM t")
    q(storage, session, "CLOSE ALL")
    for name in ("a", "b"):
        with pytest.raises(SQLError):
            q(storage, session, f"FETCH 1 FROM {name}")


def test_without_hold_cursor_closes_at_commit(storage, session):
    q(storage, session, "BEGIN")
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    assert fetch_ids(storage, session, "FETCH 1 FROM c") == [1]
    q(storage, session, "COMMIT")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "FETCH 1 FROM c")
    assert ei.value.sqlstate == "34000"


def test_with_hold_cursor_survives_commit(storage, session):
    q(storage, session, "BEGIN")
    q(storage, session, "DECLARE h CURSOR WITH HOLD FOR SELECT id FROM t ORDER BY id")
    q(storage, session, "COMMIT")
    assert fetch_ids(storage, session, "FETCH 2 FROM h") == [1, 2]


def test_fetch_unknown_cursor_errors(storage, session):
    with pytest.raises(SQLError) as ei:
        q(storage, session, "FETCH 1 FROM nope")
    assert ei.value.sqlstate == "34000"
