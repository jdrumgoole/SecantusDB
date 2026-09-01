"""Cooperative ``maxTimeMS`` enforcement.

``maxTimeMS`` was parsed and validated exactly as mongod validates it, and then
ignored: the operation ran to completion and answered ``ok``. mongod aborts it
and answers ``50 MaxTimeMSExpired``. That is invisible on a fast operation and
is exactly what a differential probe trips over once the budget is small enough
for the work to actually exceed it.

A real deadline cannot be a parse-time check, because the thing being bounded is
elapsed time inside the handler. It is threaded here as a THREAD-LOCAL, armed
by ``dispatch`` around the handler call and polled from the loops that can run
long -- the storage scan, the aggregation pipeline, the index build. A
thread-local rather than a parameter because the alternative is a deadline
argument on every function between ``dispatch`` and a document loop, most of
which have nothing to do with time; one server thread handles one command at a
time, which is what makes the thread-local exact.

Cooperative means the granularity is one poll: a single ``matches()`` call is
never interrupted, so an operation can overrun by the cost of one document.
mongod's own enforcement is interrupt-point-based and has the same property.
"""

from __future__ import annotations

import threading
import time as _time

__all__ = ["MaxTimeMSExpired", "arm", "check", "expired", "remaining_ms"]


class MaxTimeMSExpired(Exception):
    """The operation outlived its ``maxTimeMS`` budget.

    Carries mongod's message verbatim; ``dispatch`` turns it into
    ``{ok: 0, code: 50, codeName: "MaxTimeMSExpired"}``.
    """

    code = 50
    code_name = "MaxTimeMSExpired"

    def __init__(self, message: str = "operation exceeded time limit") -> None:
        super().__init__(message)


_state = threading.local()

#: How often a polling loop actually reads the clock. ``perf_counter`` is cheap
#: but not free, and a scan calls ``check()`` once per document -- at a million
#: documents that is a million clock reads for a budget measured in
#: milliseconds. Polling every Nth call keeps the overhead off the hot path
#: while still bounding the overrun to N documents.
POLL_EVERY = 64


class _Armed:
    __slots__ = ("deadline", "ticks")

    def __init__(self, deadline: float) -> None:
        self.deadline = deadline
        self.ticks = 0


class arm:
    """Context manager arming this thread's deadline for ``max_time_ms``.

    ``max_time_ms`` of 0 or None means "no limit" (mongod's own encoding of an
    absent budget), and arms nothing -- so the polling loops keep their fast
    path when the caller did not ask for a timeout.

    Nesting restores the outer budget on exit, so a handler that dispatches an
    inner command cannot silently widen its own limit.
    """

    __slots__ = ("_armed", "_deadline", "_previous")

    def __init__(self, max_time_ms: int | float | None) -> None:
        self._armed = bool(max_time_ms) and max_time_ms > 0
        self._previous: _Armed | None = None
        if self._armed:
            self._deadline = _time.perf_counter() + float(max_time_ms) / 1000.0

    def __enter__(self) -> arm:
        self._previous = getattr(_state, "current", None)
        if self._armed:
            _state.current = _Armed(self._deadline)
        return self

    def __exit__(self, *_exc: object) -> None:
        _state.current = self._previous


def expired() -> bool:
    """Has this thread's deadline passed? False when nothing is armed."""
    current: _Armed | None = getattr(_state, "current", None)
    return current is not None and _time.perf_counter() >= current.deadline


def remaining_ms() -> float | None:
    """Milliseconds left on this thread's budget, or None if unarmed."""
    current: _Armed | None = getattr(_state, "current", None)
    if current is None:
        return None
    return max(0.0, (current.deadline - _time.perf_counter()) * 1000.0)


def check() -> None:
    """Raise :class:`MaxTimeMSExpired` if the budget is spent.

    Call it from any loop whose length is driven by the size of the data. Cheap
    enough for a per-document call: it reads the clock once every
    :data:`POLL_EVERY` invocations, and returns on an attribute lookup when no
    deadline is armed at all.
    """
    current: _Armed | None = getattr(_state, "current", None)
    if current is None:
        return
    current.ticks += 1
    if current.ticks % POLL_EVERY:
        return
    if _time.perf_counter() >= current.deadline:
        raise MaxTimeMSExpired()


def check_now() -> None:
    """:func:`check` without the poll interval -- for coarse call sites.

    Use it between pipeline STAGES or once per batch, where the call happens a
    handful of times and skipping 63 of every 64 would mean skipping the check
    entirely.
    """
    if expired():
        raise MaxTimeMSExpired()
