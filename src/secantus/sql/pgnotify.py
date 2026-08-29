"""``LISTEN`` / ``NOTIFY`` / ``UNLISTEN`` pub-sub for the Postgres wire server.

A single server-wide :class:`NotifyHub` maps a channel name to the set of
listening sessions. ``NOTIFY`` looks up the channel's listeners and appends a
``(pid, channel, payload)`` tuple to each listener session's own delivery queue;
the *owning* connection thread drains that queue and writes the
``NotificationResponse`` to its socket (so all socket writes stay on one thread —
no cross-thread stream interleaving). An idle listener that is blocked reading
its socket is woken by the connection loop's poll timeout, which drains and
flushes pending notifications.

Delivery ordering vs Postgres: a ``NOTIFY`` issued inside an open transaction
block is buffered on the session and delivered at ``COMMIT`` (discarded on
``ROLLBACK``); an autocommit ``NOTIFY`` delivers immediately. Duplicate
``(channel, payload)`` notifications within one transaction are *not* collapsed
(Postgres collapses them) — a documented simplification. ``LISTEN`` / ``UNLISTEN``
take effect immediately rather than at commit.
"""

from __future__ import annotations

import threading
from collections import defaultdict
from typing import Any


class NotifyHub:
    """Server-wide channel → listening-session registry. Thread-safe."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        # channel -> {id(session): session}. Keyed by id() because ``Session`` is
        # an (unhashable) dataclass; identity is exactly the semantics we want.
        self._channels: dict[str, dict[int, Any]] = defaultdict(dict)

    def listen(self, channel: str, session: Any) -> None:
        with self._lock:
            if id(session) not in self._channels[channel]:
                self._channels[channel][id(session)] = session
                session.listen_count = getattr(session, "listen_count", 0) + 1

    def unlisten(self, channel: str, session: Any) -> None:
        with self._lock:
            if self._channels.get(channel, {}).pop(id(session), None) is not None:
                session.listen_count = max(0, getattr(session, "listen_count", 1) - 1)

    def unlisten_all(self, session: Any) -> None:
        """Drop ``session`` from every channel (``UNLISTEN *`` / disconnect)."""
        with self._lock:
            for listeners in self._channels.values():
                listeners.pop(id(session), None)
            session.listen_count = 0

    def is_listening(self, session: Any) -> bool:
        """Whether ``session`` listens on any channel — the wire server's idle
        loop polls (and pushes queued notifications) only for listeners, so a
        connection that never LISTENed keeps its pure blocking read. Reads the
        per-session counter maintained under the hub lock by listen /
        unlisten / unlisten_all: this runs before EVERY message read on EVERY
        connection, and taking the hub lock there put a shared lock on the
        whole server's hot path."""
        return getattr(session, "listen_count", 0) > 0

    def notify(self, channel: str, payload: str, pid: int) -> None:
        """Deliver a notification to every session listening on ``channel`` by
        appending to each one's delivery queue (drained by its own thread)."""
        with self._lock:
            targets = list(self._channels.get(channel, {}).values())
        for sess in targets:
            sess.enqueue_notification(pid, channel, payload)
