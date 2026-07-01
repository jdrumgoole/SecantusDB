"""Per-connection SQL session state.

Carries the connection's database, authenticated user, and GUC settings (the
``SET``/``SHOW`` parameters), plus the advertised version strings. Threaded
through ``run_sql`` so session functions (``current_database()``,
``current_setting(...)``, ...) and ``SHOW``/``SET`` resolve against real state.
"""

from __future__ import annotations

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
