"""Per-connection SQL session state.

Carries the connection's database, authenticated user, and GUC settings (the
``SET``/``SHOW`` parameters), plus the advertised version strings. Threaded
through ``run_sql`` so session functions (``current_database()``,
``current_setting(...)``, ...) and ``SHOW``/``SET`` resolve against real state.
"""

from __future__ import annotations

import threading
from collections import deque
from dataclasses import dataclass, field
from typing import Any

# Short form for the ``server_version`` ParameterStatus (libpq parses the
# leading number to gate features); the long banner is what ``version()``
# returns.
SERVER_VERSION = "15.0 (SecantusDB)"
VERSION_STRING = "PostgreSQL 15.0 (SecantusDB) on x86_64-pc-linux-gnu, compiled by python"

# Default GUC values reported by SHOW / current_setting when the client hasn't
# SET them. Enough to satisfy the common introspection probes.
GUC_DEFAULTS: dict[str, str] = {
    "server_version": SERVER_VERSION,
    "server_encoding": "UTF8",
    "client_encoding": "UTF8",
    "DateStyle": "ISO, MDY",
    "IntervalStyle": "postgres",
    "TimeZone": "UTC",
    "integer_datetimes": "on",
    "standard_conforming_strings": "on",
    "search_path": '"$user", public',
    "application_name": "",
    "is_superuser": "off",
}

# GUCs the server echoes back via a ParameterStatus message when SET (the
# protocol's GUC_REPORT set). Clients track these for behaviour decisions.
REPORTABLE_GUCS = frozenset(
    {
        "client_encoding",
        "DateStyle",
        "TimeZone",
        "application_name",
        "standard_conforming_strings",
        "search_path",
    }
)


@dataclass
class _Savepoint:
    """One open savepoint. ``snapshots`` maps a collection name to the deep-copied
    documents it held at this savepoint's establishment (captured lazily, on the
    first write to that collection while this savepoint is open)."""

    name: str
    snapshots: dict[str, list] = field(default_factory=dict)


@dataclass
class _Cursor:
    """A server-side cursor (``DECLARE … CURSOR``). The query is materialized once
    at declaration; ``pos`` is the index of the row the cursor is currently on
    (-1 = before the first row, len(rows) = after the last), which ``FETCH`` /
    ``MOVE`` advance. ``hold`` cursors survive COMMIT (``WITH HOLD``)."""

    name: str
    columns: list = field(default_factory=list)
    rows: list = field(default_factory=list)
    pos: int = -1
    hold: bool = False


@dataclass
class Session:
    database: str = "postgres"
    user: str = "secantus"
    backend_pid: int = 0
    settings: dict[str, str] = field(default_factory=dict)
    # RBAC (#193). ``authz_active`` gates per-statement authorization — the wire
    # server sets it when started with ``require_auth`` *and* explicit per-user
    # role bindings. When false (embedded ``run_sql``, or trust mode) the SQL
    # surface is unrestricted, preserving prior behaviour. ``roles`` is the
    # authenticated user's bindings (``[{"role": ..., "db": ...}]``), reused by
    # ``secantus.rbac.check_privilege`` — the same model the Mongo server uses.
    authz_active: bool = False
    roles: list[Any] = field(default_factory=list)
    # Multi-statement transaction state. ``txn_handle`` is the open
    # ``Storage`` user-transaction (None outside a BEGIN block); ``txn_failed``
    # marks an aborted block (every command except COMMIT/ROLLBACK errors with
    # 25P02 until the block ends — Postgres semantics).
    txn_handle: Any = None
    txn_failed: bool = False
    # Open savepoints, innermost last. Each is a ``_Savepoint`` carrying the
    # pre-image snapshot (collection -> docs) captured on the first write to that
    # collection after the savepoint was established, so ROLLBACK TO can restore.
    savepoints: list[Any] = field(default_factory=list)
    # Open server-side cursors by name (``DECLARE … CURSOR`` / ``FETCH`` / ``CLOSE``).
    cursors: dict[str, Any] = field(default_factory=dict)
    # SQL-level prepared statements (``PREPARE name AS …`` / ``EXECUTE`` /
    # ``DEALLOCATE``). Maps a statement name to ``(query_ast, param_count)``. These
    # are the SQL command-level prepared statements — distinct from the extended
    # wire protocol's Parse/Bind portals (``pgextended.py``), which don't touch this.
    prepared: dict[str, Any] = field(default_factory=dict)
    # Deferred-constraint state (DEFERRABLE FK / UNIQUE). ``pending_deferred`` holds
    # re-check records collected while a deferred constraint would be violated
    # inside a transaction; they run at COMMIT (or SET CONSTRAINTS … IMMEDIATE).
    # ``deferred_all`` is the SET CONSTRAINTS ALL override (None = per-constraint
    # default); ``deferred_names`` overrides individual constraints by name.
    pending_deferred: list[Any] = field(default_factory=list)
    deferred_all: bool | None = None
    deferred_names: dict[str, bool] = field(default_factory=dict)
    # Per-session sequence values for currval / lastval. ``seq_values`` maps a
    # sequence name to the last value ``nextval`` returned *in this session*;
    # ``last_seq_value`` is the most recent one (``lastval()``, sequence-agnostic).
    seq_values: dict[str, int] = field(default_factory=dict)
    last_seq_value: int | None = None
    # LISTEN / NOTIFY. ``notify_hub`` is the server-wide channel registry (None for
    # the embedded API, where NOTIFY is a no-op delivery). ``_notify_deliveries``
    # is this connection's inbound queue of ``(pid, channel, payload)`` tuples,
    # drained by the owning connection thread. ``pending_notifies`` buffers NOTIFYs
    # issued inside an open transaction block (delivered at COMMIT).
    notify_hub: Any = None
    _notify_lock: Any = field(default_factory=threading.Lock)
    _notify_deliveries: deque = field(default_factory=deque)
    pending_notifies: list[tuple[str, str]] = field(default_factory=list)

    def enqueue_notification(self, pid: int, channel: str, payload: str) -> None:
        """Called by another connection's NOTIFY thread — thread-safe append."""
        with self._notify_lock:
            self._notify_deliveries.append((pid, channel, payload))

    def drain_notifications(self) -> list[tuple[int, str, str]]:
        """Pop all queued inbound notifications for this connection to deliver."""
        with self._notify_lock:
            out = list(self._notify_deliveries)
            self._notify_deliveries.clear()
        return out

    def has_pending_notifications(self) -> bool:
        with self._notify_lock:
            return bool(self._notify_deliveries)

    def record_sequence_value(self, name: str, value: int) -> None:
        """Remember a ``nextval`` result for later ``currval(name)`` / ``lastval()``."""
        self.seq_values[name] = value
        self.last_seq_value = value

    def currval(self, name: str) -> int:
        """The last value ``nextval(name)`` returned in this session (error 55000 if
        ``nextval`` hasn't been called for ``name`` yet)."""
        if name not in self.seq_values:
            from secantus.sql import errors

            raise errors.SQLError(
                "55000", f'currval of sequence "{name}" is not yet defined in this session'
            )
        return self.seq_values[name]

    def lastval(self) -> int:
        if self.last_seq_value is None:
            from secantus.sql import errors

            raise errors.SQLError("55000", "lastval is not yet defined in this session")
        return self.last_seq_value

    def constraint_is_deferred(self, name: str, initially_deferred: bool) -> bool:
        """Whether constraint ``name`` is currently deferred, honouring SET
        CONSTRAINTS overrides over the constraint's INITIALLY DEFERRED default."""
        if name in self.deferred_names:
            return self.deferred_names[name]
        if self.deferred_all is not None:
            return self.deferred_all
        return initially_deferred

    def reset_deferred(self) -> None:
        """Clear all deferred-constraint state (at end of transaction)."""
        self.pending_deferred = []
        self.deferred_all = None
        self.deferred_names = {}

    def get_setting(self, name: str) -> str:
        return self.settings.get(name, GUC_DEFAULTS.get(name, ""))

    def txn_status(self) -> bytes:
        """The ReadyForQuery status byte: idle / in-transaction / failed."""
        if self.txn_handle is None:
            return b"I"
        return b"E" if self.txn_failed else b"T"

    @property
    def current_schema(self) -> str:
        first = self.get_setting("search_path").split(",")[0].strip().strip('"')
        # "$user" resolves to the user's schema, which we collapse to public.
        if first in ("$user", "", "$user"):
            return "public"
        return first
