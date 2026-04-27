from __future__ import annotations

import itertools
import threading
from dataclasses import dataclass
from typing import Any


class CursorNotFound(Exception):
    def __init__(self, cursor_id: int) -> None:
        super().__init__(f"cursor id {cursor_id} not found")
        self.cursor_id = cursor_id


@dataclass
class _Entry:
    cursor_id: int
    namespace: str
    remaining: list[dict[str, Any]]


class CursorRegistry:
    def __init__(self) -> None:
        self._cursors: dict[int, _Entry] = {}
        self._lock = threading.Lock()
        self._next_id = itertools.count(1)

    def register(self, namespace: str, remaining: list[dict[str, Any]]) -> int:
        with self._lock:
            cursor_id = next(self._next_id)
            self._cursors[cursor_id] = _Entry(cursor_id, namespace, list(remaining))
            return cursor_id

    def next_batch(self, cursor_id: int, batch_size: int) -> tuple[list[dict[str, Any]], bool]:
        with self._lock:
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
            return batch, exhausted

    def kill(self, cursor_ids: list[int]) -> tuple[list[int], list[int]]:
        killed: list[int] = []
        not_found: list[int] = []
        with self._lock:
            for cid in cursor_ids:
                if self._cursors.pop(cid, None) is not None:
                    killed.append(cid)
                else:
                    not_found.append(cid)
        return killed, not_found

    def __len__(self) -> int:
        with self._lock:
            return len(self._cursors)
