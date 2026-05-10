"""Logical session registry.

MongoDB drivers send an ``lsid`` (logical session id) on every
command they issue, and explicitly bracket session lifecycles with
``startSession`` / ``endSessions`` / ``refreshSessions``. The id is
a UUID wrapped in a BSON ``Binary`` (subtype 4) under the key
``{id: BinData(4, <uuid>)}``.

Real ``mongod`` uses sessions to:

* correlate retryable writes (``txnNumber`` is per-session)
* group statements into a transaction
* attach cursors to a session so the cursor is killed when the
  session ends

SecantusDB doesn't yet implement any of those, but it *does* now
track session lifetime so:

* ``startSession`` actually registers a fresh id (drivers can
  observe its presence in subsequent ``rolesInfo`` / ``currentOp``
  / future cursor-affinity surfaces).
* ``endSessions`` drops the listed sessions from the registry.
* ``refreshSessions`` extends the idle TTL on listed sessions.
* Sessions register implicitly the first time a command carries an
  ``lsid``, matching mongod's behaviour for drivers that don't
  call ``startSession`` explicitly.
* Idle sessions older than the TTL (default 30 min, matching
  ``logicalSessionTimeoutMinutes``) are pruned opportunistically.

The registry is in-memory only — sessions don't survive a server
restart, exactly as in real mongod.
"""

from __future__ import annotations

import threading
import time
from collections.abc import Callable

# Sessions older than this are pruned. 30 minutes matches mongod's
# default ``logicalSessionTimeoutMinutes`` advertised in ``hello``.
DEFAULT_IDLE_TTL_SECONDS = 30 * 60


class SessionRegistry:
    """Thread-safe map of ``lsid_bytes`` (16-byte UUID) → ``last_access``.

    Keys are the raw 16-byte UUID payload of the BSON ``Binary``
    subtype 4 — not the wrapping ``Binary`` object — so callers
    don't accidentally rely on identity equality between separately
    decoded copies.
    """

    def __init__(
        self,
        *,
        idle_ttl_seconds: float = DEFAULT_IDLE_TTL_SECONDS,
        time_func: Callable[[], float] | None = None,
    ) -> None:
        self._sessions: dict[bytes, float] = {}
        self._lock = threading.Lock()
        self._time = time_func if callable(time_func) else time.time
        self._idle_ttl = idle_ttl_seconds

    def register(self, lsid_bytes: bytes) -> None:
        """Insert (or refresh) a session and prune idle ones."""
        if not isinstance(lsid_bytes, bytes) or len(lsid_bytes) != 16:
            raise ValueError("lsid must be a 16-byte UUID payload")
        with self._lock:
            self._sessions[lsid_bytes] = self._time()
            self._prune_locked()

    def refresh(self, lsid_bytes: bytes) -> None:
        """Bump the last-access timestamp on an existing session.

        If the session isn't known, registers it (mongod's
        ``refreshSessions`` is implicit-create). Same semantics as
        :meth:`register`.
        """
        self.register(lsid_bytes)

    def unregister(self, lsid_bytes: bytes) -> None:
        """Drop a session from the registry. No-op if absent."""
        with self._lock:
            self._sessions.pop(lsid_bytes, None)

    def clear(self) -> None:
        """Drop every session.

        Used by the ``killAllSessions`` admin command; driver test
        suites call it between tests to guarantee a clean session
        state before the next subtest's setup.
        """
        with self._lock:
            self._sessions.clear()

    def is_known(self, lsid_bytes: bytes) -> bool:
        with self._lock:
            return lsid_bytes in self._sessions

    def prune_idle(self) -> int:
        """Drop sessions whose last access is older than ``idle_ttl_seconds``.

        Returns the number of sessions removed. Called opportunistically
        from :meth:`register`; callers can also drive it directly (e.g.
        from a background sweeper).
        """
        with self._lock:
            return self._prune_locked()

    def _prune_locked(self) -> int:
        cutoff = self._time() - self._idle_ttl
        stale = [k for k, t in self._sessions.items() if t < cutoff]
        for k in stale:
            del self._sessions[k]
        return len(stale)

    def __len__(self) -> int:
        with self._lock:
            return len(self._sessions)


__all__ = ["DEFAULT_IDLE_TTL_SECONDS", "SessionRegistry"]
