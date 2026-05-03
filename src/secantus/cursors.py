from __future__ import annotations

import itertools
import secrets
import threading
import time
import uuid as _uuid
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

DEFAULT_IDLE_TTL_SECONDS = 600.0  # matches MongoDB's 10-minute cursor TTL.
TAILABLE_IDLE_TTL_SECONDS = 1800.0  # 30 min — change-stream clients legitimately idle.


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
    tailable: bool = False
    await_data: bool = False
    no_cursor_timeout: bool = False
    producer: Callable[[], list[dict[str, Any]]] | None = None
    position_seq: int = 0
    collection_uuid: _uuid.UUID | None = None
    invalidated: bool = False
    final_event_pending: bool = False


class CursorRegistry:
    def __init__(
        self,
        idle_ttl_seconds: float = DEFAULT_IDLE_TTL_SECONDS,
        time_func: Callable[[], float] | None = None,
        tailable_idle_ttl_seconds: float = TAILABLE_IDLE_TTL_SECONDS,
    ) -> None:
        self._cursors: dict[int, _Entry] = {}
        self._lock = threading.Lock()
        self._next_id = itertools.count(1)
        self.idle_ttl_seconds = idle_ttl_seconds
        self.tailable_idle_ttl_seconds = tailable_idle_ttl_seconds
        self._time = time_func or time.monotonic

    def _prune_locked(self) -> None:
        now = self._time()
        expired: list[int] = []
        for cid, e in self._cursors.items():
            if e.no_cursor_timeout:
                continue
            ttl = self.tailable_idle_ttl_seconds if e.tailable else self.idle_ttl_seconds
            if ttl <= 0:
                continue
            if e.last_access < now - ttl:
                expired.append(cid)
        for cid in expired:
            del self._cursors[cid]

    def register(self, namespace: str, remaining: list[dict[str, Any]]) -> int:
        with self._lock:
            self._prune_locked()
            cursor_id = next(self._next_id)
            self._cursors[cursor_id] = _Entry(cursor_id, namespace, list(remaining), self._time())
            return cursor_id

    def register_tailable(
        self,
        namespace: str,
        producer: Callable[[], list[dict[str, Any]]],
        *,
        await_data: bool = True,
        no_cursor_timeout: bool = False,
        position_seq: int = 0,
        collection_uuid: _uuid.UUID | None = None,
    ) -> int:
        """Register a tailable cursor backed by a producer closure.

        Cursor IDs are int64-random (above 2**32) to match what real
        ``mongod`` issues for change streams; some drivers compare these
        as signed 64-bit and break on small sequential ints.
        """
        with self._lock:
            self._prune_locked()
            for _attempt in range(8):
                candidate = secrets.randbits(63) | (1 << 32)  # ensure > 2**32
                if candidate not in self._cursors:
                    cursor_id = candidate
                    break
            else:
                raise RuntimeError("could not mint unique tailable cursor id")
            self._cursors[cursor_id] = _Entry(
                cursor_id=cursor_id,
                namespace=namespace,
                remaining=[],
                last_access=self._time(),
                tailable=True,
                await_data=await_data,
                no_cursor_timeout=no_cursor_timeout,
                producer=producer,
                position_seq=position_seq,
                collection_uuid=collection_uuid,
            )
            return cursor_id

    def get(self, cursor_id: int) -> _Entry:
        with self._lock:
            self._prune_locked()
            entry = self._cursors.get(cursor_id)
            if entry is None:
                raise CursorNotFound(cursor_id)
            entry.last_access = self._time()
            return entry

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
            if entry.tailable:
                # Tailable cursors persist across empty batches.
                entry.last_access = self._time()
                return batch, False
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


__all__ = [
    "CursorNotFound",
    "CursorRegistry",
    "_Entry",
    "DEFAULT_IDLE_TTL_SECONDS",
    "TAILABLE_IDLE_TTL_SECONDS",
]
