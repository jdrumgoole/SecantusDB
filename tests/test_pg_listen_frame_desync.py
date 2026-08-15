"""A mid-frame poll wakeup on a LISTENing connection must not desync the wire
stream (#882).

When a session holds an active LISTEN, the server drops its read wait to a
0.25s poll so queued notifications flush promptly. The old implementation set
that as a socket *read timeout*, which could fire after ``read_message`` had
already consumed a frame's type byte or part of its length/payload — the
partial bytes were discarded and the next read re-synced on the wrong offset,
misinterpreting every subsequent byte. The fix waits for readability with
``select`` and only then reads a complete frame with a blocking recv, so a poll
wakeup can never truncate a frame.

This drives a real socket and deliberately fragments a Query message so its
tail lands after a poll interval, proving the frame still parses.
"""

from __future__ import annotations

import socket
import struct
import time

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

DB = "d"


def _startup(s: socket.socket) -> None:
    s.sendall(pgwire.build_startup_message({"user": "joe", "database": DB}))
    while pgwire.read_message(s).type != "Z":
        pass


def _query_bytes(sql: str) -> bytes:
    payload = sql.encode() + b"\x00"
    return b"Q" + struct.pack(">i", len(payload) + 4) + payload


def _drain_until_ready(s: socket.socket) -> list[str]:
    types: list[str] = []
    while True:
        m = pgwire.read_message(s)
        types.append(m.type)
        if m.type == "Z":
            return types


def test_fragmented_query_on_listening_conn_still_parses(tmp_path) -> None:
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        s = socket.create_connection((host, port), timeout=10)
        try:
            _startup(s)
            # Arm the idle-poll path: this session now LISTENs, so the server
            # waits in 0.25s slices.
            s.sendall(_query_bytes("LISTEN chan"))
            assert "Z" in _drain_until_ready(s)

            # Send a query in two fragments straddling >1 poll interval: the
            # type byte + length first, then the payload after 0.4s. A
            # read-timeout implementation would wake mid-frame between the two
            # and discard the first fragment.
            frame = _query_bytes("SELECT 1")
            split = 3  # after the type byte and part of the length
            s.sendall(frame[:split])
            time.sleep(0.4)
            s.sendall(frame[split:])

            types = _drain_until_ready(s)
            # A correctly-parsed SELECT 1 yields RowDescription/DataRow/
            # CommandComplete then ReadyForQuery — the point is it parsed at
            # all (no desync → no dropped connection).
            assert "T" in types and "D" in types and "C" in types and "Z" in types
        finally:
            s.close()
    finally:
        srv.stop()
        st.close()


def test_notifications_still_flush_while_idle_listening(tmp_path) -> None:
    """The select-based wait must still deliver async NOTIFY to an idle
    listener (the feature the poll exists for), not just avoid the desync."""
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        listener = socket.create_connection((host, port), timeout=10)
        notifier = socket.create_connection((host, port), timeout=10)
        try:
            _startup(listener)
            _startup(notifier)
            listener.sendall(_query_bytes("LISTEN chan"))
            _drain_until_ready(listener)

            # A different connection notifies; the idle listener must receive
            # an 'A' NotificationResponse without sending a query first.
            notifier.sendall(_query_bytes("NOTIFY chan, 'hi'"))
            _drain_until_ready(notifier)

            listener.settimeout(5)
            got_notification = False
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                m = pgwire.read_message(listener)
                if m.type == "A":
                    got_notification = True
                    break
            assert got_notification, "idle listener never received the async NOTIFY"
        finally:
            listener.close()
            notifier.close()
    finally:
        srv.stop()
        st.close()
