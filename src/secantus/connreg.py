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

import itertools
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


class ConnectionRegistry:
    """Thread-safe map of conn_id → ConnInfo for currently-open connections.

    All public methods take the internal lock; snapshots are returned as
    fresh ``ConnInfo`` instances so the caller cannot accidentally mutate
    registry state.
    """

    def __init__(self, time_func: object | None = None) -> None:
        self._conns: dict[int, ConnInfo] = {}
        self._lock = threading.Lock()
        self._next_id = itertools.count(1)
        self._time = time_func if callable(time_func) else time.time

    def open(self, peer_addr: tuple[str, int]) -> int:
        """Register a new connection and return its assigned ``conn_id``."""
        with self._lock:
            conn_id = next(self._next_id)
            self._conns[conn_id] = ConnInfo(
                conn_id=conn_id,
                peer_addr=peer_addr,
                opened_at=self._time(),
            )
            return conn_id

    def close(self, conn_id: int) -> None:
        """Drop a connection from the registry. No-op if already closed."""
        with self._lock:
            self._conns.pop(conn_id, None)

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
                )
                for info in sorted(self._conns.values(), key=lambda i: i.conn_id)
            ]

    def __len__(self) -> int:
        with self._lock:
            return len(self._conns)


__all__ = ["ConnectionRegistry", "ConnInfo"]
