"""Per-server connection registry surfaced through ``currentOp``.

Tracks the small set of facts mongod exposes per active client connection
that admin tools and the upcoming admin UI rely on:

  * **conn_id** — monotonic, mirrors mongod's ``connectionId``.
  * **peer_addr** — the (host, port) tuple of the remote client.
  * **opened_at / last_cmd_at** — wall-clock timestamps; cheap to render
    as "uptime" / "idle for" in dashboards.
  * **op_count** — lifetime command count for this connection.
  * **user** — authenticated principal once SCRAM completes; ``None``
    until then.
  * **last_command_name** — the most recent dispatched command, useful
    for "what is this connection doing right now" panels.

Pure module — no I/O, no Storage import. Held on
``SecantusDBServer.connections``, mutated from the accept loop and
``commands.dispatch``, read from the ``currentOp`` command handler.
"""

from __future__ import annotations

import contextlib
import itertools
import socket
import sys
import threading
import time
from dataclasses import dataclass


@dataclass
class ConnInfo:
    """Snapshot of a single active connection."""

    conn_id: int
    peer_addr: tuple[str, int]
    opened_at: float
    last_cmd_at: float | None = None
    op_count: int = 0
    user: str | None = None
    last_command_name: str | None = None
    # Driver self-identification sent in the ``hello`` handshake's
    # ``client`` subdoc (per the MongoDB Handshake spec): driver
    # name + version, OS info, platform string, application name.
    # ``currentOp`` surfaces this as ``clientMetadata`` on each
    # in-progress op — that's how drivers identify their own
    # connections in admin tooling. mongo-rust-driver's
    # ``test::client::metadata_sent_in_handshake`` reads this
    # subdoc back via ``currentOp``. Stored verbatim; the emit
    # path doesn't reshape it.
    client_metadata: dict | None = None


class ConnectionRegistry:
    """Thread-safe map of conn_id → ConnInfo for currently-open connections.

    All public methods take the internal lock; snapshots are returned as
    fresh ``ConnInfo`` instances so the caller cannot accidentally mutate
    registry state.

    ``kill(conn_id)`` shuts down the underlying socket so the per-
    connection thread's blocking ``recv`` returns and the loop exits.
    The socket reference lives alongside ``ConnInfo`` but is never
    handed out — only the registry calls ``shutdown`` on it.
    """

    def __init__(self, time_func: object | None = None) -> None:
        self._conns: dict[int, ConnInfo] = {}
        self._sockets: dict[int, socket.socket] = {}
        self._lock = threading.Lock()
        self._next_id = itertools.count(1)
        self._time = time_func if callable(time_func) else time.time

    def open(self, peer_addr: tuple[str, int], sock: socket.socket | None = None) -> int:
        """Register a new connection and return its assigned ``conn_id``.

        ``sock`` is the per-connection socket. When non-None it's kept
        so ``kill(conn_id)`` can shut it down from another thread.
        """
        with self._lock:
            conn_id = next(self._next_id)
            self._conns[conn_id] = ConnInfo(
                conn_id=conn_id,
                peer_addr=peer_addr,
                opened_at=self._time(),
            )
            if sock is not None:
                self._sockets[conn_id] = sock
            return conn_id

    def close(self, conn_id: int) -> None:
        """Drop a connection from the registry. No-op if already closed."""
        with self._lock:
            self._conns.pop(conn_id, None)
            self._sockets.pop(conn_id, None)

    def kill(self, conn_id: int) -> bool:
        """Forcibly close the connection by shutting down its socket.

        Returns True if the connection existed (and a shutdown was
        attempted), False if already gone. Real mongod's ``killOp``
        signals an interrupt flag the long-running paths poll; we
        don't have per-op cancellation, so the closest faithful
        semantic is "close the socket" — any in-flight command runs
        to completion, the connection thread's next ``recv`` returns
        0, the loop exits, and the connection unregisters cleanly.

        Idempotent: a second call after the thread has already
        unregistered returns False with no error.
        """
        with self._lock:
            sock = self._sockets.get(conn_id)
            if sock is None:
                return False
        with contextlib.suppress(OSError):
            sock.shutdown(socket.SHUT_RDWR)
        return True

    def close_all(self) -> None:
        """Wake every registered connection's blocked ``recv`` so the handler
        threads return and exit. Used at server stop to drain in-flight
        connections before the storage engine closes (a handler thread
        mid-WiredTiger-op when the WT connection closes is a use-after-free /
        native crash). Idempotent — safe to call repeatedly during the drain
        poll, which is how late-registering connections get caught.

        Platform split, because the primitive that wakes a ``recv`` blocked in
        *another* thread differs by OS:

          * **POSIX**: ``shutdown(SHUT_RDWR)`` wakes the blocked ``recv``
            (returns EOF) while leaving the fd valid; the owning handler closes
            it later via its ``with conn`` block. We must **not** ``close()``
            here — closing the fd from this thread does not wake the parked
            ``recv`` (it keeps reading the old fd), and the freed fd number is
            immediately reusable by another socket / WiredTiger, so the handler
            ends up blocked forever on a recycled descriptor. ``shutdown``-only
            is the correct, race-free POSIX wake.
          * **Windows**: ``shutdown`` does *not* interrupt an already-blocked
            ``recv`` (it only affects the next call), so the handler would stay
            parked and the drain barrier would time out. ``closesocket`` is what
            aborts the in-progress ``recv`` there, and Windows handles aren't
            recycled under a blocked read the way POSIX fds are, so the
            reuse hazard above doesn't apply."""
        with self._lock:
            socks = list(self._sockets.values())
        for sock in socks:
            with contextlib.suppress(OSError):
                sock.shutdown(socket.SHUT_RDWR)
            if sys.platform == "win32":
                with contextlib.suppress(OSError):
                    sock.close()

    def record_command(self, conn_id: int, name: str) -> None:
        """Bump ``op_count`` and set ``last_command_name`` / ``last_cmd_at``."""
        with self._lock:
            info = self._conns.get(conn_id)
            if info is None:
                return
            info.op_count += 1
            info.last_command_name = name
            info.last_cmd_at = self._time()

    def authenticate(self, conn_id: int, user: str) -> None:
        """Mark a connection as authenticated as ``user``."""
        with self._lock:
            info = self._conns.get(conn_id)
            if info is None:
                return
            info.user = user

    def set_client_metadata(self, conn_id: int, metadata: dict) -> None:
        """Stash the ``hello.client`` subdoc the driver sent.

        Per the MongoDB Handshake spec, drivers send their
        self-identification once per connection on the first
        ``hello`` / ``isMaster`` command. We stash it on the
        registry so ``currentOp`` can echo it back as
        ``clientMetadata`` on the corresponding in-progress op.
        Idempotent — drivers MAY re-send on a later ``hello`` for
        speculative-auth refresh; we just replace.
        """
        with self._lock:
            info = self._conns.get(conn_id)
            if info is None:
                return
            info.client_metadata = dict(metadata)

    def get(self, conn_id: int) -> ConnInfo | None:
        """Return a fresh copy of the ``ConnInfo`` for ``conn_id`` or ``None``.

        Single-connection lookup that avoids walking the full snapshot.
        Used by handlers like ``whatsmyuri`` that only need the current
        connection's peer address.
        """
        with self._lock:
            info = self._conns.get(conn_id)
            if info is None:
                return None
            return ConnInfo(
                conn_id=info.conn_id,
                peer_addr=info.peer_addr,
                opened_at=info.opened_at,
                last_cmd_at=info.last_cmd_at,
                op_count=info.op_count,
                user=info.user,
                last_command_name=info.last_command_name,
                client_metadata=dict(info.client_metadata)
                if info.client_metadata is not None
                else None,
            )

    def snapshot(self) -> list[ConnInfo]:
        """Return a fresh list of ``ConnInfo`` copies, sorted by ``conn_id``."""
        with self._lock:
            return [
                ConnInfo(
                    conn_id=info.conn_id,
                    peer_addr=info.peer_addr,
                    opened_at=info.opened_at,
                    last_cmd_at=info.last_cmd_at,
                    op_count=info.op_count,
                    user=info.user,
                    last_command_name=info.last_command_name,
                    client_metadata=dict(info.client_metadata)
                    if info.client_metadata is not None
                    else None,
                )
                for info in sorted(self._conns.values(), key=lambda i: i.conn_id)
            ]

    def __len__(self) -> int:
        with self._lock:
            return len(self._conns)


__all__ = ["ConnectionRegistry", "ConnInfo"]
