from __future__ import annotations

import itertools
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

DEFAULT_IDLE_TTL_SECONDS = 600.0  # matches MongoDB's 10-minute cursor TTL.


class CursorNotFound(Exception):
    def __init__(self, cursor_id: int) -> None:
        super().__init__(f"cursor id {cursor_id} not found")
        self.cursor_id = cursor_id


@dataclass
class _Entry:
    cursor_id: int
    namespace: str
    remaining: list[dict[str, Any]]
    last_access: float


class CursorRegistry:
    def __init__(
        self,
        idle_ttl_seconds: float = DEFAULT_IDLE_TTL_SECONDS,
        time_func: Callable[[], float] | None = None,
    ) -> None:
        self._cursors: dict[int, _Entry] = {}
        self._lock = threading.Lock()
        self._next_id = itertools.count(1)
        self.idle_ttl_seconds = idle_ttl_seconds
        self._time = time_func or time.monotonic

    def _prune_locked(self) -> None:
        if self.idle_ttl_seconds <= 0:
            return
        cutoff = self._time() - self.idle_ttl_seconds
        expired = [cid for cid, e in self._cursors.items() if e.last_access < cutoff]
        for cid in expired:
            del self._cursors[cid]

    def register(self, namespace: str, remaining: list[dict[str, Any]]) -> int:
        with self._lock:
            self._prune_locked()
            cursor_id = next(self._next_id)
            self._cursors[cursor_id] = _Entry(cursor_id, namespace, list(remaining), self._time())
            return cursor_id

    def next_batch(self, cursor_id: int, batch_size: int) -> tuple[list[dict[str, Any]], bool]:
        with self._lock:
            self._prune_locked()
            entry = self._cursors.get(cursor_id)
            if entry is None:
                raise CursorNotFound(cursor_id)
            if batch_size <= 0:
                batch_size = len(entry.remaining)
            batch = entry.remaining[:batch_size]
            entry.remaining = entry.remaining[batch_size:]
            exhausted = not entry.remaining
            if exhausted:
                del self._cursors[cursor_id]
            else:
                entry.last_access = self._time()
            return batch, exhausted

    def kill(self, cursor_ids: list[int]) -> tuple[list[int], list[int]]:
        killed: list[int] = []
        not_found: list[int] = []
        with self._lock:
            self._prune_locked()
            for cid in cursor_ids:
                if self._cursors.pop(cid, None) is not None:
                    killed.append(cid)
                else:
                    not_found.append(cid)
        return killed, not_found

    def __len__(self) -> int:
        with self._lock:
            self._prune_locked()
            return len(self._cursors)
