from __future__ import annotations

import secrets
import threading
import time
import uuid as _uuid
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

DEFAULT_IDLE_TTL_SECONDS = 600.0  # matches MongoDB's 10-minute cursor TTL.
TAILABLE_IDLE_TTL_SECONDS = 1800.0  # 30 min — change-stream clients legitimately idle.

# Hard cap on simultaneous live cursors. Without this, a malicious or
# buggy client can open cursors with `no_cursor_timeout=True` (or a low
# request rate that stays under the prune cadence) and accumulate
# unbounded `remaining` doc lists, OOMing the server.
DEFAULT_MAX_CURSORS = 10_000


class CursorLimitExceeded(Exception):
    """Raised when register() would push the registry over its cap."""

    def __init__(self, limit: int) -> None:
        super().__init__(
            f"cursor registry full ({limit} cursors live); kill an existing "
            f"cursor or increase max_cursors before opening a new one"
        )
        self.limit = limit


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
    # Latest change-stream resume token observed by the producer.
    # Returned to the client as ``cursor.postBatchResumeToken`` on
    # every getMore so consumers on quiet collections can advance
    # their resume position even when no events are visible. None
    # for non-change-stream cursors.
    last_token: dict[str, str] | None = None


class CursorRegistry:
    def __init__(
        self,
        idle_ttl_seconds: float = DEFAULT_IDLE_TTL_SECONDS,
        time_func: Callable[[], float] | None = None,
        tailable_idle_ttl_seconds: float = TAILABLE_IDLE_TTL_SECONDS,
        max_cursors: int = DEFAULT_MAX_CURSORS,
    ) -> None:
        self._cursors: dict[int, _Entry] = {}
        self._lock = threading.Lock()
        self.idle_ttl_seconds = idle_ttl_seconds
        self.tailable_idle_ttl_seconds = tailable_idle_ttl_seconds
        self.max_cursors = max_cursors
        self._time = time_func or time.monotonic
        # Last successful prune timestamp; -inf means never. Used to amortise
        # the O(N) prune walk across operations rather than running it on
        # every register / get / next_batch / kill / __len__ call.
        self._last_prune: float = float("-inf")

    def _prune_interval(self) -> float:
        # Cap the prune cadence at 60s and at one-tenth of the (smaller) TTL,
        # so a cursor can't outlive its TTL by more than ~10%.
        ttl = self.idle_ttl_seconds
        if ttl <= 0:
            return float("inf")
        return min(60.0, ttl / 10.0)

    def _prune_locked(self) -> None:
        now = self._time()
        if now - self._last_prune < self._prune_interval():
            return
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
        self._last_prune = now

    def register(self, namespace: str, remaining: list[dict[str, Any]]) -> int:
        with self._lock:
            self._prune_locked()
            if len(self._cursors) >= self.max_cursors:
                raise CursorLimitExceeded(self.max_cursors)
            # Cursor IDs were `itertools.count(1)` — sequential, predictable,
            # and trivially enumerable by another connection issuing
            # `getMore` against guessed IDs. Mint with `secrets.randbits`
            # like the tailable path so cross-session cursor hijacking
            # requires brute-forcing a 63-bit space.
            for _attempt in range(8):
                candidate = secrets.randbits(63) | 1  # avoid 0
                if candidate not in self._cursors:
                    cursor_id = candidate
                    break
            else:
                raise RuntimeError("could not mint unique cursor id")
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
        initial_remaining: list[dict[str, Any]] | None = None,
    ) -> int:
        """Register a tailable cursor backed by a producer closure.

        ``initial_remaining`` lets the caller pre-seed the cursor's
        ``remaining`` queue with docs already matched at ``find`` time
        that didn't fit in ``firstBatch``. Subsequent ``getMore``s drain
        that queue first, then fall through to ``producer()`` for
        newly-inserted docs.

        Cursor IDs are int64-random (above 2**32) to match what real
        ``mongod`` issues for change streams; some drivers compare these
        as signed 64-bit and break on small sequential ints.
        """
        with self._lock:
            self._prune_locked()
            if len(self._cursors) >= self.max_cursors:
                raise CursorLimitExceeded(self.max_cursors)
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
                remaining=list(initial_remaining) if initial_remaining else [],
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

    def snapshot(self) -> list[dict[str, Any]]:
        """Return a list of plain dicts describing the live cursors.

        Used by the ``currentOp`` command and the admin UI's cursors page.
        Each entry has ``cursor_id``, ``namespace``, ``remaining``,
        ``last_access``, ``tailable``, and ``await_data``. We don't return
        ``_Entry`` instances so callers can't accidentally mutate registry
        state.
        """
        with self._lock:
            self._prune_locked()
            return [
                {
                    "cursor_id": e.cursor_id,
                    "namespace": e.namespace,
                    "remaining": len(e.remaining),
                    "last_access": e.last_access,
                    "tailable": e.tailable,
                    "await_data": e.await_data,
                }
                for e in sorted(self._cursors.values(), key=lambda x: x.cursor_id)
            ]


__all__ = [
    "CursorLimitExceeded",
    "CursorNotFound",
    "CursorRegistry",
    "_Entry",
    "DEFAULT_IDLE_TTL_SECONDS",
    "DEFAULT_MAX_CURSORS",
    "TAILABLE_IDLE_TTL_SECONDS",
]
