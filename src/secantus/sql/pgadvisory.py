"""Server-wide advisory-lock hub (the ``pg_advisory_lock`` family, #135).

Before this hub, advisory locks were per-``Session`` bookkeeping that always
granted — two connections could both "hold" the same exclusive lock, so
leader-election / migration-fencing patterns (alembic, cron fencing) got no
mutual exclusion. The hub is the server-wide authority, shared across every
connection by the wire server (the ``NotifyHub`` pattern); the per-session
bookkeeping stays as the ``pg_locks`` reflection + unlock-truthfulness layer
and is kept in sync by ``Session``'s advisory methods.

Semantics mirror real PostgreSQL:

* exclusive is grantable when no OTHER owner holds the key in any mode;
  shared is grantable when no OTHER owner holds it exclusively — both
  re-entrant per owner, with session- and transaction-level lifetimes.
* a blocking acquire waits on a condition; every ``deadlock_timeout``-ish
  second the waiter re-runs a wait-for-graph cycle check and aborts with
  PostgreSQL's ``40P01 deadlock detected`` when it is part of a cycle.
* transaction-level holds release at COMMIT/ROLLBACK, session-level holds at
  ``pg_advisory_unlock[_all]``, and everything an owner holds releases when
  its connection ends (``release_all``, wired into the server's teardown).
"""

from __future__ import annotations

import threading
from collections import defaultdict
from typing import Any

from secantus.sql import errors

# How long a blocked acquire waits between deadlock checks — the analogue of
# PostgreSQL's ``deadlock_timeout`` (default 1s there too).
_DEADLOCK_CHECK_SECONDS = 1.0

_MODES = ("excl", "shared")


class AdvisoryLockHub:
    """Thread-safe server-wide advisory-lock table."""

    def __init__(self) -> None:
        self._cv = threading.Condition()
        # (key3, mode, xact) -> {owner_id: count}. Split by xact-ness so the
        # transaction-end release drops exactly the transaction-level holds.
        self._holds: dict[tuple[tuple[int, int, int], str, bool], dict[int, int]] = defaultdict(
            dict
        )
        # owner_id -> (key3, wants_shared) while blocked in acquire() — the
        # wait-for graph's edges for deadlock detection.
        self._waiting: dict[int, tuple[tuple[int, int, int], bool]] = {}

    @staticmethod
    def _owner(session: Any) -> int:
        return id(session)

    # -- grant logic (caller holds self._cv) --------------------------------

    def _holders(self, key: tuple[int, int, int], mode: str) -> set[int]:
        out: set[int] = set()
        for xact in (False, True):
            out.update(o for o, n in self._holds.get((key, mode, xact), {}).items() if n > 0)
        return out

    def _grantable(self, key: tuple[int, int, int], owner: int, *, shared: bool) -> bool:
        excl_holders = self._holders(key, "excl") - {owner}
        if excl_holders:
            return False
        if shared:
            return True
        return not (self._holders(key, "shared") - {owner})

    def _blockers(self, key: tuple[int, int, int], owner: int, *, shared: bool) -> set[int]:
        """The owners currently preventing ``owner`` from taking ``key``."""
        blockers = self._holders(key, "excl") - {owner}
        if not shared:
            blockers |= self._holders(key, "shared") - {owner}
        return blockers

    def _in_deadlock(self, me: int) -> bool:
        """Wait-for-graph cycle check from ``me`` (caller holds ``_cv``;
        ``me`` is registered in ``_waiting``). Edges: a waiter → every holder
        blocking its requested key. ``me`` is deadlocked when a path of
        waiters leads back to it."""
        waited = self._waiting.get(me)
        if waited is None:
            return False
        key, shared = waited
        stack = list(self._blockers(key, me, shared=shared))
        seen: set[int] = set()
        while stack:
            owner = stack.pop()
            if owner == me:
                return True
            if owner in seen:
                continue
            seen.add(owner)
            w = self._waiting.get(owner)
            if w is not None:
                stack.extend(self._blockers(w[0], owner, shared=w[1]))
        return False

    # -- public API ---------------------------------------------------------

    def acquire(
        self,
        session: Any,
        key: tuple[int, int, int],
        *,
        shared: bool,
        xact: bool,
        blocking: bool,
    ) -> bool:
        """Take (or re-enter) the lock. Non-blocking: returns whether granted.
        Blocking: waits until granted — raising PostgreSQL's ``40P01`` when the
        wait-for graph shows this waiter in a cycle."""
        owner = self._owner(session)
        mode = "shared" if shared else "excl"
        with self._cv:
            while not self._grantable(key, owner, shared=shared):
                if not blocking:
                    return False
                self._waiting[owner] = (key, shared)
                try:
                    if self._in_deadlock(owner):
                        raise errors.SQLError("40P01", "deadlock detected")
                    self._cv.wait(_DEADLOCK_CHECK_SECONDS)
                finally:
                    self._waiting.pop(owner, None)
            holds = self._holds[(key, mode, xact)]
            holds[owner] = holds.get(owner, 0) + 1
            return True

    def release(self, session: Any, key: tuple[int, int, int], *, shared: bool) -> bool:
        """Release one level of a *session-level* hold. Returns whether one was
        held (mirroring ``pg_advisory_unlock``'s boolean)."""
        owner = self._owner(session)
        mode = "shared" if shared else "excl"
        with self._cv:
            holds = self._holds.get((key, mode, False), {})
            n = holds.get(owner, 0)
            if n <= 0:
                return False
            if n == 1:
                del holds[owner]
            else:
                holds[owner] = n - 1
            self._cv.notify_all()
            return True

    def release_session_level(self, session: Any) -> None:
        """``pg_advisory_unlock_all`` — drop every session-level hold."""
        self._release_matching(session, lambda xact: not xact)

    def release_xact(self, session: Any) -> None:
        """Transaction end — drop every transaction-level hold."""
        self._release_matching(session, lambda xact: xact)

    def release_all(self, session: Any) -> None:
        """Connection teardown — drop everything this owner holds (PostgreSQL
        releases all advisory locks at session end)."""
        self._release_matching(session, lambda _xact: True)

    def _release_matching(self, session: Any, want: Any) -> None:
        owner = self._owner(session)
        with self._cv:
            dropped = False
            for (key, mode, xact), holds in list(self._holds.items()):
                if want(xact) and holds.pop(owner, None):
                    dropped = True
                    if not holds:
                        del self._holds[(key, mode, xact)]
            if dropped:
                self._cv.notify_all()
