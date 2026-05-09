"""Per-server metrics surfaced through ``serverStatus``.

Tracks the small set of counters real ``mongod`` exposes that drivers
and ops dashboards actually look at:

  * **uptime** — server start time (computed against ``time.monotonic()``
    so wall-clock jumps don't perturb it).
  * **connections** — current open + lifetime total.
  * **opcounters** — per-mutating-command counts (insert, query, update,
    delete, getmore, command). Match mongod's bucket names so existing
    monitoring queries (``db.serverStatus().opcounters.insert`` etc.)
    Just Work.
  * **network** — total wire-protocol requests (mirrors mongod's
    ``network.numRequests``; we collapse bytes-in / bytes-out since we
    don't track those at the dispatch layer).

Pure module — no I/O, no Storage import. Held on
``SecantusDBServer.metrics``, threaded into ``CommandContext.metrics``,
and read in :func:`secantus.commands._server_status`.
"""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass, field


@dataclass
class _OpCounters:
    """Per-command-bucket lifetime counts (matches mongod's shape)."""

    insert: int = 0
    query: int = 0
    update: int = 0
    delete: int = 0
    getmore: int = 0
    command: int = 0  # everything that doesn't fit the above buckets

    def to_doc(self) -> dict[str, int]:
        return {
            "insert": self.insert,
            "query": self.query,
            "update": self.update,
            "delete": self.delete,
            "getmore": self.getmore,
            "command": self.command,
        }


# Map command name → opcounter bucket. The shape mirrors mongod's
# accounting: `find` is "query", every cursor continuation is
# "getmore", CRUD goes to its own bucket, everything else is "command".
_COMMAND_BUCKET: dict[str, str] = {
    "insert": "insert",
    "find": "query",
    "count": "query",
    "distinct": "query",
    "update": "update",
    "findAndModify": "update",
    "findandmodify": "update",
    "delete": "delete",
    "getMore": "getmore",
}


@dataclass
class Metrics:
    """Thread-safe per-server counters.

    ``connections_current`` / ``connections_total`` are bumped from the
    accept loop and the per-connection finally-block; opcounters /
    requests bump from ``commands.dispatch`` once per request.

    A single ``threading.Lock`` serialises the counter writes; reads
    take a snapshot under the lock so a ``serverStatus`` reply is
    internally consistent.
    """

    start_monotonic: float = field(default_factory=time.monotonic)
    start_wallclock: float = field(default_factory=time.time)
    connections_current: int = 0
    connections_total: int = 0
    requests: int = 0
    op_counters: _OpCounters = field(default_factory=_OpCounters)
    _lock: threading.Lock = field(default_factory=threading.Lock)

    # ---- connection lifecycle ----------------------------------------------

    def connection_opened(self) -> None:
        with self._lock:
            self.connections_current += 1
            self.connections_total += 1

    def connection_closed(self) -> None:
        with self._lock:
            if self.connections_current > 0:
                self.connections_current -= 1

    # ---- per-request -------------------------------------------------------

    def record_command(self, name: str) -> None:
        """Increment counters for one dispatched command."""
        bucket = _COMMAND_BUCKET.get(name, "command")
        with self._lock:
            self.requests += 1
            setattr(
                self.op_counters,
                bucket,
                getattr(self.op_counters, bucket) + 1,
            )

    # ---- snapshot ----------------------------------------------------------

    def snapshot(self) -> dict[str, object]:
        """Return a serialisable snapshot for ``serverStatus``.

        ``uptime`` / ``uptimeMillis`` are integer seconds / millis since
        ``start_monotonic`` (matches mongod's int-truncation behaviour).
        ``localTime`` and ``opcounters`` round out the conventional
        envelope; the caller adds host / version / process / pid before
        returning to the wire.
        """
        with self._lock:
            now = time.monotonic()
            uptime_s = int(now - self.start_monotonic)
            uptime_ms = int((now - self.start_monotonic) * 1000)
            return {
                "uptime": uptime_s,
                "uptimeMillis": uptime_ms,
                "uptimeEstimate": uptime_s,
                "connections": {
                    "current": self.connections_current,
                    "available": 0,  # unbounded in practice; mongod-shape requires the field
                    "totalCreated": self.connections_total,
                },
                "opcounters": self.op_counters.to_doc(),
                "network": {
                    "numRequests": self.requests,
                    # bytesIn / bytesOut aren't tracked at the dispatch
                    # layer — surfaced as zero rather than dropped so
                    # mongod-shaped tooling doesn't trip on missing keys.
                    "bytesIn": 0,
                    "bytesOut": 0,
                },
            }
