"""In-process log ring buffer surfaced through ``getLog``.

Real ``mongod`` keeps a bounded in-memory log accessible via
``db.adminCommand({getLog: "global"})``. SecantusDB's ``getLog`` was a
stub returning an empty array; this module provides the backing store
so admin tooling, the new admin UI, and ad-hoc operators can see what
the server has been doing without parsing stderr.

Pure module — no I/O outside the in-memory deque. Held on
``SecantusDBServer.logs``, written by ``commands.dispatch`` (and any
other site that wants log surface), read by :func:`secantus.commands._get_log`.
"""

from __future__ import annotations

import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import Any

_DEFAULT_CAPACITY = 5000


@dataclass
class LogEntry:
    """One line in the in-memory log."""

    ts: float
    level: str  # "I" / "W" / "E" / "D" matching mongod's severity letters
    component: str  # "COMMAND" / "STORAGE" / "NETWORK" / ...
    msg: str
    ctx: dict[str, Any] | None = None  # arbitrary per-entry payload


class LogBuffer:
    """Thread-safe bounded ring buffer of log entries.

    ``capacity`` is the maximum number of entries retained — older
    entries are dropped when full. Reads return fresh lists so the
    caller cannot mutate buffered state.
    """

    def __init__(self, capacity: int = _DEFAULT_CAPACITY) -> None:
        if capacity <= 0:
            raise ValueError("capacity must be positive")
        self._entries: deque[LogEntry] = deque(maxlen=capacity)
        self._lock = threading.Lock()
        self.capacity = capacity

    def append(
        self,
        level: str,
        component: str,
        msg: str,
        ctx: dict[str, Any] | None = None,
    ) -> None:
        entry = LogEntry(ts=time.time(), level=level, component=component, msg=msg, ctx=ctx)
        with self._lock:
            self._entries.append(entry)

    def tail(self, n: int | None = None) -> list[LogEntry]:
        """Return the most recent ``n`` entries (oldest-first). ``None`` = all."""
        with self._lock:
            if n is None or n >= len(self._entries):
                return list(self._entries)
            # deque slicing requires a list copy — buffer is bounded so this is cheap.
            return list(self._entries)[-n:]

    def __len__(self) -> int:
        with self._lock:
            return len(self._entries)


__all__ = ["LogBuffer", "LogEntry"]
