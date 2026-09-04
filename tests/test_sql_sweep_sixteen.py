"""A sixteenth differential sweep — LISTEN / NOTIFY.

The delivery semantics were already right: cross-connection and self-delivery,
payloads, `pg_notify()`, `UNLISTEN` and `UNLISTEN *`, delivery deferred to
COMMIT, discard on ROLLBACK, channel-name case folding (and quoted names
keeping their case), and a repeated `LISTEN` still delivering once. Three
things were not.

**`pg_notify()` delivered the notification TWICE.** Describe evaluates a
FROM-less SELECT to learn its column shape, and `engine._VOLATILE_FN_TAGS` —
the table that exists precisely so Describe derives a volatile call's shape
statically instead of running it — was missing `pg_notify`. So Describe sent
the notification and Execute sent it again. It only reproduces through the
extended protocol, because a parameter is what stops a driver using the simple
one: `SELECT pg_notify('c','p')` written with literals looked perfectly fine,
which is why a literal corpus could never have found it.

The blast radius was checked rather than assumed — `nextval`, `pg_sleep`, the
advisory locks, INSERT and a CTE INSERT with parameters were all measured and
are all correct, because they were already in that table. `pg_notify` was the
only omission, and the only one of them whose side effect is externally
visible.

**Duplicate notifications did not collapse.** PostgreSQL delivers one event
when the same channel is signalled with an identical payload more than once in
a transaction, so a loop that notifies per row wakes a listener once rather
than once per row. Both spellings now collapse; distinct payloads on one
channel are still all delivered, so this deduplicates the PAIR, not the
channel.

**The payload cap was not enforced.** 7999 bytes is accepted and 8000 is not
(measured), so the limit is exclusive.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def servers(tmp_path):
    st = Storage(str(tmp_path / "s16"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    host, port = srv.address

    def connect():
        return psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True)

    listener, notifier = connect(), connect()
    try:
        yield listener, notifier
    finally:
        listener.close()
        notifier.close()
        srv.stop()
        st.close()


def drain(c, timeout=1.0, expect=1):
    """(channel, payload) pairs. The PID is excluded — it differs per run."""
    gen = c.notifies(timeout=timeout, stop_after=expect) if expect else c.notifies(timeout=timeout)
    return sorted((n.channel, n.payload) for n in gen)


def sqlstate(exc):
    return getattr(getattr(exc, "diag", None), "sqlstate", None)


# --- the double-delivery bug ------------------------------------------------- #


def test_pg_notify_with_a_parameter_delivers_once(servers):
    """Describe used to evaluate the call and Execute evaluate it again."""
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("SELECT pg_notify('ch', %s)", ("payload",))
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "payload")]


def test_pg_notify_prepared_delivers_once(servers):
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("SELECT pg_notify('ch', %s)", ("p",), prepare=True)
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "p")]


def test_pg_notify_with_literals_still_delivers_once(servers):
    """The simple-protocol spelling was always right; pinned so a fix to the
    extended path cannot break it."""
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("SELECT pg_notify('ch', 'lit')")
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "lit")]


def test_a_parameterised_notify_statement_delivers_once(servers):
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("NOTIFY ch, 'stmt'")
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "stmt")]


# --- duplicate collapse within a transaction --------------------------------- #


def test_identical_notifies_in_one_transaction_collapse(servers):
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("BEGIN")
    notifier.execute("NOTIFY ch, 'same'")
    notifier.execute("NOTIFY ch, 'same'")
    notifier.execute("COMMIT")
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "same")]


def test_pg_notify_collapses_the_same_way(servers):
    """The two spellings of one operation had different semantics."""
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("BEGIN")
    notifier.execute("SELECT pg_notify('ch', 'same')")
    notifier.execute("SELECT pg_notify('ch', 'same')")
    notifier.execute("COMMIT")
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "same")]


def test_distinct_payloads_are_all_delivered(servers):
    """The collapse is of the (channel, payload) PAIR, not of the channel."""
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("BEGIN")
    notifier.execute("NOTIFY ch, 'one'")
    notifier.execute("NOTIFY ch, 'two'")
    notifier.execute("COMMIT")
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "one"), ("ch", "two")]


def test_outside_a_transaction_repeats_are_separate_events(servers):
    """Each autocommit statement is its own transaction, so there is nothing to
    collapse against."""
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("NOTIFY ch, 'x'")
    notifier.execute("NOTIFY ch, 'x'")
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "x"), ("ch", "x")]


# --- payload cap ------------------------------------------------------------- #


def test_payload_of_7999_is_accepted(servers):
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("NOTIFY ch, '" + "x" * 7999 + "'")
    assert [(c, len(p)) for c, p in drain(listener, expect=0, timeout=0.8)] == [("ch", 7999)]


@pytest.mark.parametrize("size", [8000, 9000])
def test_payload_of_8000_or_more_is_refused(servers, size):
    _listener, notifier = servers
    with pytest.raises(psycopg.Error) as ei:
        notifier.execute("NOTIFY ch, '" + "x" * size + "'")
    assert sqlstate(ei.value) == "22023"
    assert "payload string too long" in str(ei.value)


def test_pg_notify_enforces_the_same_cap(servers):
    _listener, notifier = servers
    with pytest.raises(psycopg.Error) as ei:
        notifier.execute("SELECT pg_notify('ch', %s)", ("x" * 8000,))
    assert sqlstate(ei.value) == "22023"


# --- regression cover for what was already correct --------------------------- #


def test_delivery_waits_for_commit(servers):
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("BEGIN")
    notifier.execute("NOTIFY ch, 'txn'")
    assert drain(listener, expect=0, timeout=0.3) == []
    notifier.execute("COMMIT")
    assert drain(listener, expect=0, timeout=0.8) == [("ch", "txn")]


def test_rollback_discards(servers):
    listener, notifier = servers
    listener.execute("LISTEN ch")
    notifier.execute("BEGIN")
    notifier.execute("NOTIFY ch, 'gone'")
    notifier.execute("ROLLBACK")
    assert drain(listener, expect=0, timeout=0.4) == []


def test_channel_names_fold_but_quoted_ones_do_not(servers):
    listener, notifier = servers
    listener.execute("LISTEN MixedCase")
    notifier.execute("NOTIFY mixedcase, 'folded'")
    assert drain(listener, expect=0, timeout=0.8) == [("mixedcase", "folded")]
    listener.execute('LISTEN "KeepCase"')
    notifier.execute("NOTIFY othercase, 'no'")
    assert drain(listener, expect=0, timeout=0.3) == []
    notifier.execute("NOTIFY \"KeepCase\", 'kept'")
    assert drain(listener, expect=0, timeout=0.8) == [("KeepCase", "kept")]


def test_unlisten_star_stops_everything(servers):
    listener, notifier = servers
    listener.execute("LISTEN a1")
    listener.execute("LISTEN a2")
    listener.execute("UNLISTEN *")
    notifier.execute("NOTIFY a1")
    notifier.execute("NOTIFY a2")
    assert drain(listener, expect=0, timeout=0.4) == []
