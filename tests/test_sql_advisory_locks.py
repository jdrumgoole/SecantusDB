"""Advisory locks (#135): the ``pg_advisory_lock`` family as session-tracked
single-node no-op locking, reflected through ``pg_catalog.pg_locks``. Single
node → a lock is always granted immediately; we track what the session holds so
``pg_advisory_unlock`` reports truthfully and ``pg_locks`` reflects it. Driven
over the real ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def test_acquire_and_reflect(storage):
    sess = Session(database=DB, backend_pid=4242)
    assert _run(storage, sess, "SELECT pg_advisory_lock(1)").rows == [(None,)]
    assert _run(storage, sess, "SELECT pg_try_advisory_lock(1, 2)").rows == [(True,)]
    assert _run(storage, sess, "SELECT pg_advisory_lock_shared(5)").rows == [(None,)]
    rows = _run(
        storage,
        sess,
        "SELECT locktype, classid, objid, objsubid, mode, granted, pid "
        "FROM pg_catalog.pg_locks ORDER BY objid",
    ).rows
    assert rows == [
        ("advisory", 0, 1, 1, "ExclusiveLock", True, 4242),
        ("advisory", 1, 2, 2, "ExclusiveLock", True, 4242),
        ("advisory", 0, 5, 1, "ShareLock", True, 4242),
    ]


def test_unlock_returns_whether_held(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SELECT pg_advisory_lock(1)")
    assert _run(storage, sess, "SELECT pg_advisory_unlock(1)").rows == [(True,)]
    # Second unlock of the same key: not held → false (Postgres warns, returns false).
    assert _run(storage, sess, "SELECT pg_advisory_unlock(1)").rows == [(False,)]
    assert _run(storage, sess, "SELECT count(*) FROM pg_catalog.pg_locks").rows == [(0,)]


def test_shared_and_exclusive_unlock_are_distinct(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SELECT pg_advisory_lock_shared(9)")
    # An exclusive unlock doesn't release the shared lock.
    assert _run(storage, sess, "SELECT pg_advisory_unlock(9)").rows == [(False,)]
    assert _run(storage, sess, "SELECT pg_advisory_unlock_shared(9)").rows == [(True,)]


def test_reentrant_stacking(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SELECT pg_advisory_lock(3)")
    _run(storage, sess, "SELECT pg_advisory_lock(3)")
    # Held once in pg_locks (one row per key+mode), but needs two unlocks.
    assert _run(storage, sess, "SELECT count(*) FROM pg_catalog.pg_locks").rows == [(1,)]
    assert _run(storage, sess, "SELECT pg_advisory_unlock(3)").rows == [(True,)]
    assert _run(storage, sess, "SELECT count(*) FROM pg_catalog.pg_locks").rows == [(1,)]
    assert _run(storage, sess, "SELECT pg_advisory_unlock(3)").rows == [(True,)]
    assert _run(storage, sess, "SELECT count(*) FROM pg_catalog.pg_locks").rows == [(0,)]


def test_unlock_all_clears_session_locks(storage):
    sess = Session(database=DB)
    _run(storage, sess, "SELECT pg_advisory_lock(1)")
    _run(storage, sess, "SELECT pg_advisory_lock(2, 3)")
    _run(storage, sess, "SELECT pg_advisory_lock_shared(4)")
    assert _run(storage, sess, "SELECT pg_advisory_unlock_all()").rows == [(None,)]
    assert _run(storage, sess, "SELECT count(*) FROM pg_catalog.pg_locks").rows == [(0,)]


def test_xact_locks_release_at_commit(storage):
    sess = Session(database=DB)
    _run(storage, sess, "BEGIN")
    _run(storage, sess, "SELECT pg_advisory_xact_lock(7)")
    _run(storage, sess, "SELECT pg_advisory_lock(8)")  # session-level, survives commit
    assert _run(storage, sess, "SELECT objid FROM pg_catalog.pg_locks ORDER BY objid").rows == [
        (7,),
        (8,),
    ]
    _run(storage, sess, "COMMIT")
    # xact lock released, session lock survives.
    assert _run(storage, sess, "SELECT objid FROM pg_catalog.pg_locks").rows == [(8,)]


def test_xact_locks_release_at_rollback(storage):
    sess = Session(database=DB)
    _run(storage, sess, "BEGIN")
    _run(storage, sess, "SELECT pg_advisory_xact_lock_shared(11)")
    assert _run(storage, sess, "SELECT count(*) FROM pg_catalog.pg_locks").rows == [(1,)]
    _run(storage, sess, "ROLLBACK")
    assert _run(storage, sess, "SELECT count(*) FROM pg_catalog.pg_locks").rows == [(0,)]


def test_xact_lock_not_manually_unlockable(storage):
    sess = Session(database=DB)
    _run(storage, sess, "BEGIN")
    _run(storage, sess, "SELECT pg_advisory_xact_lock(20)")
    # A transaction-level lock isn't releasable via pg_advisory_unlock.
    assert _run(storage, sess, "SELECT pg_advisory_unlock(20)").rows == [(False,)]
    _run(storage, sess, "COMMIT")


def test_negative_and_large_bigint_key_split(storage):
    sess = Session(database=DB)
    # A single bigint splits into signed 32-bit (classid, objid); objsubid = 1.
    _run(storage, sess, "SELECT pg_advisory_lock(-1)")
    rows = _run(storage, sess, "SELECT classid, objid, objsubid FROM pg_catalog.pg_locks").rows
    assert rows == [(-1, -1, 1)]


def test_locks_are_per_session(storage):
    a = Session(database=DB, backend_pid=1)
    b = Session(database=DB, backend_pid=2)
    _run(storage, a, "SELECT pg_advisory_lock(1)")
    # Session b sees only its own locks (single-node dev surface).
    assert _run(storage, b, "SELECT count(*) FROM pg_catalog.pg_locks").rows == [(0,)]
