"""Server-side metrics sampler for the admin dashboard.

A daemon thread polls the target SecantusDB's ``serverStatus`` once per
second, computes a per-tick delta from the prior snapshot, and pushes
each tick into a bounded ring + every subscriber's asyncio queue.

Subscribers are WebSocket connections. Each one registers via
:meth:`Hub.subscribe` and receives an asyncio queue that yields:

* an initial backlog frame (the recent samples in the ring), then
* one frame per subsequent tick.

The thread bridges sync ``MongoFacade.server_status()`` (pymongo, blocking)
to the asyncio event loop via ``loop.call_soon_threadsafe``. The hub itself
is async-only — its public surface assumes the caller holds the FastAPI
event loop reference.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import threading
import time
from collections import deque
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

logger = logging.getLogger(__name__)

DEFAULT_INTERVAL_SECONDS = 1.0
DEFAULT_HISTORY_SIZE = 300


# ---------------------------------------------------------------------------
# Pure helpers
# ---------------------------------------------------------------------------


_OPCOUNTER_FIELDS = ("insert", "query", "update", "delete", "getmore", "command")


def compute_delta(prev: dict[str, Any] | None, current: dict[str, Any]) -> dict[str, int]:
    """Return per-bucket deltas between two ``serverStatus`` snapshots.

    Returns zeros when ``prev`` is ``None`` (the first tick has nothing
    to compare against). Negative deltas can never happen with a correctly
    monotonic counter — but a reset (server restart) would manifest as
    ``current < prev``; we floor the delta at zero so the dashboard
    doesn't render a sudden negative spike.
    """
    out = {f: 0 for f in _OPCOUNTER_FIELDS}
    out["requests"] = 0
    if prev is None:
        return out
    prev_op = prev.get("opcounters") or {}
    curr_op = current.get("opcounters") or {}
    for f in _OPCOUNTER_FIELDS:
        out[f] = max(0, int(curr_op.get(f, 0) or 0) - int(prev_op.get(f, 0) or 0))
    prev_net = (prev.get("network") or {}).get("numRequests", 0) or 0
    curr_net = (current.get("network") or {}).get("numRequests", 0) or 0
    out["requests"] = max(0, int(curr_net) - int(prev_net))
    return out


def build_sample(ts: float, snapshot: dict[str, Any], delta: dict[str, int]) -> dict[str, Any]:
    """Project a ``serverStatus`` snapshot + delta into the wire frame."""
    conns = snapshot.get("connections") or {}
    opc = snapshot.get("opcounters") or {}
    net = snapshot.get("network") or {}
    return {
        "ts": ts,
        "uptime": int(snapshot.get("uptime", 0) or 0),
        "connections": {
            "current": int(conns.get("current", 0) or 0),
            "totalCreated": int(conns.get("totalCreated", 0) or 0),
        },
        "opcounters": {f: int(opc.get(f, 0) or 0) for f in _OPCOUNTER_FIELDS},
        "delta": dict(delta),
        "requests": int(net.get("numRequests", 0) or 0),
    }


# ---------------------------------------------------------------------------
# Hub: async-side subscriber registry
# ---------------------------------------------------------------------------


@dataclass(eq=False)
class _Subscriber:
    queue: asyncio.Queue


@dataclass
class Hub:
    """Async-side subscriber registry.

    Lives on the event loop. ``subscribe()`` returns a queue that the
    caller drains; ``broadcast()`` is invoked on every tick and pushes
    each sample into every subscriber's queue (dropping the oldest if
    the queue is full — slow clients shouldn't stall the sampler).
    """

    queue_size: int = 32
    _subscribers: set[_Subscriber] = field(default_factory=set)

    def subscribe(self) -> asyncio.Queue:
        sub = _Subscriber(queue=asyncio.Queue(maxsize=self.queue_size))
        self._subscribers.add(sub)
        return sub.queue

    def unsubscribe(self, queue: asyncio.Queue) -> None:
        for sub in list(self._subscribers):
            if sub.queue is queue:
                self._subscribers.discard(sub)
                return

    def broadcast(self, sample: dict[str, Any]) -> None:
        for sub in self._subscribers:
            if sub.queue.full():
                # Slow client. Drop the oldest queued sample to make room
                # so we don't block the producer thread.
                with contextlib.suppress(asyncio.QueueEmpty):
                    sub.queue.get_nowait()
            # Should not happen because we just drained, but stay safe.
            with contextlib.suppress(asyncio.QueueFull):
                sub.queue.put_nowait(sample)


# ---------------------------------------------------------------------------
# Sampler: sync-side polling thread
# ---------------------------------------------------------------------------


class Sampler:
    """Background thread polling ``serverStatus`` once per ``interval_seconds``.

    Constructed in ``create_app`` and started in the FastAPI lifespan;
    ``stop()`` is awaited at shutdown. Designed to be safe to construct
    eagerly even if the target server is unreachable — failed polls log
    at WARNING and skip the tick rather than crashing the thread.
    """

    def __init__(
        self,
        snapshot_fn: Callable[[], dict[str, Any]],
        *,
        hub: Hub,
        loop: asyncio.AbstractEventLoop,
        interval_seconds: float = DEFAULT_INTERVAL_SECONDS,
        history_size: int = DEFAULT_HISTORY_SIZE,
        time_func: Callable[[], float] = time.time,
    ) -> None:
        self._snapshot_fn = snapshot_fn
        self._hub = hub
        self._loop = loop
        self._interval = interval_seconds
        self._history: deque[dict[str, Any]] = deque(maxlen=history_size)
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._lock = threading.Lock()
        self._time = time_func

    def start(self) -> None:
        if self._thread is not None and self._thread.is_alive():
            return
        self._stop.clear()
        self._thread = threading.Thread(
            target=self._run, name="secantus-admin-sampler", daemon=True
        )
        self._thread.start()

    def stop(self, *, timeout: float = 2.0) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=timeout)
            self._thread = None

    def history(self) -> list[dict[str, Any]]:
        with self._lock:
            return list(self._history)

    # Exposed for tests so they can drive a single tick without waiting.
    def tick_once(self) -> dict[str, Any] | None:
        return self._tick()

    # --- internals --------------------------------------------------------

    def _run(self) -> None:
        next_due = self._time()
        while not self._stop.is_set():
            now = self._time()
            if now >= next_due:
                self._tick()
                next_due = now + self._interval
            # Bounded sleep so we react to ``stop()`` quickly.
            self._stop.wait(timeout=min(self._interval, max(0.05, next_due - now)))

    def _tick(self) -> dict[str, Any] | None:
        try:
            snapshot = self._snapshot_fn()
        except Exception as exc:
            logger.warning("admin sampler failed to read serverStatus: %s", exc)
            return None
        with self._lock:
            prev = self._history[-1]["_raw"] if self._history else None
            delta = compute_delta(prev, snapshot)
            sample = build_sample(self._time(), snapshot, delta)
            sample["_raw"] = snapshot  # internal — stripped before broadcast
            self._history.append(sample)
            wire_sample = {k: v for k, v in sample.items() if k != "_raw"}
        # Push to subscribers from the loop thread.
        # Loop is closing; drop silently.
        with contextlib.suppress(RuntimeError):
            self._loop.call_soon_threadsafe(self._hub.broadcast, wire_sample)
        return wire_sample

    # ---- backlog for new subscribers ------------------------------------

    def backlog_frame(self) -> dict[str, Any]:
        """Return a ``backlog`` frame the WS handler ships on connect."""
        with self._lock:
            samples = [{k: v for k, v in s.items() if k != "_raw"} for s in self._history]
        return {"type": "backlog", "samples": samples}


__all__ = [
    "Hub",
    "Sampler",
    "build_sample",
    "compute_delta",
    "DEFAULT_HISTORY_SIZE",
    "DEFAULT_INTERVAL_SECONDS",
]
