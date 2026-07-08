"""Real transaction semantics: BEGIN / COMMIT / ROLLBACK.

Driven through ``run_sql`` over the real WT-backed ``Storage``, which snapshots
at BEGIN and restores on abort (all-or-nothing via WiredTiger user
transactions). The wire-level transaction-status byte is covered in
``test_pgserver_pg8000.py``.
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
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)


def one(storage, session, sql):
    return run(storage, session, sql)[0]


def count(storage, session):
    return one(storage, session, "SELECT COUNT(*) FROM t").rows[0][0]


def _seed(storage, session):
    one(storage, session, "CREATE TABLE t (id bigint primary key, n int)")
    one(storage, session, "INSERT INTO t (id, n) VALUES (1, 10)")


# --------------------------------------------------------------------------- #


def test_rollback_undoes_writes(storage, session):
    _seed(storage, session)
    one(storage, session, "BEGIN")
    one(storage, session, "INSERT INTO t (id, n) VALUES (2, 20)")
    assert count(storage, session) == 2  # visible inside the block
    assert session.txn_status() == b"T"
    one(storage, session, "ROLLBACK")
    assert count(storage, session) == 1  # the insert is gone
    assert session.txn_status() == b"I"


def test_commit_persists_writes(storage, session):
    _seed(storage, session)
    one(storage, session, "BEGIN")
    one(storage, session, "INSERT INTO t (id, n) VALUES (2, 20)")
    assert one(storage, session, "COMMIT").command_tag == "COMMIT"
    assert count(storage, session) == 2


def test_rollback_undoes_update_and_delete(storage, session):
    _seed(storage, session)
    one(storage, session, "BEGIN")
    one(storage, session, "UPDATE t SET n = 999 WHERE id = 1")
    one(storage, session, "DELETE FROM t WHERE id = 1")
    assert count(storage, session) == 0
    one(storage, session, "ROLLBACK")
    assert one(storage, session, "SELECT n FROM t WHERE id = 1").rows == [(10,)]


def test_error_aborts_block_until_rollback(storage, session):
    _seed(storage, session)
    one(storage, session, "BEGIN")
    with pytest.raises(SQLError) as ei:
        one(storage, session, "SELECT * FROM nonexistent")
    assert ei.value.sqlstate == "42P01"
    assert session.txn_status() == b"E"
    # Every command except COMMIT/ROLLBACK is rejected while the block is failed.
    with pytest.raises(SQLError) as ei2:
        one(storage, session, "SELECT COUNT(*) FROM t")
    assert ei2.value.sqlstate == "25P02"
    # COMMIT of an aborted block rolls back and reports ROLLBACK.
    assert one(storage, session, "COMMIT").command_tag == "ROLLBACK"
    assert session.txn_status() == b"I"


def test_aborted_block_rolls_back_partial_writes(storage, session):
    _seed(storage, session)
    one(storage, session, "BEGIN")
    one(storage, session, "INSERT INTO t (id, n) VALUES (2, 20)")  # succeeds
    with pytest.raises(SQLError):
        one(storage, session, "INSERT INTO t (id, n) VALUES (1, 99)")  # duplicate PK -> abort
    one(storage, session, "ROLLBACK")
    assert count(storage, session) == 1  # neither write survived


def test_commit_and_rollback_without_block_are_noops(storage, session):
    _seed(storage, session)
    assert one(storage, session, "COMMIT").command_tag == "COMMIT"
    assert one(storage, session, "ROLLBACK").command_tag == "ROLLBACK"
    assert session.txn_status() == b"I"


def test_ddl_is_transactional(storage, session):
    one(storage, session, "BEGIN")
    one(storage, session, "CREATE TABLE temp (id bigint primary key)")
    one(storage, session, "ROLLBACK")
    # The rolled-back CREATE TABLE leaves no relation behind.
    with pytest.raises(SQLError) as ei:
        one(storage, session, "SELECT * FROM temp")
    assert ei.value.sqlstate == "42P01"


def test_multi_statement_block_in_one_call(storage, session):
    _seed(storage, session)
    run(
        storage,
        session,
        "BEGIN;"
        " INSERT INTO t (id, n) VALUES (2, 20);"
        " INSERT INTO t (id, n) VALUES (3, 30);"
        " COMMIT;",
    )
    assert count(storage, session) == 3
    assert session.txn_status() == b"I"
