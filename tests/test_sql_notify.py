"""LISTEN / NOTIFY / UNLISTEN (#120): the NotifyHub registry, the engine-level
command handling (transactional buffering, pg_notify), and channel identifier
folding. (The end-to-end wire delivery is covered in test_pgserver_pg8000.py.)
"""

from __future__ import annotations

import psycopg
import pytest

from secantus.sql import run_sql
from secantus.sql.pgnotify import NotifyHub
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


def _sessions():
    hub = NotifyHub()
    a = Session(database=DB, user="u", backend_pid=101)
    b = Session(database=DB, user="u", backend_pid=202)
    a.notify_hub = hub
    b.notify_hub = hub
    return hub, a, b


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


@pytest.fixture
def st(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


# --------------------------------------------------------------------------- #
# NotifyHub
# --------------------------------------------------------------------------- #


def test_hub_listen_notify():
    hub = NotifyHub()
    a = Session(backend_pid=1)
    hub.listen("chan", a)
    hub.notify("chan", "hi", 9)
    assert a.drain_notifications() == [(9, "chan", "hi")]
    # drained
    assert a.drain_notifications() == []


def test_hub_only_listeners_receive():
    hub = NotifyHub()
    a, b = Session(backend_pid=1), Session(backend_pid=2)
    hub.listen("chan", a)
    hub.notify("chan", "x", 5)
    assert a.drain_notifications() == [(5, "chan", "x")]
    assert b.drain_notifications() == []


def test_hub_unlisten():
    hub = NotifyHub()
    a = Session(backend_pid=1)
    hub.listen("chan", a)
    hub.unlisten("chan", a)
    hub.notify("chan", "x", 5)
    assert a.drain_notifications() == []


def test_hub_unlisten_all():
    hub = NotifyHub()
    a = Session(backend_pid=1)
    hub.listen("c1", a)
    hub.listen("c2", a)
    hub.unlisten_all(a)
    hub.notify("c1", "x", 5)
    hub.notify("c2", "y", 5)
    assert a.drain_notifications() == []


# --------------------------------------------------------------------------- #
# Engine command handling
# --------------------------------------------------------------------------- #


def test_listen_notify_command_tags(st):
    _hub, listener, notifier = _sessions()
    assert _run(st, listener, "LISTEN chan").command_tag == "LISTEN"
    assert _run(st, notifier, "NOTIFY chan").command_tag == "NOTIFY"
    assert _run(st, listener, "UNLISTEN chan").command_tag == "UNLISTEN"


def test_notify_delivers_with_payload(st):
    _hub, listener, notifier = _sessions()
    _run(st, listener, "LISTEN chan")
    _run(st, notifier, "NOTIFY chan, 'hello world'")
    assert listener.drain_notifications() == [(202, "chan", "hello world")]


def test_notify_payload_quote_escape(st):
    _hub, listener, notifier = _sessions()
    _run(st, listener, "LISTEN chan")
    _run(st, notifier, "NOTIFY chan, 'it''s here'")
    assert listener.drain_notifications() == [(202, "chan", "it's here")]


def test_pg_notify_function_form(st):
    _hub, listener, notifier = _sessions()
    _run(st, listener, "LISTEN chan")
    _run(st, notifier, "SELECT pg_notify('chan', 'via-func')")
    assert listener.drain_notifications() == [(202, "chan", "via-func")]


def test_notify_buffered_until_commit(st):
    _hub, listener, notifier = _sessions()
    _run(st, listener, "LISTEN chan")
    _run(st, notifier, "BEGIN")
    _run(st, notifier, "NOTIFY chan, 'in-txn'")
    assert listener.drain_notifications() == []  # not yet delivered
    _run(st, notifier, "COMMIT")
    assert listener.drain_notifications() == [(202, "chan", "in-txn")]


def test_notify_discarded_on_rollback(st):
    _hub, listener, notifier = _sessions()
    _run(st, listener, "LISTEN chan")
    _run(st, notifier, "BEGIN")
    _run(st, notifier, "NOTIFY chan, 'rolled'")
    _run(st, notifier, "ROLLBACK")
    assert listener.drain_notifications() == []


def test_unlisten_star(st):
    _hub, listener, notifier = _sessions()
    _run(st, listener, "LISTEN a")
    _run(st, listener, "LISTEN b")
    _run(st, listener, "UNLISTEN *")
    _run(st, notifier, "NOTIFY a")
    _run(st, notifier, "NOTIFY b")
    assert listener.drain_notifications() == []


def test_channel_identifier_folding(st):
    _hub, listener, notifier = _sessions()
    # Unquoted names fold to lower case; a quoted name keeps its case.
    _run(st, listener, "LISTEN MyChan")  # folds to "mychan"
    _run(st, notifier, "NOTIFY mychan")
    assert listener.drain_notifications() == [(202, "mychan", "")]

    _run(st, listener, 'LISTEN "CaseChan"')
    _run(st, notifier, 'NOTIFY "CaseChan"')
    assert listener.drain_notifications() == [(202, "CaseChan", "")]
    # ...and a lower-cased notify does NOT reach the case-sensitive listener.
    _run(st, notifier, "NOTIFY casechan")
    assert listener.drain_notifications() == []


def test_self_notify_delivered(st):
    # A session listening on a channel receives its own NOTIFY (Postgres does).
    _hub, sess, _other = _sessions()
    _run(st, sess, "LISTEN chan")
    _run(st, sess, "NOTIFY chan, 'self'")
    assert sess.drain_notifications() == [(101, "chan", "self")]


def test_embedded_no_hub_is_noop(st):
    # Without a hub (embedded run_sql), the commands are accepted as no-ops.
    s = Session(database=DB)
    assert _run(st, s, "LISTEN chan").command_tag == "LISTEN"
    assert _run(st, s, "NOTIFY chan, 'x'").command_tag == "NOTIFY"
    assert _run(st, s, "UNLISTEN chan").command_tag == "UNLISTEN"


# --------------------------------------------------------------------------- #
# Async wire delivery: real PG pushes a notification to an IDLE listening
# connection without waiting for its next query (pgx's WaitForNotification
# and psycopg's notifies() both just block reading the socket). Listening
# sessions poll in short slices and flush queued notifications from their own
# thread; non-listeners keep the pure blocking read.


@pytest.fixture()
def wire_server(tmp_path):
    srv = SecantusPGServer(storage_path=str(tmp_path), port=0)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture()
def wire_dsn(wire_server):
    host, port = wire_server.address
    return f"host={host} port={port} dbname=test user=test password=test"


def test_idle_listener_receives_pushed_notification(wire_dsn):
    with (
        psycopg.connect(wire_dsn, autocommit=True) as listener,
        psycopg.connect(wire_dsn, autocommit=True) as notifier,
    ):
        listener.execute("listen foo")
        notifier.execute("notify foo, 'bar'")
        got = list(listener.notifies(timeout=5, stop_after=1))
        assert len(got) == 1
        assert got[0].channel == "foo" and got[0].payload == "bar"


def test_notification_arrives_while_already_waiting(wire_dsn):
    import threading
    import time as _time

    with (
        psycopg.connect(wire_dsn, autocommit=True) as listener,
        psycopg.connect(wire_dsn, autocommit=True) as notifier,
    ):
        listener.execute("listen ping")
        t = threading.Thread(
            target=lambda: (_time.sleep(0.6), notifier.execute("notify ping, 'later'"))
        )
        t.start()
        got = list(listener.notifies(timeout=5, stop_after=1))
        t.join()
        assert [n.payload for n in got] == ["later"]


def test_endless_poll_is_woken_by_another_connection(wire_dsn):
    """A listener blocked with NO timeout is still woken by another connection.

    This is the shape pgjdbc's `NotifyTest` uses (`getNotifications(0)`, wait
    forever) and the reason that class is excluded from the pgjdbc gauge. The
    two tests above both pass a timeout, so the endless form — where nothing
    but the server's own push can end the wait — was untested.

    The wait runs on a daemon thread with a join deadline so a regression fails
    this test instead of hanging the suite, exactly what the JUnit version
    cannot do (a JUnit timeout can't interrupt that socket read).
    """
    import threading
    import time as _time

    with (
        psycopg.connect(wire_dsn, autocommit=True) as listener,
        psycopg.connect(wire_dsn, autocommit=True) as notifier,
    ):
        listener.execute("listen endless")
        got: list[str] = []

        def wait_forever() -> None:
            for n in listener.notifies():  # no timeout
                got.append(n.payload)
                break

        t = threading.Thread(target=wait_forever, daemon=True)
        t.start()
        _time.sleep(0.5)  # let it reach the blocking read
        notifier.execute("notify endless, 'woken'")
        t.join(timeout=10)
        assert not t.is_alive(), "the endless poll was never woken"
        assert got == ["woken"]


def test_idle_in_txn_timeout_still_fires_for_listener(tmp_path):
    srv = SecantusPGServer(storage_path=str(tmp_path), port=0, idle_in_transaction_timeout_s=0.5)
    srv.start()
    try:
        host, port = srv.address
        dsn = f"host={host} port={port} dbname=test user=test password=test"
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("listen chan")
            c.execute("begin")
            import time as _time

            _time.sleep(1.2)
            # The idle-in-transaction deadline must survive the notification
            # poll slices: the server killed the connection with 25P03.
            # Windows can discard the buffered FATAL on the server's close
            # (WSAECONNABORTED), so a bare connection error is accepted too —
            # the contract under test is that the connection is dead.
            with pytest.raises(
                (psycopg.errors.IdleInTransactionSessionTimeout, psycopg.OperationalError)
            ):
                c.execute("select 1")
    finally:
        srv.stop()
