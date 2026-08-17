"""Advisory locks (#135): the ``pg_advisory_lock`` family as session-tracked
single-node no-op locking, reflected through ``pg_catalog.pg_locks``. Single
node → a lock is always granted immediately; we track what the session holds so
``pg_advisory_unlock`` reports truthfully and ``pg_locks`` reflects it. Driven
over the real ``Storage``.
"""

from __future__ import annotations

import threading
import time

import pytest

from secantus.sql import errors as sql_errors
from secantus.sql import run_sql
from secantus.sql.pgadvisory import AdvisoryLockHub
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


# --- cross-connection exclusion (the AdvisoryLockHub) -----------------------
#
# The hub is the server-wide authority the wire server shares across
# connections; two embedded Sessions sharing one hub reproduce the
# cross-connection semantics without a socket in the way.


def _hub_pair():
    hub = AdvisoryLockHub()
    s1 = Session(database=DB, backend_pid=101)
    s2 = Session(database=DB, backend_pid=102)
    s1.advisory_hub = hub
    s2.advisory_hub = hub
    return s1, s2


def test_exclusive_lock_excludes_other_sessions(storage):
    s1, s2 = _hub_pair()
    assert _run(storage, s1, "SELECT pg_try_advisory_lock(7)").rows == [(True,)]
    # Another session can neither try-take it...
    assert _run(storage, s2, "SELECT pg_try_advisory_lock(7)").rows == [(False,)]
    # ...but the holder re-enters freely.
    assert _run(storage, s1, "SELECT pg_try_advisory_lock(7)").rows == [(True,)]
    _run(storage, s1, "SELECT pg_advisory_unlock(7)")
    assert _run(storage, s2, "SELECT pg_try_advisory_lock(7)").rows == [(False,)]
    _run(storage, s1, "SELECT pg_advisory_unlock(7)")
    assert _run(storage, s2, "SELECT pg_try_advisory_lock(7)").rows == [(True,)]


def test_blocking_acquire_waits_for_the_holder(storage):
    s1, s2 = _hub_pair()
    _run(storage, s1, "SELECT pg_advisory_lock(8)")
    got = []

    def waiter():
        _run(storage, s2, "SELECT pg_advisory_lock(8)")
        got.append(time.monotonic())

    t = threading.Thread(target=waiter, daemon=True)
    t.start()
    time.sleep(0.3)
    assert not got, "waiter acquired while the lock was held"
    _run(storage, s1, "SELECT pg_advisory_unlock(8)")
    t.join(10)
    assert got, "waiter never acquired after release"


def test_shared_locks_coexist_but_block_exclusive(storage):
    s1, s2 = _hub_pair()
    assert _run(storage, s1, "SELECT pg_try_advisory_lock_shared(9)").rows == [(True,)]
    assert _run(storage, s2, "SELECT pg_try_advisory_lock_shared(9)").rows == [(True,)]
    assert _run(storage, s1, "SELECT pg_try_advisory_lock(9)").rows == [(False,)]
    _run(storage, s2, "SELECT pg_advisory_unlock_shared(9)")
    # Own shared hold doesn't block own exclusive.
    assert _run(storage, s1, "SELECT pg_try_advisory_lock(9)").rows == [(True,)]


def test_deadlock_is_detected(storage):
    s1, s2 = _hub_pair()
    _run(storage, s1, "SELECT pg_advisory_lock(11)")
    _run(storage, s2, "SELECT pg_advisory_lock(12)")
    outcomes: list[str] = []
    lock_ = threading.Lock()

    def cross(sess, key):
        try:
            _run(storage, sess, f"SELECT pg_advisory_lock({key})")
            with lock_:
                outcomes.append("acquired")
        except sql_errors.SQLError as exc:
            with lock_:
                outcomes.append(exc.sqlstate)
            # Free what this session holds so the other waiter can finish.
            sess.advisory_hub.release_all(sess)

    t1 = threading.Thread(target=cross, args=(s1, 12), daemon=True)
    t2 = threading.Thread(target=cross, args=(s2, 11), daemon=True)
    t1.start()
    t2.start()
    t1.join(15)
    t2.join(15)
    assert not t1.is_alive() and not t2.is_alive(), f"deadlock not resolved: {outcomes}"
    assert "40P01" in outcomes, f"no deadlock error surfaced: {outcomes}"


def test_xact_lock_releases_at_commit_for_other_sessions(storage):
    s1, s2 = _hub_pair()
    _run(storage, s1, "BEGIN")
    _run(storage, s1, "SELECT pg_advisory_xact_lock(13)")
    assert _run(storage, s2, "SELECT pg_try_advisory_lock(13)").rows == [(False,)]
    _run(storage, s1, "COMMIT")
    assert _run(storage, s2, "SELECT pg_try_advisory_lock(13)").rows == [(True,)]


def test_session_teardown_releases_hub_holds(storage):
    s1, s2 = _hub_pair()
    _run(storage, s1, "SELECT pg_advisory_lock(14)")
    assert _run(storage, s2, "SELECT pg_try_advisory_lock(14)").rows == [(False,)]
    s1.advisory_hub.release_all(s1)  # what pgserver's teardown calls
    assert _run(storage, s2, "SELECT pg_try_advisory_lock(14)").rows == [(True,)]


def test_wire_level_cross_connection_exclusion(tmp_path):
    # End-to-end over real sockets: the wire server attaches one hub to every
    # connection, and a dropped connection releases its holds.
    psycopg = pytest.importorskip("psycopg")
    from secantus.sql.pgserver import SecantusPGServer

    st = Storage(str(tmp_path / "wire"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        dsn = f"host=127.0.0.1 port={srv.address[1]} dbname={DB} user=postgres"
        with (
            psycopg.connect(dsn, autocommit=True) as c1,
            psycopg.connect(dsn, autocommit=True) as c2,
        ):
            assert c1.execute("SELECT pg_try_advisory_lock(21)").fetchone() == (True,)
            assert c2.execute("SELECT pg_try_advisory_lock(21)").fetchone() == (False,)
            c1.execute("SELECT pg_advisory_unlock(21)")
            assert c2.execute("SELECT pg_try_advisory_lock(21)").fetchone() == (True,)
        # Both connections closed — a fresh one can take either key. The
        # server releases a dropped connection's holds in ITS OWN connection
        # thread's teardown, which the client's close() does not wait for, so
        # poll for the release instead of assuming it is instantaneous (a
        # bare assert here failed on a loaded CI runner: c3 probed before the
        # server had reaped c2).
        with psycopg.connect(dsn, autocommit=True) as c3:
            deadline = time.monotonic() + 10
            while True:
                got = c3.execute("SELECT pg_try_advisory_lock(21)").fetchone()
                if got == (True,) or time.monotonic() >= deadline:
                    break
                time.sleep(0.05)
            assert got == (True,)
    finally:
        srv.stop()
        st.close()
