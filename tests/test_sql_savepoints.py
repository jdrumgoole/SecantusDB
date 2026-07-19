"""Real nested savepoints: SAVEPOINT / RELEASE / ROLLBACK TO SAVEPOINT.

A savepoint captures each touched collection's pre-image the first time it's
written after the savepoint is established; ROLLBACK TO restores those
pre-images (undoing every later write), keeps the savepoint open, and un-poisons
an aborted block. RELEASE forgets the savepoint but keeps its writes, merging its
undo state into the enclosing savepoint. Driven through ``run_sql`` over the
real WT-backed ``Storage`` (BEGIN snapshots, abort restores).
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int)", session=Session(database=DB))
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def ids(storage, session):
    return [r[0] for r in q(storage, session, "SELECT id FROM t ORDER BY id").rows]


def test_rollback_to_savepoint_undoes_inserts(storage, session):
    q(storage, session, "INSERT INTO t (id, n) VALUES (1, 10)")
    q(storage, session, "BEGIN")
    q(storage, session, "INSERT INTO t (id, n) VALUES (2, 20)")
    q(storage, session, "SAVEPOINT sp1")
    q(storage, session, "INSERT INTO t (id, n) VALUES (3, 30)")
    assert ids(storage, session) == [1, 2, 3]
    q(storage, session, "ROLLBACK TO SAVEPOINT sp1")
    assert ids(storage, session) == [1, 2]  # insert of 3 undone, 2 kept
    q(storage, session, "INSERT INTO t (id, n) VALUES (4, 40)")
    q(storage, session, "COMMIT")
    assert ids(storage, session) == [1, 2, 4]


def test_rollback_to_savepoint_undoes_update_and_delete(storage, session):
    for i in (1, 2, 3):
        q(storage, session, f"INSERT INTO t (id, n) VALUES ({i}, {i * 10})")
    q(storage, session, "BEGIN")
    q(storage, session, "SAVEPOINT sp")
    q(storage, session, "UPDATE t SET n = 999 WHERE id = 1")
    q(storage, session, "DELETE FROM t WHERE id = 2")
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 999), (3, 30)]
    q(storage, session, "ROLLBACK TO SAVEPOINT sp")
    # The UPDATE and the DELETE are both undone.
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [
        (1, 10),
        (2, 20),
        (3, 30),
    ]
    q(storage, session, "COMMIT")
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [
        (1, 10),
        (2, 20),
        (3, 30),
    ]


def test_nested_savepoints_partial_rollback(storage, session):
    q(storage, session, "INSERT INTO t (id, n) VALUES (1, 10)")
    q(storage, session, "BEGIN")
    q(storage, session, "SAVEPOINT a")
    q(storage, session, "INSERT INTO t (id, n) VALUES (2, 20)")
    q(storage, session, "SAVEPOINT b")
    q(storage, session, "INSERT INTO t (id, n) VALUES (3, 30)")
    q(storage, session, "ROLLBACK TO SAVEPOINT b")
    assert ids(storage, session) == [1, 2]  # only b's insert undone
    q(storage, session, "ROLLBACK TO SAVEPOINT a")
    assert ids(storage, session) == [1]  # a's insert undone too
    q(storage, session, "COMMIT")
    assert ids(storage, session) == [1]


def test_repeated_rollback_to_same_savepoint(storage, session):
    q(storage, session, "BEGIN")
    q(storage, session, "SAVEPOINT sp")
    q(storage, session, "INSERT INTO t (id, n) VALUES (1, 10)")
    q(storage, session, "ROLLBACK TO SAVEPOINT sp")
    assert ids(storage, session) == []
    q(storage, session, "INSERT INTO t (id, n) VALUES (2, 20)")
    q(storage, session, "ROLLBACK TO SAVEPOINT sp")  # sp still open, undoes again
    assert ids(storage, session) == []
    q(storage, session, "COMMIT")
    assert ids(storage, session) == []


def test_release_keeps_writes_but_parent_can_still_undo(storage, session):
    q(storage, session, "INSERT INTO t (id, n) VALUES (1, 10)")
    q(storage, session, "BEGIN")
    q(storage, session, "SAVEPOINT a")
    q(storage, session, "INSERT INTO t (id, n) VALUES (2, 20)")
    q(storage, session, "SAVEPOINT b")
    q(storage, session, "INSERT INTO t (id, n) VALUES (3, 30)")
    assert q(storage, session, "RELEASE SAVEPOINT b").command_tag == "RELEASE"
    assert ids(storage, session) == [1, 2, 3]  # release keeps 3
    # Rolling back to the outer savepoint still undoes what b did (merged down).
    q(storage, session, "ROLLBACK TO SAVEPOINT a")
    assert ids(storage, session) == [1]
    q(storage, session, "COMMIT")


def test_rollback_to_savepoint_recovers_aborted_block(storage, session):
    q(storage, session, "INSERT INTO t (id, n) VALUES (1, 10)")
    q(storage, session, "BEGIN")
    q(storage, session, "SAVEPOINT sp")
    with pytest.raises(SQLError):
        q(storage, session, "INSERT INTO t (id, n) VALUES (1, 99)")  # duplicate pk
    # The block is poisoned: ordinary statements are refused.
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT 1")
    assert ei.value.sqlstate == "25P02"
    # ROLLBACK TO SAVEPOINT recovers the block.
    q(storage, session, "ROLLBACK TO SAVEPOINT sp")
    assert q(storage, session, "SELECT 1").rows == [(1,)]
    q(storage, session, "COMMIT")


def test_savepoint_outside_transaction_errors(storage, session):
    for sql in ("SAVEPOINT x", "RELEASE SAVEPOINT x", "ROLLBACK TO SAVEPOINT x"):
        with pytest.raises(SQLError) as ei:
            q(storage, session, sql)
        assert ei.value.sqlstate == "25P01"


def test_rollback_to_unknown_savepoint_errors(storage, session):
    q(storage, session, "BEGIN")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "ROLLBACK TO SAVEPOINT nope")
    assert ei.value.sqlstate == "3B001"
    q(storage, session, "ROLLBACK")


def test_savepoint_name_shadowing(storage, session):
    # Two savepoints with the same name: ROLLBACK TO / RELEASE hit the innermost.
    q(storage, session, "BEGIN")
    q(storage, session, "SAVEPOINT s")
    q(storage, session, "INSERT INTO t (id, n) VALUES (1, 10)")
    q(storage, session, "SAVEPOINT s")
    q(storage, session, "INSERT INTO t (id, n) VALUES (2, 20)")
    q(storage, session, "ROLLBACK TO SAVEPOINT s")  # innermost s → undoes only 2
    assert ids(storage, session) == [1]
    q(storage, session, "COMMIT")
    assert ids(storage, session) == [1]


def test_full_rollback_clears_savepoints(storage, session):
    q(storage, session, "BEGIN")
    q(storage, session, "SAVEPOINT sp")
    q(storage, session, "INSERT INTO t (id, n) VALUES (1, 10)")
    q(storage, session, "ROLLBACK")
    assert ids(storage, session) == []
    # A fresh transaction: the old savepoint name is gone.
    q(storage, session, "BEGIN")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "ROLLBACK TO SAVEPOINT sp")
    assert ei.value.sqlstate == "3B001"
    q(storage, session, "ROLLBACK")


def test_rollback_to_savepoint_undoes_create_type(storage, session):
    # Catalog DDL (CREATE TYPE / CREATE TABLE) is snapshotted too, so a
    # savepoint rollback reverts the schema change — a re-CREATE then succeeds.
    q(storage, session, "BEGIN")
    for _ in range(3):
        q(storage, session, "SAVEPOINT sp")
        q(storage, session, "CREATE TYPE mood AS ENUM ('sad', 'ok')")
        q(storage, session, "CREATE TABLE moody (id int PRIMARY KEY, m mood)")
        q(storage, session, "ROLLBACK TO SAVEPOINT sp")
        # The type and table are gone after each rollback.
        assert q(storage, session, "SELECT count(*) FROM pg_type WHERE typname = 'mood'").rows == [
            (0,)
        ]
    q(storage, session, "COMMIT")


def test_rollback_to_savepoint_restores_dropped_type(storage, session):
    run_sql(storage, DB, "CREATE TYPE hue AS ENUM ('r', 'g')", session=session)
    q(storage, session, "BEGIN")
    q(storage, session, "SAVEPOINT sp")
    q(storage, session, "DROP TYPE hue")
    q(storage, session, "ROLLBACK TO SAVEPOINT sp")
    # The dropped type is back.
    assert q(storage, session, "SELECT count(*) FROM pg_type WHERE typname = 'hue'").rows == [(1,)]
    q(storage, session, "COMMIT")
