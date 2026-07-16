"""LISTEN / NOTIFY / UNLISTEN (#120): the NotifyHub registry, the engine-level
command handling (transactional buffering, pg_notify), and channel identifier
folding. (The end-to-end wire delivery is covered in test_pgserver_pg8000.py.)
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.pgnotify import NotifyHub
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
