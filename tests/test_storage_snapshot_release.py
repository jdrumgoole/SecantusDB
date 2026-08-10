"""An idle thread's cached WT session must never pin the oldest-transaction
horizon (the pgjdbc CopyLargeFileTest wedge: one idle connection's positioned
cursor made every later write's history unevictable, degrading linearly with
churn until page reads stalled the server).

``release_thread_snapshot`` is the guarantee: both wire servers call it before
blocking for the next client message. These tests read WiredTiger's
``transaction range of IDs currently pinned`` statistic directly, so they are
deterministic — no timing thresholds."""

from __future__ import annotations

import threading

import pytest

import secantus.storage as storage_mod
from secantus.storage import Storage


@pytest.fixture
def stats_storage(tmp_path, monkeypatch):
    """A Storage whose WT connection has statistics enabled, so tests can read
    the pinned-transaction-range counter."""
    orig_open = storage_mod.wt.wiredtiger_open
    monkeypatch.setattr(
        storage_mod.wt,
        "wiredtiger_open",
        lambda home, config: orig_open(home, config + ",statistics=(fast)"),
    )
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def pinned_range(s: Storage) -> int:
    sess = s._conn.open_session()
    try:
        cur = sess.open_cursor("statistics:", None, "statistics=(fast)")
        val = 0
        while cur.next() == 0:
            _, desc, _, v = cur.get_key(), *cur.get_value()
            if desc.endswith("range of IDs currently pinned"):
                val = v
        cur.close()
        return val
    finally:
        sess.close()


def churn(s: Storage, n: int = 200) -> None:
    for i in range(n):
        s.update_matching("db", "seq", {"_id": "s"}, {"$set": {"v": i}})


def test_positioned_cursor_pins_and_release_unpins(stats_storage):
    s = stats_storage
    s.insert("db", "seq", [{"_id": "s", "v": 0}])
    s.insert("db", "data", [{"_id": i} for i in range(10)])

    ready = threading.Event()
    release = threading.Event()
    done = threading.Event()

    def reader() -> None:
        # Position a cursor mid-table and stop — the implicit transaction
        # holds this thread's read snapshot, exactly the state a wire
        # connection could be left in after a statement.
        cur = s._cursor(storage_mod._doc_table_for("db", "data"))
        cur.next()
        ready.set()
        release.wait(timeout=30)
        s.release_thread_snapshot()
        done.set()

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    assert ready.wait(timeout=10)

    churn(s)
    pinned_before = pinned_range(s)
    assert pinned_before > 100, (
        f"expected the positioned cursor to pin the oldest-txn horizon "
        f"(pinned={pinned_before}) — if this stopped pinning, the test's "
        f"staging no longer models the wedge"
    )

    release.set()
    assert done.wait(timeout=10)
    t.join(timeout=10)

    churn(s, n=20)  # advance the counter so a stale pin would show
    pinned_after = pinned_range(s)
    assert pinned_after < 50, (
        f"release_thread_snapshot did not release the pinned snapshot (pinned={pinned_after})"
    )


def test_release_is_noop_inside_user_transaction(stats_storage):
    s = stats_storage
    s.insert("db", "c", [{"_id": 1}])
    h = s.begin_user_transaction()
    with s.use_user_transaction(h):
        s.insert("db", "c", [{"_id": 2}])
        # Inside the installed transaction the release must not touch the
        # transaction's session — its snapshot is the txn's semantics.
        s.release_thread_snapshot()
    s.commit_user_transaction(h)
    assert sorted(d["_id"] for d in s.find_matching("db", "c", {})) == [1, 2]


def test_idle_pg_connection_never_pins(tmp_path, monkeypatch):
    """Wire-level invariant: whatever statements a connection ran, once it
    goes idle it holds no snapshot — churn from another connection never
    accumulates a pinned range."""
    psycopg = pytest.importorskip("psycopg")
    from secantus.sql.pgserver import SecantusPGServer

    orig_open = storage_mod.wt.wiredtiger_open
    monkeypatch.setattr(
        storage_mod.wt,
        "wiredtiger_open",
        lambda home, config: orig_open(home, config + ",statistics=(fast)"),
    )
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        dsn = f"host={host} port={port} dbname=db user=t"
        idle = psycopg.connect(dsn)  # autocommit off, like JDBC defaults
        idle.execute("CREATE TABLE a (id int, v text)")
        idle.commit()
        idle.execute("INSERT INTO a VALUES (1, 'x')")
        idle.commit()
        idle.cursor().execute("SELECT * FROM a")
        idle.rollback()  # end the block; connection now idles holding nothing
        with psycopg.connect(dsn, autocommit=True) as work:
            for i in range(300):
                work.execute("INSERT INTO a VALUES (%s, 'y')", (i,))
            # The work connection's own snapshot is released when its server
            # thread loops back to the pre-idle release — poll briefly rather
            # than racing it. The invariant under test is the *idle* handling:
            # without the release, the idle connection pins the whole churn
            # (pinned ≈ 300) and this never converges.
            import time

            for _ in range(50):
                if pinned_range(st) < 100:
                    break
                time.sleep(0.1)
            assert pinned_range(st) < 100
        idle.close()
    finally:
        srv.stop()
        st.close()
