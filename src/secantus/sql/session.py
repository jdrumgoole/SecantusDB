"""Per-connection SQL session state.

Carries the connection's database, authenticated user, and GUC settings (the
``SET``/``SHOW`` parameters), plus the advertised version strings. Threaded
through ``run_sql`` so session functions (``current_database()``,
``current_setting(...)``, ...) and ``SHOW``/``SET`` resolve against real state.
"""

from __future__ import annotations

import itertools
import os
import threading
from collections import deque
from dataclasses import dataclass, field
from typing import Any

# Per-session temp-namespace numbers (``pg_temp_<n>`` — real PG mints one
# schema per backend). Seeded from the pid like the wire server's backend_pid
# sequence, so a namespace left behind by a crashed process (temp catalog
# entries persist in storage until their session drops them) is vanishingly
# unlikely to be re-minted and collide with its stale tables.
_TEMP_SCHEMA_SEQ = itertools.count((os.getpid() & 0x7FFFFF) << 8 | 1)

# Short form for the ``server_version`` ParameterStatus (libpq parses the
# leading number to gate features); the long banner is what ``version()``
# returns.
SERVER_VERSION = "15.0 (SecantusDB)"
VERSION_STRING = "PostgreSQL 15.0 (SecantusDB) on x86_64-pc-linux-gnu, compiled by python"

# Default GUC values reported by SHOW / current_setting when the client hasn't
# SET them. Enough to satisfy the common introspection probes.
GUC_DEFAULTS: dict[str, str] = {
    "server_version": SERVER_VERSION,
    # The integer form clients compare against (major*10000 + minor); pgx's
    # protocol-3.2 test reads it to decide the expected secret-key length.
    "server_version_num": "150000",
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
    # Transaction characteristics — single-node, so these are honest constants
    # (SET TRANSACTION is a no-op) but ``current_setting`` must report them.
    # Two-phase commit is supported (#139); a real PG defaults this to 0 but a
    # zero here reads as "2PC disabled" to drivers' capability probes.
    "max_prepared_transactions": "100",
    # 0 = disabled (PG default); a positive value (ms) terminates a connection
    # left idle in a transaction block that long.
    "idle_in_transaction_session_timeout": "0",
    "transaction_isolation": "read committed",
    "transaction_read_only": "off",
    "transaction_deferrable": "off",
    "default_transaction_isolation": "read committed",
    "default_transaction_read_only": "off",
    "default_transaction_deferrable": "off",
    # PG's compile-time INDEX_MAX_KEYS default. pgjdbc reads it once per
    # connection (getMaxIndexKeys) and every FK/primary-key metadata call
    # errors out if the pg_settings row is absent.
    "max_index_keys": "32",
}

# Postgres encoding name (canonicalised: upper, no -_/ separators) -> Python
# codec. ``None`` means pass-through: SQL_ASCII performs no conversion in
# Postgres, so wire bytes ride through unchanged (utf-8 + surrogateescape makes
# arbitrary byte sequences round-trip losslessly through Python str).
_PG_ENCODINGS: dict[str, str | None] = {
    "UTF8": "utf-8",
    "UNICODE": "utf-8",
    "LATIN1": "iso8859-1",
    "ISO88591": "iso8859-1",
    "LATIN2": "iso8859-2",
    "ISO88592": "iso8859-2",
    "LATIN5": "iso8859-9",
    "LATIN9": "iso8859-15",
    "ISO885915": "iso8859-15",
    "WIN1250": "cp1250",
    "WIN1251": "cp1251",
    "WIN1252": "cp1252",
    "SQLASCII": None,
    "EUCJP": "euc_jp",
    "EUCKR": "euc_kr",
    "EUCCN": "gb2312",
    "SJIS": "shift_jis",
    "BIG5": "big5",
    "GBK": "gbk",
    "EUCTW": None,  # no Python codec — pass-through bytes
    "MULEINTERNAL": None,
}

# Canonical PG spelling for the ParameterStatus / SHOW value.
_PG_ENCODING_CANONICAL: dict[str, str] = {
    "UTF8": "UTF8",
    "UNICODE": "UTF8",
    "LATIN1": "LATIN1",
    "ISO88591": "LATIN1",
    "LATIN2": "LATIN2",
    "ISO88592": "LATIN2",
    "LATIN5": "LATIN5",
    "LATIN9": "LATIN9",
    "ISO885915": "LATIN9",
    "WIN1250": "WIN1250",
    "WIN1251": "WIN1251",
    "WIN1252": "WIN1252",
    "SQLASCII": "SQL_ASCII",
    # East-Asian encodings Python can convert.
    "EUCJP": "EUC_JP",
    "EUCKR": "EUC_KR",
    "EUCCN": "EUC_CN",
    "SJIS": "SJIS",
    "SHIFTJIS": "SJIS",
    "BIG5": "BIG5",
    "GBK": "GBK",
    # Real PG encodings Python has NO codec for — accepted (a client's own
    # capability check may still reject them; psycopg raises client-side for
    # EUC_TW) with pass-through bytes like SQL_ASCII.
    "EUCTW": "EUC_TW",
    "MULEINTERNAL": "MULE_INTERNAL",
}


def canonical_client_encoding(name: str) -> str | None:
    """The canonical Postgres spelling for a client_encoding value the user SET
    (``'latin-1'`` -> ``LATIN1``), or None when the encoding isn't supported."""
    key = str(name).upper().replace("-", "").replace("_", "").replace("/", "")
    return _PG_ENCODING_CANONICAL.get(key)


def python_codec_for(pg_name: str) -> str | None:
    """The Python codec for a canonical PG encoding name; None = pass-through."""
    key = str(pg_name).upper().replace("-", "").replace("_", "")
    return _PG_ENCODINGS.get(key, "utf-8")


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
        # PG reports these too (pgtest param_status reads them after SET /
        # SET LOCAL / SET ROLE and on the transaction-end unwind).
        "IntervalStyle",
        "is_superuser",
    }
)

#: GUCs whose reported value PG lowercases (enum GUCs — ``IntervalStyle =
#: 'ISO_8601'`` reports ``iso_8601``).
_LOWERCASE_REPORTED_GUCS = frozenset({"IntervalStyle"})


#: DateStyle output styles, in PG's reported spelling.
_DATESTYLE_STYLES = {"iso": "ISO", "postgres": "Postgres", "sql": "SQL", "german": "German"}
_DATESTYLE_ORDERS = ("MDY", "DMY", "YMD")


def canonical_guc_value(name: str, value: str) -> str:
    """The value PG stores/reports for GUC ``name`` (canonical spelling)."""
    if name in _LOWERCASE_REPORTED_GUCS:
        return str(value).strip().strip("'\"").lower()
    if name == "DateStyle":
        # PG reports DateStyle as ``<style>, <order>`` regardless of the order
        # they were written in (``YMD, ISO`` -> ``ISO, YMD``; pgtest
        # param_status). A single-component SET passes through so the other
        # half keeps its current value, like PG's merge.
        parts = [p.strip() for p in str(value).strip().strip("'\"").split(",") if p.strip()]
        style = next(
            (_DATESTYLE_STYLES[p.lower()] for p in parts if p.lower() in _DATESTYLE_STYLES), None
        )
        order = next((p.upper() for p in parts if p.upper() in _DATESTYLE_ORDERS), None)
        if style is not None and order is not None:
            return f"{style}, {order}"
    return value


# GUC names are case-insensitive; SET / SHOW / ParameterStatus all resolve to
# the canonical spelling (``set timezone`` must hit ``TimeZone``'s default,
# report as ``TimeZone``, and read back for rendering).
_GUC_CANONICAL: dict[str, str] = {k.lower(): k for k in GUC_DEFAULTS}
_GUC_CANONICAL.update({k.lower(): k for k in REPORTABLE_GUCS})
# ``SET TIME ZONE 'x'`` and ``SHOW TIME ZONE`` spell the GUC with a space, and
# that is the spelling JDBC uses to pin a connection's zone. Without this the
# two-word form set nothing and SHOW answered empty, so every client that
# configures its zone that way silently stayed on the default.
_GUC_CANONICAL["time zone"] = "TimeZone"


def canonical_timezone_setting(value: str) -> str:
    """Normalize a TimeZone GUC value the way Postgres reports it: the
    GMT/UTC-prefixed POSIX spellings uppercase their prefix (``gmt-3`` ->
    ``GMT-3``) and bare utc/gmt uppercase entirely. pgjdbc's ParameterStatus
    parser matches ``GMT+``/``GMT-`` case-sensitively — a lowercase echo left
    it assuming UTC, so binary timestamptz getString rendered the wrong zone.
    Region names (``Europe/Paris``) pass through unchanged."""
    v = (value or "").strip().strip("'\"")
    low = v.lower()
    if low in ("utc", "gmt"):
        return low.upper()
    if low.startswith(("gmt+", "gmt-", "utc+", "utc-")):
        return v[:3].upper() + v[3:]
    posix = _numeric_offset_zone(v)
    return posix if posix is not None else v


def _numeric_offset_zone(value: str) -> str | None:
    """PG renders a NUMERIC ``SET TIME ZONE`` offset as a POSIX zone spec:
    ``+6`` -> ``<+06>-06``, ``-11.5`` -> ``<-11:30>+11:30``. The label keeps
    the ISO sign; the offset AFTER it is POSIX-style, i.e. sign-inverted
    (pgtest param_status pins both spellings). None when ``value`` isn't a
    bare number."""
    try:
        hours = float(value)
    except ValueError:
        return None
    if not -16 <= hours <= 16:  # PG's numeric TimeZone range
        return None
    sign = "-" if hours < 0 else "+"
    total_minutes = round(abs(hours) * 60)
    hh, mm = divmod(total_minutes, 60)
    body = f"{hh:02d}" if mm == 0 else f"{hh:02d}:{mm:02d}"
    inverted = "+" if sign == "-" else "-"
    return f"<{sign}{body}>{inverted}{body}"


def canonical_guc_name(name: str) -> str:
    # Collapse internal whitespace so ``TIME  ZONE`` resolves like ``TIME ZONE``.
    return _GUC_CANONICAL.get(" ".join(name.split()).lower(), name.strip())


@dataclass
class _Savepoint:
    """One open savepoint. ``snapshots`` maps a collection name to the deep-copied
    documents it held at this savepoint's establishment (captured lazily, on the
    first write to that collection while this savepoint is open)."""

    name: str
    snapshots: dict[str, list] = field(default_factory=dict)
    #: Every GUC's value when this savepoint was established. ROLLBACK TO
    #: SAVEPOINT reverts GUCs set after it and re-reports the GUC_REPORT ones
    #: (pgtest param_status), exactly like PG's per-subtransaction GUC stack.
    gucs: dict[str, str] = field(default_factory=dict)


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
    # SCROLL / NO SCROLL: True = explicit SCROLL, False = NO SCROLL (backward
    # movement rejected), None = default (backward allowed since rows are
    # materialized). For pg_cursors reflection.
    scrollable: bool | None = None
    # For pg_cursors: the DECLARE's query text and the creation instant.
    statement: str = ""
    created: Any = None


class ActivityRegistry:
    """Server-wide registry of live connection ``Session``s, for
    ``pg_catalog.pg_stat_activity`` (#137). Keyed by ``backend_pid`` (the wire
    server assigns a unique one per connection). Thread-safe."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._sessions: dict[int, Session] = {}

    def register(self, session: Session) -> None:
        with self._lock:
            self._sessions[session.backend_pid] = session

    def unregister(self, session: Session) -> None:
        with self._lock:
            self._sessions.pop(session.backend_pid, None)

    def snapshot(self) -> list[Session]:
        with self._lock:
            return list(self._sessions.values())


@dataclass
class PreparedXact:
    """One prepared (two-phase) transaction (#139). ``handle`` is the open
    ``Storage`` user-transaction detached from its originating session at
    ``PREPARE TRANSACTION`` time — its WT session stays open (holding the
    uncommitted writes and its snapshot) until ``COMMIT PREPARED`` /
    ``ROLLBACK PREPARED`` commits or aborts it, possibly from a different
    connection. ``notifies`` are the ``(channel, payload)`` NOTIFYs buffered in
    the block, delivered only if the prepared xact commits."""

    gid: str
    handle: Any
    owner: str
    database: str
    prepared_at: Any
    notifies: list = field(default_factory=list)


class PreparedXactRegistry:
    """Server-wide registry of prepared two-phase transactions (#139), keyed by
    global transaction id (``gid``). A ``PREPARE TRANSACTION 'gid'`` stashes the
    open ``Storage`` user-transaction handle here (disassociating it from the
    session); ``COMMIT PREPARED`` / ``ROLLBACK PREPARED`` — possibly on another
    connection — look it up by gid and commit / abort. Thread-safe.

    In-memory only: prepared transactions do NOT survive a server restart (real
    Postgres persists them to ``pg_twophase`` so they outlive a crash). This is a
    documented surrogate limitation — see ``tasks/backlog.md``."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._xacts: dict[str, PreparedXact] = {}

    def add(self, xact: PreparedXact) -> None:
        """Register a freshly prepared xact; error 42710 if the gid is in use."""
        with self._lock:
            if xact.gid in self._xacts:
                from secantus.sql import errors

                raise errors.SQLError(
                    "42710", f'transaction identifier "{xact.gid}" is already in use'
                )
            self._xacts[xact.gid] = xact

    def pop(self, gid: str) -> PreparedXact:
        """Remove and return the prepared xact for ``gid``; error 42704 if none."""
        with self._lock:
            xact = self._xacts.pop(gid, None)
        if xact is None:
            from secantus.sql import errors

            raise errors.SQLError(
                "42704", f'prepared transaction with identifier "{gid}" does not exist'
            )
        return xact

    def snapshot(self) -> list[PreparedXact]:
        with self._lock:
            return list(self._xacts.values())


@dataclass
class Session:
    database: str = "postgres"
    user: str = "secantus"
    backend_pid: int = 0
    settings: dict[str, str] = field(default_factory=dict)
    # Server-config GUC defaults (postgresql.conf tier). Sits between the
    # session's ``SET`` overrides and the built-in ``GUC_DEFAULTS``: an explicit
    # ``SET`` wins, ``RESET`` falls back here (not to the built-in), and
    # ``SHOW`` reports the effective value. The wire server seeds this at
    # connect (e.g. its ``idle_in_transaction_timeout_s`` default).
    server_gucs: dict[str, str] = field(default_factory=dict)
    # Reportable-GUC changes made mid-statement by ``set_config()`` (which has
    # no SQLResult of its own to carry them) — drained by the wire layer after
    # each statement and emitted as ParameterStatus messages, like real PG.
    pending_parameter_status: list[tuple[str, str]] = field(default_factory=list)
    # RBAC (#193). ``authz_active`` gates per-statement authorization — the wire
    # server sets it when started with ``require_auth`` *and* explicit per-user
    # role bindings. When false (embedded ``run_sql``, or trust mode) the SQL
    # surface is unrestricted, preserving prior behaviour. ``roles`` is the
    # authenticated user's bindings (``[{"role": ..., "db": ...}]``), reused by
    # ``secantus.rbac.check_privilege`` — the same model the Mongo server uses.
    authz_active: bool = False
    roles: list[Any] = field(default_factory=list)
    # The transaction-stable ``now()`` instant (PG freezes now()/
    # CURRENT_TIMESTAMP for the whole transaction). Cleared at each
    # statement start outside a block and at BEGIN; set lazily by
    # ``scalar._utcnow`` on first use.
    txn_now: Any = None
    # Temp tables this session created (``(db, name)``, name carrying the
    # ``pg_temp_<n>.`` prefix) — dropped at connection teardown by
    # ``engine.drop_session_temp_tables`` (PG drops temp tables at session
    # end; embedded ``run_sql`` sessions live for the process).
    temp_tables: set[tuple[str, str]] = field(default_factory=set)
    # This session's private temp namespace (``pg_temp_<n>``), allocated
    # lazily on first use — real PG gives every backend its own temp schema,
    # which is what keeps concurrent sessions' same-named temp tables apart.
    temp_schema: str | None = None
    # SET ROLE / SET SESSION AUTHORIZATION (#128). ``user`` is the *session user*
    # — the login identity, changed only by SET SESSION AUTHORIZATION. ``role`` is
    # the *current role* override set by SET ROLE (None = current role tracks the
    # session user). ``login_user`` is the original authenticated login, captured
    # once so RESET SESSION AUTHORIZATION can restore it. ``effective_user``
    # (``role or user``) is what ``current_user`` reports and what the table-grant
    # gate (#127) matches against; ``user`` is what ``session_user`` reports.
    role: str | None = None
    login_user: str | None = None
    # Multi-statement transaction state. ``txn_handle`` is the open
    # ``Storage`` user-transaction (None outside a BEGIN block); ``txn_failed``
    # marks an aborted block (every command except COMMIT/ROLLBACK errors with
    # 25P02 until the block ends — Postgres semantics).
    txn_handle: Any = None
    txn_failed: bool = False
    #: The open transaction was started IMPLICITLY by the extended protocol
    #: (statements pipelined before a Sync form one transaction in PG —
    #: pgjdbc's batches rely on a mid-batch error rolling back the whole
    #: pipeline). Sync commits/rolls it back; an explicit BEGIN takes it over.
    txn_is_implicit: bool = False
    # Open savepoints, innermost last. Each is a ``_Savepoint`` carrying the
    # pre-image snapshot (collection -> docs) captured on the first write to that
    # collection after the savepoint was established, so ROLLBACK TO can restore.
    savepoints: list[Any] = field(default_factory=list)
    # Open server-side cursors by name (``DECLARE … CURSOR`` / ``FETCH`` / ``CLOSE``).
    cursors: dict[str, Any] = field(default_factory=dict)
    # The connection's wire-level (extended-protocol Parse) prepared statements —
    # set by ExtendedSession so pg_prepared_statements can list them.
    wire_prepared: dict[str, Any] = field(default_factory=dict)
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
    # Number of channels this session LISTENs on, maintained by NotifyHub
    # under its lock. The wire server's read loop checks it before EVERY
    # message read to decide whether to poll for notification pushes — a
    # plain int read, so the hot path takes no shared lock.
    listen_count: int = 0
    _notify_lock: Any = field(default_factory=threading.Lock)
    _notify_deliveries: deque = field(default_factory=deque)
    pending_notifies: list[tuple[str, str]] = field(default_factory=list)
    # pg_stat_activity (#137). Populated by the wire server: when the connection
    # was established (``backend_start``), the client host (``client_addr``), the
    # activity ``state`` ('active' while a query runs, else 'idle'), the current /
    # last ``current_query`` text and its ``query_start``, and a back-reference to
    # the server-wide ``ActivityRegistry`` (None for the embedded ``run_sql`` API).
    backend_start: Any = None
    client_addr: str | None = None
    state: str = "idle"
    current_query: str = ""
    query_start: Any = None
    activity_registry: Any = None
    # pg_terminate_backend: closes this session's socket (set by the wire
    # server; None for the embedded API).
    terminate_cb: Any = None
    # Query cancellation (the wire CancelRequest sub-protocol and
    # pg_cancel_backend). Set from another thread; cancellation points —
    # pg_sleep, the COPY TO row stream — observe it and raise 57014. Cleared
    # at the start of each query cycle, so a cancel that lands while idle is
    # ignored, like real PG. ``cancel_key`` is the BackendKeyData secret a
    # CancelRequest must echo.
    cancel_event: threading.Event = field(default_factory=threading.Event)
    cancel_key: int = 0
    # ``statement_timeout`` enforcement: the monotonic deadline for the current
    # statement / extended-protocol message batch, or None when unset. Armed at
    # the first statement of a batch (kept across the batch so a slow statement
    # later in it still times out) and cleared at the batch boundary (Sync /
    # simple-query end). ``pg_sleep`` and other cancellation points check it.
    statement_deadline: float | None = None
    # SET LOCAL (#136): GUCs set with ``SET LOCAL`` inside a transaction, mapped to
    # the value to restore at transaction end (the pre-``SET LOCAL`` session value,
    # or None if it wasn't set). Reverted in ``engine._end_txn_state``.
    local_gucs: dict = field(default_factory=dict)
    # Pre-SET values of GUCs set with a plain ``SET`` inside the open
    # transaction block — unwound on ROLLBACK, discarded on COMMIT.
    txn_gucs: dict = field(default_factory=dict)
    # Advisory locks (``pg_advisory_lock`` family, #135). Single-node: a lock is
    # always granted immediately, so we only *track* what this session holds so
    # ``pg_advisory_unlock`` reports truthfully and ``pg_catalog.pg_locks``
    # reflects it. Keyed by ``(classid, objid, objsubid, mode, xact)`` → stack
    # ``count`` (advisory locks are re-entrant); ``xact`` locks release at
    # COMMIT/ROLLBACK, session locks at ``pg_advisory_unlock[_all]``.
    advisory_locks: dict = field(default_factory=dict)
    # Server-wide AdvisoryLockHub (set by the wire server, like notify_hub).
    # None for embedded run_sql sessions — a single connection needs no
    # cross-connection exclusion, and the local bookkeeping alone reproduces
    # the old behaviour there.
    advisory_hub: Any = None
    # Two-phase commit (#139). Server-wide ``PreparedXactRegistry`` shared by all
    # connections (set by the wire server; the embedded ``run_sql`` lazily makes a
    # per-session one). ``PREPARE TRANSACTION`` moves this session's ``txn_handle``
    # into it; ``COMMIT PREPARED`` / ``ROLLBACK PREPARED`` resolve a gid against it.
    prepared_xacts: Any = None

    def __post_init__(self) -> None:
        if self.login_user is None:
            self.login_user = self.user

    @property
    def effective_user(self) -> str:
        """The current role (``current_user``) — the SET ROLE override, else the
        session user."""
        return self.role or self.user

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

    def advisory_lock_acquire(
        self, key: tuple, *, shared: bool, xact: bool, blocking: bool = True
    ) -> bool:
        """Acquire an advisory lock (re-entrant). With the server-wide hub
        attached this is REAL cross-connection exclusion: a blocking acquire
        waits for the holder (aborting with ``40P01`` on a detected deadlock)
        and a ``pg_try_*`` acquire returns whether the lock was granted.
        Embedded sessions (no hub) keep the old always-granted behaviour."""
        if self.advisory_hub is not None:
            granted = self.advisory_hub.acquire(
                self, key, shared=shared, xact=xact, blocking=blocking
            )
            if not granted:
                return False
        mode = "ShareLock" if shared else "ExclusiveLock"
        k = (key[0], key[1], key[2], mode, xact)
        self.advisory_locks[k] = self.advisory_locks.get(k, 0) + 1
        return True

    def advisory_lock_release(self, key: tuple, *, shared: bool) -> bool:
        """Release one *session-level* advisory lock. Returns True if one was
        held (and decremented), False otherwise (Postgres warns and returns
        false). Transaction-level locks aren't manually releasable."""
        mode = "ShareLock" if shared else "ExclusiveLock"
        k = (key[0], key[1], key[2], mode, False)
        n = self.advisory_locks.get(k, 0)
        if n <= 0:
            return False
        if n == 1:
            del self.advisory_locks[k]
        else:
            self.advisory_locks[k] = n - 1
        if self.advisory_hub is not None:
            self.advisory_hub.release(self, (key[0], key[1], key[2]), shared=shared)
        return True

    def advisory_unlock_all(self) -> None:
        """Release every *session-level* advisory lock (``pg_advisory_unlock_all``);
        transaction-level locks are left to the transaction's end."""
        self.advisory_locks = {k: v for k, v in self.advisory_locks.items() if k[4]}
        if self.advisory_hub is not None:
            self.advisory_hub.release_session_level(self)

    def release_xact_advisory_locks(self) -> None:
        """Drop all transaction-level advisory locks (called at COMMIT/ROLLBACK)."""
        self.advisory_locks = {k: v for k, v in self.advisory_locks.items() if not k[4]}
        if self.advisory_hub is not None:
            self.advisory_hub.release_xact(self)

    def held_advisory_locks(self) -> list[tuple]:
        """The distinct advisory locks currently held, as ``(classid, objid,
        objsubid, mode)`` — for ``pg_catalog.pg_locks`` reflection (session- and
        transaction-level collapse to one row per key+mode, as pg_locks shows)."""
        out: dict[tuple, bool] = {}
        for (classid, objid, objsubid, mode, _xact), n in self.advisory_locks.items():
            if n > 0:
                out[(classid, objid, objsubid, mode)] = True
        return list(out.keys())

    def record_txn_guc(self, name: str) -> None:
        """Capture a GUC's pre-``SET`` value when a plain ``SET`` runs inside a
        transaction block — PG unwinds those on ROLLBACK (and keeps them on
        COMMIT). Captured once per GUC per transaction."""
        if name not in self.txn_gucs:
            self.txn_gucs[name] = self.settings.get(name)

    def restore_txn_gucs(self) -> None:
        """ROLLBACK's unwind of plain in-transaction ``SET``s, re-reporting
        GUC_REPORT parameters so the client's ParameterStatus cache reverts."""
        for name, prior in self.txn_gucs.items():
            if prior is None:
                self.settings.pop(name, None)
            else:
                self.settings[name] = prior
        self._report_restored(list(self.txn_gucs))
        self.txn_gucs = {}

    def _report_restored(self, names: list[str]) -> None:
        """Queue ParameterStatus for the GUC_REPORT parameters among ``names``
        after an unwind. PG orders them case-insensitively by name, and a
        reverted ``role`` also re-reports ``is_superuser`` (pgtest
        param_status reads the whole list after ROLLBACK / COMMIT)."""
        reportable = {n for n in names if n in REPORTABLE_GUCS}
        if any(n.lower() == "role" for n in names):
            self.role = self.settings.get("role") or None
            is_super = self.role is None or self.role == (self.login_user or self.user)
            value = "on" if is_super else "off"
            if self.settings.get("is_superuser") != value:
                self.settings["is_superuser"] = value
                reportable.add("is_superuser")
        for name in sorted(reportable, key=str.lower):
            self.pending_parameter_status.append((name, self.get_setting(name)))

    def restore_savepoint_gucs(self, snapshot: dict[str, str]) -> None:
        """Revert to a savepoint's GUC snapshot, re-reporting the GUC_REPORT
        parameters whose value changed (PG orders them case-insensitively by
        name)."""
        changed = [
            name
            for name in set(snapshot) | set(self.settings)
            if snapshot.get(name) != self.settings.get(name)
        ]
        self.settings = dict(snapshot)
        for name in sorted(changed, key=str.lower):
            if name in REPORTABLE_GUCS:
                self.pending_parameter_status.append((name, self.get_setting(name)))

    def set_local(self, name: str, value: str) -> None:
        """``SET LOCAL name = value`` (#136) — applies for the rest of the current
        transaction only. Records the value to restore at transaction end (captured
        once per GUC: the pre-``SET LOCAL`` session value, or ``None`` if unset)."""
        if name not in self.local_gucs:
            self.local_gucs[name] = self.settings.get(name)
        self.settings[name] = value

    def restore_local_gucs(self) -> None:
        """Revert every ``SET LOCAL`` made in the just-ended transaction (called at
        COMMIT / ROLLBACK) — restoring each GUC to its pre-``SET LOCAL`` value."""
        for name, prior in self.local_gucs.items():
            if prior is None:
                self.settings.pop(name, None)
            else:
                self.settings[name] = prior
        # PG re-reports GUC_REPORT parameters when the transaction's SET LOCALs
        # unwind — without this the client's ParameterStatus cache keeps the
        # local value forever (pgjdbc's transactionalParameters* trio).
        self._report_restored(list(self.local_gucs))
        self.local_gucs = {}

    def all_settings(self) -> dict[str, str]:
        """Every GUC's current value — the built-in defaults overlaid with the
        session's ``SET`` overrides. Used by ``SHOW ALL`` and ``pg_settings``."""
        merged = dict(GUC_DEFAULTS)
        merged.update(self.server_gucs)
        merged.update(self.settings)
        return merged

    def apply_database_defaults(self, defaults: dict[str, str]) -> None:
        """Merge per-database GUC defaults (``ALTER DATABASE … SET``) into this
        session. Called once at connect, BEFORE any client ``SET``, so an
        explicit session setting always wins — PG's precedence order."""
        for key, value in defaults.items():
            self.settings.setdefault(canonical_guc_name(key), value)

    def get_setting(self, name: str) -> str:
        key = canonical_guc_name(name)
        if key in self.settings:
            return self.settings[key]
        # The per-transaction characteristics mirror their session defaults
        # until a BEGIN/SET TRANSACTION overrides them (like real Postgres).
        if key in ("transaction_isolation", "transaction_read_only", "transaction_deferrable"):
            return self.get_setting(f"default_{key}")
        if key in self.server_gucs:
            return self.server_gucs[key]
        return GUC_DEFAULTS.get(key, "")

    @property
    def wire_encoding(self) -> str | None:
        """The Python codec for this connection's ``client_encoding``, or None
        for SQL_ASCII (no conversion — bytes pass through)."""
        return python_codec_for(self.get_setting("client_encoding"))

    def statement_timeout_seconds(self) -> float:
        """The ``statement_timeout`` GUC as seconds (0 = disabled). A bare number
        is milliseconds (PG's default unit); an ``s`` / ``min`` / ``h`` / ``ms``
        suffix is honoured."""
        raw = str(self.get_setting("statement_timeout") or "").strip().lower()
        if not raw or raw in ("0", "0ms"):
            return 0.0
        units = (("ms", 0.001), ("min", 60.0), ("h", 3600.0), ("s", 1.0))
        for suffix, scale in units:
            if raw.endswith(suffix):
                try:
                    return float(raw[: -len(suffix)].strip()) * scale
                except ValueError:
                    return 0.0
        try:
            return float(raw) / 1000.0  # bare number == milliseconds
        except ValueError:
            return 0.0

    def arm_statement_deadline(self) -> None:
        """Start the ``statement_timeout`` clock for the current statement /
        message batch if one is configured and not already running. Kept across
        an extended-protocol batch so a slow statement later in it still trips."""
        if self.statement_deadline is not None:
            return
        timeout = self.statement_timeout_seconds()
        if timeout > 0:
            import time as _time

            self.statement_deadline = _time.monotonic() + timeout

    def clear_statement_deadline(self) -> None:
        self.statement_deadline = None

    def txn_status(self) -> bytes:
        """The ReadyForQuery status byte: idle / in-transaction / failed."""
        if self.txn_handle is None:
            return b"I"
        return b"E" if self.txn_failed else b"T"

    def ensure_temp_schema(self) -> str:
        """This session's temp namespace, allocating it on first use."""
        if self.temp_schema is None:
            self.temp_schema = f"pg_temp_{next(_TEMP_SCHEMA_SEQ)}"
        return self.temp_schema

    @property
    def search_path(self) -> list[str]:
        """``search_path`` as a list of schema names, in resolution order.

        "$user" collapses to public (we have no per-user schemas) and repeats
        are dropped, so the list is what an unqualified relation name is
        actually tried against.
        """
        names: list[str] = []
        for raw in self.get_setting("search_path").split(","):
            name = raw.strip().strip('"')
            # "$user" resolves to the user's schema, which we collapse to public.
            if name in ("$user", ""):
                name = "public"
            if name not in names:
                names.append(name)
        return names or ["public"]

    @property
    def current_schema(self) -> str:
        return self.search_path[0]
