"""``run_sql`` — the embedded SQL entry point.

Parses a SQL string, plans each statement, and executes it against a
``Storage`` instance, returning one ``SQLResult`` per statement. This is both
the embedded API and what the PostgreSQL-wire server drives. A per-connection
``Session`` carries the database, user, and GUC settings so session functions
and ``SHOW`` / ``SET`` resolve against real state.
"""

from __future__ import annotations

import contextlib
import copy
import datetime as _dt
import re
from collections.abc import Iterator
from dataclasses import dataclass, field
from typing import Any

import sqlglot
from sqlglot import exp

from secantus.sql import (
    authz,
    errors,
    executor,
    planner,
    reflect,
    rls,
    scalar,
    srf,
    typecheck,
    typemap,
    virtual,
)
from secantus.sql import explain as explain_mod
from secantus.sql import session as sql_session
from secantus.sql.catalog import ALL_CATALOG_COLLECTIONS, Catalog, Column, TableDef
from secantus.sql.result import ColumnDesc, SQLResult
from secantus.sql.session import (
    REPORTABLE_GUCS,
    PreparedXact,
    PreparedXactRegistry,
    Session,
    _Cursor,
    _Savepoint,
)


def drop_session_temp_tables(storage: Any, session: Session) -> None:
    """Drop every temp table ``session`` created — the wire server calls this at
    connection teardown, matching real PG's drop-temp-at-session-end. Stale
    entries (already dropped by the user) are skipped silently."""
    for tdb, name in list(getattr(session, "temp_tables", ()) or ()):
        with contextlib.suppress(Exception):
            catalog = Catalog(storage)
            t = catalog.get(tdb, name)
            if t is not None and t.temp:
                executor.execute_drop_table(
                    planner.DropTablePlan(name=name, if_exists=True), catalog, storage, tdb
                )
    if getattr(session, "temp_tables", None):
        session.temp_tables.clear()


def run_sql(storage: Any, db: str, sql: str, *, session: Session | None = None) -> list[SQLResult]:
    """Execute ``sql`` against ``db`` on ``storage``; one result per statement.

    ``storage`` is any object exposing the ``Storage`` data API. ``session`` is
    the per-connection state (created fresh if omitted, e.g. for the embedded
    API); the wire server passes a long-lived one so ``SET`` persists.
    """
    if session is None:
        session = Session(database=db)
    # LISTEN / NOTIFY / UNLISTEN are handled before sqlglot: it mis-parses
    # ``LISTEN chan`` (as an alias) and fails outright on ``NOTIFY chan, 'p'``.
    pubsub = _maybe_pubsub(sql, session)
    if pubsub is not None:
        return [pubsub]
    catalog = Catalog(storage)
    with _storage_conflicts_as_sqlstate():
        # Two-phase commit (#139) is handled before sqlglot: it cannot parse
        # ``COMMIT PREPARED`` / ``ROLLBACK PREPARED`` at all, and ``PREPARE
        # TRANSACTION`` collides with the SQL-level ``PREPARE name AS`` (#121).
        two_phase = _maybe_two_phase(sql, storage, db, catalog, session)
        if two_phase is not None:
            return [two_phase]
        results: list[SQLResult] = []
        if session.get_setting("standard_conforming_strings").lower() in ("off", "false", "0"):
            sql = planner.decode_nonstandard_strings(sql)
        stmts = planner.parse(sql)
        # A MULTI-statement simple query runs in ONE implicit transaction,
        # like real PG: a mid-batch error rolls back the earlier statements'
        # writes (their result rows were already streamed — PG streams too),
        # and an explicit BEGIN inside the batch takes the transaction over
        # while COMMIT/ROLLBACK end it (the remainder starts a fresh implicit
        # one). Pinned by the pgtest batch_stmt corpus.
        implicit = len(stmts) > 1 and session.txn_handle is None
        if implicit:
            session.txn_handle = storage.begin_user_transaction()
            session.txn_failed = False
            session.txn_is_implicit = True
        try:
            for stmt in stmts:
                if implicit and session.txn_handle is None:
                    session.txn_handle = storage.begin_user_transaction()
                    session.txn_failed = False
                    session.txn_is_implicit = True
                result = _normalize_result(_dispatch(stmt, storage, db, catalog, session))
                _drain_plpgsql_notices(session, result)
                results.append(result)
            if implicit and session.txn_handle is not None and session.txn_is_implicit:
                _commit_txn(storage, db, catalog, session)
        except errors.SQLError as exc:
            if implicit and session.txn_handle is not None and session.txn_is_implicit:
                with contextlib.suppress(Exception):
                    _rollback_txn(storage, session)
            # Real PG streams each statement's results as it executes, so a
            # mid-batch error still delivers the EARLIER statements' rows
            # before the ErrorResponse (pgx's ExecMultipleQueriesError counts
            # them). Carry the completed results on the exception for the
            # wire layer; embedded callers see the same raise as before.
            exc.partial_results = results
            raise
        return results


def _drain_plpgsql_notices(session: Session, result: SQLResult) -> None:
    """Move plpgsql ``RAISE`` notices raised by any function this statement
    evaluated (side-channel from ``secantus.sql.plpgsql``) onto the result, so
    the wire layer emits them as NoticeResponse (pgjdbc surfaces them via
    ``Statement.getWarnings()``)."""
    pending = getattr(session, "plpgsql_notices", None)
    if pending:
        result.notices = list(result.notices or []) + pending
        session.plpgsql_notices = []


@contextlib.contextmanager
def _storage_conflicts_as_sqlstate() -> Iterator[None]:
    """Map a storage-level write-write conflict to SQLSTATE 40001.

    WiredTiger is first-updater-wins: a statement (or COMMIT) that loses a race
    with a concurrent writer raises ``WriteConflictError`` / ``WT_ROLLBACK``.
    Without this mapping the loser escaped as an unhandled exception and the
    wire layer sent the generic ``XX000 internal error`` — a real Postgres
    reports ``40001 serialization_failure``, the retriable signal drivers and
    ORMs key their retry loops on. Imported lazily so the engine keeps no
    module-level dependency on the storage implementation."""
    try:
        yield
    except errors.SQLError:
        raise
    except Exception as exc:
        from secantus.aggregate import AggregateError
        from secantus.storage import WriteConflictError, _is_wt_rollback

        if isinstance(exc, WriteConflictError) or _is_wt_rollback(exc):
            raise errors.serialization_failure() from exc
        if isinstance(exc, AggregateError):
            # The pipeline hit a hard limit (an unbounded cross-product cap) —
            # surface it as a clean SQLSTATE 54000 (program_limit_exceeded)
            # instead of a generic XX000 internal error.
            raise errors.SQLError("54000", str(exc)) from exc
        raise


def _normalize_result(result: SQLResult) -> SQLResult:
    """Tag naive ``timestamptz`` result values UTC-aware (#141), so the embedded
    ``run_sql`` return matches the tz-aware instant the wire path renders. No-op
    unless a column is ``timestamptz`` / ``timestamptz[]``."""
    if not result.rows or not result.columns:
        return result
    idxs = [
        i
        for i, c in enumerate(result.columns)
        if c.type_tag == "timestamptz" or c.type_tag == "timestamptz[]"
    ]
    if not idxs:
        return result
    rows = []
    for row in result.rows:
        r = list(row)
        for i in idxs:
            r[i] = typemap.normalize_result_value(r[i], result.columns[i].type_tag)
        rows.append(tuple(r))
    result.rows = rows
    return result


_PUBSUB_RE = re.compile(
    r"""^\s*
    (?P<verb>LISTEN|UNLISTEN|NOTIFY)\s+
    (?P<chan>\*|"[^"]+"|[A-Za-z_][A-Za-z0-9_$]*)
    (?:\s*,\s*(?P<payload>'(?:[^']|'')*'))?
    \s*;?\s*$""",
    re.IGNORECASE | re.VERBOSE,
)


def is_pubsub_statement(sql: str) -> bool:
    """Whether ``sql`` is a single LISTEN / UNLISTEN / NOTIFY command — used by the
    wire server to skip the sqlglot-based COPY probe, which chokes on ``NOTIFY
    chan, 'payload'``."""
    return _PUBSUB_RE.match(sql) is not None


def _pubsub_ident(token: str) -> str:
    """Normalise a LISTEN/NOTIFY channel token: a quoted ``"Ch"`` keeps its case;
    an unquoted name is lower-cased (Postgres identifier folding)."""
    if token.startswith('"') and token.endswith('"'):
        return token[1:-1]
    return token.lower()


def _maybe_pubsub(sql: str, session: Session) -> SQLResult | None:
    """Handle a single ``LISTEN`` / ``UNLISTEN`` / ``NOTIFY`` statement, or None if
    ``sql`` isn't one. Requires ``session.notify_hub`` (the wire server); with no
    hub (embedded API) the commands are accepted as no-ops so scripts don't error."""
    m = _PUBSUB_RE.match(sql)
    if m is None:
        return None
    verb = m.group("verb").upper()
    hub = session.notify_hub
    if verb == "UNLISTEN":
        if hub is not None:
            if m.group("chan") == "*":
                hub.unlisten_all(session)
            else:
                hub.unlisten(_pubsub_ident(m.group("chan")), session)
        return SQLResult(command_tag="UNLISTEN")
    channel = _pubsub_ident(m.group("chan"))
    if verb == "LISTEN":
        if hub is not None:
            hub.listen(channel, session)
        return SQLResult(command_tag="LISTEN")
    # NOTIFY — the payload literal is single-quoted with '' escaping.
    raw = m.group("payload")
    payload = raw[1:-1].replace("''", "'") if raw else ""
    if hub is not None:
        if session.txn_handle is not None:
            # Inside a transaction block: buffer, deliver at COMMIT.
            session.pending_notifies.append((channel, payload))
        else:
            hub.notify(channel, payload, session.backend_pid)
    return SQLResult(command_tag="NOTIFY")


_TWO_PHASE_RE = re.compile(
    r"""^\s*
    (?:
        (?P<prepare>PREPARE)\s+TRANSACTION
      | (?P<commit>COMMIT)\s+PREPARED
      | (?P<rollback>ROLLBACK)\s+PREPARED
    )\s+
    (?P<gid>'(?:[^']|'')*')
    \s*;?\s*$""",
    re.IGNORECASE | re.VERBOSE,
)


def is_two_phase_statement(sql: str) -> bool:
    """Whether ``sql`` is a single ``PREPARE TRANSACTION`` / ``COMMIT PREPARED`` /
    ``ROLLBACK PREPARED`` — used by the wire server to skip the sqlglot COPY probe,
    which can't parse ``COMMIT``/``ROLLBACK PREPARED`` at all (#139)."""
    return _TWO_PHASE_RE.match(sql) is not None


def _ensure_prepared_registry(session: Session) -> PreparedXactRegistry:
    """The session's shared ``PreparedXactRegistry`` (set by the wire server), or
    a lazily-created per-session one for the embedded ``run_sql`` API — so a
    ``PREPARE`` / ``COMMIT PREPARED`` pair on one session works standalone."""
    if session.prepared_xacts is None:
        session.prepared_xacts = PreparedXactRegistry()
    return session.prepared_xacts


def _maybe_two_phase(
    sql: str, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult | None:
    """Handle a single ``PREPARE TRANSACTION`` / ``COMMIT PREPARED`` / ``ROLLBACK
    PREPARED`` statement (#139), or None if ``sql`` isn't one."""
    m = _TWO_PHASE_RE.match(sql)
    if m is None:
        return None
    gid = m.group("gid")[1:-1].replace("''", "'")
    if m.group("prepare"):
        return _prepare_transaction(gid, storage, db, catalog, session)
    if m.group("commit"):
        return _commit_prepared(gid, storage, session)
    return _rollback_prepared(gid, storage, session)


def _prepare_transaction(
    gid: str, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``PREPARE TRANSACTION 'gid'`` — detach the open block's ``Storage``
    user-transaction into the prepared-xact registry (uncommitted), leaving the
    session with no active transaction. The writes commit only at a later
    ``COMMIT PREPARED 'gid'`` (which may run on a different connection)."""
    handle = session.txn_handle
    if handle is None:
        raise errors.SQLError("25P01", "PREPARE TRANSACTION can only be used in transaction blocks")
    if session.txn_failed:
        # An aborted block can't be prepared; Postgres rolls it back and errors.
        storage.abort_user_transaction(handle)
        _end_txn_state(session)
        raise errors.SQLError(
            "25P02",
            "current transaction is aborted, commands ignored until end of transaction block",
        )
    registry = _ensure_prepared_registry(session)
    # Deferred constraints are checked at PREPARE time (as at COMMIT); a surviving
    # violation aborts the block and re-raises, exactly like _commit_txn.
    if session.pending_deferred:
        try:
            with storage.use_user_transaction(handle):
                executor.flush_deferred(session, storage, db, catalog)
        except Exception:
            storage.abort_user_transaction(handle)
            _end_txn_state(session)
            raise
    xact = PreparedXact(
        gid=gid,
        handle=handle,
        owner=session.effective_user,
        database=session.database,
        prepared_at=_dt.datetime.now(_dt.timezone.utc),
        notifies=list(session.pending_notifies),
    )
    # add() raises 42710 on a duplicate gid *before* we clear the session, so the
    # block stays open (the handle isn't lost) and the client can ROLLBACK it.
    registry.add(xact)
    _end_txn_state(session)
    return SQLResult(command_tag="PREPARE TRANSACTION")


def _commit_prepared(gid: str, storage: Any, session: Session) -> SQLResult:
    """``COMMIT PREPARED 'gid'`` — commit a previously prepared transaction. Must
    run outside any transaction block; delivers the block's buffered NOTIFYs."""
    if session.txn_handle is not None:
        raise errors.SQLError("25001", "COMMIT PREPARED cannot run inside a transaction block")
    xact = _ensure_prepared_registry(session).pop(gid)
    storage.commit_user_transaction(xact.handle)
    if session.notify_hub is not None:
        for channel, payload in xact.notifies:
            session.notify_hub.notify(channel, payload, session.backend_pid)
    return SQLResult(command_tag="COMMIT PREPARED")


def _rollback_prepared(gid: str, storage: Any, session: Session) -> SQLResult:
    """``ROLLBACK PREPARED 'gid'`` — abort a previously prepared transaction. Must
    run outside any transaction block."""
    if session.txn_handle is not None:
        raise errors.SQLError("25001", "ROLLBACK PREPARED cannot run inside a transaction block")
    xact = _ensure_prepared_registry(session).pop(gid)
    storage.abort_user_transaction(xact.handle)
    return SQLResult(command_tag="ROLLBACK PREPARED")


def _dispatch(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Apply transaction framing around a statement, then execute it.

    BEGIN/COMMIT/ROLLBACK manage a real ``Storage`` user-transaction on the
    session; other statements inside an open block run within it (so a ROLLBACK
    undoes their writes), and a statement that errors poisons the block until it
    ends (Postgres' aborted-transaction semantics).
    """
    # ``now()`` / CURRENT_TIMESTAMP are transaction-stable in Postgres (every
    # call in a transaction sees the same instant; autocommit statements are
    # their own transaction). Reset the frozen clock at each statement start
    # unless an explicit block is open — scalar's ``_utcnow`` freezes the first
    # value it derives into ``session.txn_now``.
    if session.txn_handle is None:
        session.txn_now = None
    if isinstance(stmt, exp.Transaction):
        session.txn_now = None  # BEGIN starts a fresh transaction clock
        return _begin_txn(storage, session, stmt.args.get("modes") or [])
    if isinstance(stmt, exp.Commit):
        return _commit_txn(storage, db, catalog, session)
    if isinstance(stmt, exp.Rollback):
        if stmt.args.get("savepoint") is not None:
            # ROLLBACK TO SAVEPOINT recovers the block from an error, so it runs
            # even when the transaction is poisoned (unlike SAVEPOINT / RELEASE).
            return _rollback_to_savepoint(stmt.args["savepoint"].name, storage, db, session)
        return _rollback_txn(storage, session)

    if session.txn_failed:
        raise errors.SQLError(
            "25P02",
            "current transaction is aborted, commands ignored until end of transaction block",
        )

    sp = _savepoint_command(stmt)
    if sp is not None:
        word, name = sp
        return (
            _savepoint(name, session) if word == "SAVEPOINT" else _release_savepoint(name, session)
        )

    # Per-statement RBAC gate (#193). No-op unless the session marked
    # authorization active (wire server with require_auth + per-user roles);
    # transaction control and savepoints above are exempt and already returned.
    authz.authorize(stmt, session, storage, catalog)
    # Secondary-read gate: a write statement's source clause (INSERT...SELECT,
    # UPDATE...FROM, DELETE...USING, CREATE TABLE...AS SELECT, subqueries) reads
    # tables the primary write privilege doesn't cover — each needs its own
    # ``find`` (SELECT) grant, or a write-only role could exfiltrate them
    # (#785, #881).
    authz.authorize_source_reads(stmt, session, storage, catalog)

    # Read-only enforcement: a write inside a READ ONLY transaction (or under
    # ``default_transaction_read_only = on``) fails with PG's 25006. The
    # ``transaction_read_only`` GUC already resolves through the session
    # default outside a block, so one check covers ``BEGIN READ ONLY``,
    # ``SET TRANSACTION READ ONLY``, and the session characteristic. PG
    # exempts temporary tables; we don't (noted in tasks/backlog.md) — no
    # gauge exercises a temp-table write under read-only.
    if session.get_setting("transaction_read_only") == "on":
        verb = _write_statement_verb(stmt)
        if verb is not None:
            if session.txn_handle is not None:
                # Any error inside a transaction block poisons it (RFQ 'E')
                # — this gate raises BEFORE the wrapped execution whose
                # except-clause normally sets the flag.
                session.txn_failed = True
            raise errors.SQLError("25006", f"cannot execute {verb} in a read-only transaction")

    if session.txn_handle is not None:
        try:
            with storage.use_user_transaction(session.txn_handle):
                _capture_savepoint_snapshots(stmt, storage, db, catalog, session)
                return _run_statement(stmt, storage, db, catalog, session)
        except Exception:
            session.txn_failed = True
            raise
    return _run_statement(stmt, storage, db, catalog, session)


def _begin_txn(storage: Any, session: Session, modes: list | None = None) -> SQLResult:
    # An explicit BEGIN inside an IMPLICIT transaction (extended-protocol
    # pipeline or a multi-statement simple batch) takes it over (PG
    # semantics): same handle, now a real block — and the BEGIN's
    # characteristics still apply (``BEGIN READ ONLY`` mid-batch must make
    # the following INSERT fail 25006; pgtest batch_stmt pins it).
    if session.txn_handle is not None and session.txn_is_implicit:
        session.txn_is_implicit = False
        for name, value in _parse_txn_characteristics(
            " ".join(str(m) for m in (modes or []))
        ).items():
            session.set_local(name, value)
        return SQLResult(command_tag="BEGIN")
    # A nested BEGIN is a no-op in Postgres (it warns and stays in the block).
    if session.txn_handle is None:
        session.txn_handle = storage.begin_user_transaction()
        session.txn_failed = False
        session.savepoints = []
        session.reset_deferred()
        # ``BEGIN ISOLATION LEVEL x / READ ONLY / DEFERRABLE`` — apply the
        # characteristics for this transaction only (the SET LOCAL mechanism
        # reverts them at COMMIT/ROLLBACK). Single-node: the isolation level is
        # tracked and reported but every transaction runs on WiredTiger's
        # snapshot isolation regardless.
        for name, value in _parse_txn_characteristics(
            " ".join(str(m) for m in (modes or []))
        ).items():
            session.set_local(name, value)
        return SQLResult(command_tag="BEGIN")
    # A nested BEGIN inside an EXPLICIT block: PG warns 25001, keeps the
    # block, and still completes with the BEGIN tag — the pgtest
    # implicit_txn corpus reads the WARNING's File/Routine fields.
    return SQLResult(
        command_tag="BEGIN",
        notices=[
            (
                "WARNING",
                "there is already a transaction in progress",
                "25001",
                "xact.c",
                "BeginTransactionBlock",
            )
        ],
    )


def _check_portal_table_pin(session: Session, table_name: str) -> None:
    """PG refuses DROP TABLE while an ACTIVE portal in this session still
    reads the table — 55006, and the block poisons (pgtest
    multiple_active_portals). A fully-drained portal no longer pins."""
    portals = getattr(session, "wire_portals", None) or {}
    for p in portals.values():
        stmt = p.bound_stmt if p.bound_stmt is not None else getattr(p.prepared, "stmt", None)
        if stmt is None:
            continue
        # Only an ACTIVE READ CURSOR pins a table against DROP — PG's "being
        # used" is about open cursors, not other statements. A write portal
        # (DML / DDL — crucially the ``DROP TABLE`` being executed right now,
        # which carries the table as its own target and otherwise pinned itself
        # → the DatabaseMetaDataTest setup regression) never pins, and neither
        # does a portal that has not been executed (a prepared-but-unopened
        # cursor holds nothing).
        if _write_statement_verb(stmt) is not None or not p.executed:
            continue
        if p.result is not None:
            rows = getattr(p.result, "rows", None)
            if rows is not None and p.offset >= len(rows):
                continue  # drained
        if any(t.name == table_name for t in stmt.find_all(exp.Table)):
            if session.txn_handle is not None:
                session.txn_failed = True
            raise errors.SQLError(
                "55006",
                f'cannot DROP TABLE "{table_name}" because it is being used by '
                "active queries in this session",
            )


def _write_statement_verb(stmt: exp.Expression) -> str | None:
    """The verb PG names in its 25006 error when ``stmt`` writes, else None.

    Matches PG's classification: DML and DDL are writes; SELECT, SHOW, SET,
    EXPLAIN, and cursor traffic are not. (``SELECT … FOR UPDATE`` and
    ``nextval()`` are also writes in PG; neither is gated here — noted in
    tasks/backlog.md.)"""
    if isinstance(stmt, exp.Insert):
        return "INSERT"
    if isinstance(stmt, exp.Update):
        return "UPDATE"
    if isinstance(stmt, exp.Delete):
        return "DELETE"
    if isinstance(stmt, exp.Merge):
        return "MERGE"
    if isinstance(stmt, exp.TruncateTable):
        return "TRUNCATE TABLE"
    if isinstance(stmt, exp.Create):
        kind = str(stmt.args.get("kind") or "").upper()
        return f"CREATE {kind}".strip()
    if isinstance(stmt, exp.Drop):
        kind = str(stmt.args.get("kind") or "").upper()
        return f"DROP {kind}".strip()
    if isinstance(stmt, exp.Alter):
        kind = str(stmt.args.get("kind") or "").upper()
        return f"ALTER {kind}".strip()
    if isinstance(stmt, exp.Grant):
        return "GRANT"
    # A SELECT/statement calling a mutating large-object function
    # (`SELECT lo_unlink(oid)`, `INSERT ... VALUES (lo_creat(-1))`) writes even
    # though its top node isn't a DML verb — `_write_statement_verb` classifying
    # it as a read let it slip the read-only-transaction gate (#836).
    for fn in stmt.find_all(exp.Anonymous):
        if str(fn.this).lower() in _MUTATING_LO_FUNCS:
            return "lo_* write function"
    return None


# Large-object functions that mutate stored data when invoked as ordinary
# SQL scalars (the ones `scalar.py` actually implements) — used to hold the
# read-only-transaction gate over the `SELECT lo_unlink(...)` path that skips
# the Fastpath sub-protocol (#836).
_MUTATING_LO_FUNCS: frozenset[str] = frozenset({"lo_creat", "lo_create", "lo_unlink"})


_TXN_ISOLATION_RE = re.compile(
    r"isolation\s+level\s+(read\s+uncommitted|read\s+committed|repeatable\s+read|serializable)",
    re.IGNORECASE,
)


def _parse_txn_characteristics(tail: str) -> dict[str, str]:
    """Map a transaction-characteristics tail (from BEGIN / START TRANSACTION /
    SET TRANSACTION) to the ``transaction_*`` GUC values it sets."""
    out: dict[str, str] = {}
    m = _TXN_ISOLATION_RE.search(tail)
    if m is not None:
        out["transaction_isolation"] = re.sub(r"\s+", " ", m.group(1).lower())
    if re.search(r"\bread\s+only\b", tail, re.IGNORECASE):
        out["transaction_read_only"] = "on"
    elif re.search(r"\bread\s+write\b", tail, re.IGNORECASE):
        out["transaction_read_only"] = "off"
    if re.search(r"\bnot\s+deferrable\b", tail, re.IGNORECASE):
        out["transaction_deferrable"] = "off"
    elif re.search(r"\bdeferrable\b", tail, re.IGNORECASE):
        out["transaction_deferrable"] = "on"
    return out


def _commit_txn(storage: Any, db: str, catalog: Catalog, session: Session) -> SQLResult:
    handle, failed = session.txn_handle, session.txn_failed
    # Notifications issued in the block deliver only if the commit lands; capture
    # them before _end_txn_state clears the session's buffer.
    buffered_notifies = list(session.pending_notifies)
    # Deferred constraints re-check against the in-transaction state before the
    # commit lands; a surviving violation aborts the block (and re-raises).
    if handle is not None and not failed and session.pending_deferred:
        try:
            with storage.use_user_transaction(handle):
                executor.flush_deferred(session, storage, db, catalog)
        except Exception:
            storage.abort_user_transaction(handle)
            _end_txn_state(session)
            session.restore_txn_gucs()  # the block rolled back — SETs unwind
            raise
    _end_txn_state(session)
    if handle is None:
        session.txn_gucs = {}
        return SQLResult(command_tag="COMMIT")  # no open block — Postgres warns, returns COMMIT
    if failed:
        # COMMIT of an aborted block actually rolls back (and tags ROLLBACK) —
        # the block's plain SETs unwind like any rollback.
        session.restore_txn_gucs()
        storage.abort_user_transaction(handle)
        return SQLResult(command_tag="ROLLBACK")
    session.txn_gucs = {}  # COMMIT keeps the block's plain SETs
    storage.commit_user_transaction(handle)
    if session.notify_hub is not None:
        for channel, payload in buffered_notifies:
            session.notify_hub.notify(channel, payload, session.backend_pid)
    return SQLResult(command_tag="COMMIT")


def _end_txn_state(session: Session) -> None:
    """Clear all per-transaction session state at the end of a block."""
    session.txn_handle = None
    session.txn_failed = False
    session.txn_is_implicit = False
    session.savepoints = []
    session.pending_notifies = []  # NOTIFYs in the block are flushed (commit) or dropped (rollback)
    session.reset_deferred()
    session.restore_local_gucs()  # SET LOCAL reverts at end of transaction
    session.release_xact_advisory_locks()  # pg_advisory_xact_lock* release at txn end
    _close_non_hold_cursors(session)  # WITHOUT HOLD cursors close at end of txn
    # PG destroys portals at transaction end — clearing here (not only at the
    # extended protocol's Sync) covers blocks ended through the simple-query
    # path, and removes any chance of a recycled txn-handle id() matching a
    # stale portal's token (pgtest multiple_active_portals).
    portals = getattr(session, "wire_portals", None)
    if portals:
        portals.clear()


def _rollback_txn(storage: Any, session: Session) -> SQLResult:
    handle = session.txn_handle
    _end_txn_state(session)  # unwinds SET LOCAL first (LOCAL sits atop txn SET)
    session.restore_txn_gucs()  # then unwind the block's plain SETs
    if handle is not None:
        storage.abort_user_transaction(handle)
    return SQLResult(command_tag="ROLLBACK")


def _savepoint_command(stmt: exp.Expression) -> tuple[str, str] | None:
    """A bare ``SAVEPOINT name`` / ``RELEASE name`` — sqlglot parses both as a
    top-level ``Alias`` (``SAVEPOINT AS name``). Returns ``(word, name)`` or None
    (``RELEASE SAVEPOINT name`` is normalized to ``RELEASE name`` in ``parse``)."""
    if not isinstance(stmt, exp.Alias):
        return None
    head = stmt.this
    if isinstance(head, exp.Column) and head.name.upper() in ("SAVEPOINT", "RELEASE"):
        return head.name.upper(), stmt.alias
    return None


def _savepoint(name: str, session: Session) -> SQLResult:
    if session.txn_handle is None:
        raise errors.SQLError("25P01", "SAVEPOINT can only be used in transaction blocks")
    session.savepoints.append(_Savepoint(name=name, gucs=dict(session.settings)))
    return SQLResult(command_tag="SAVEPOINT")


def _find_savepoint(session: Session, name: str) -> int | None:
    """Index of the innermost (topmost) open savepoint named ``name``, or None."""
    for i in range(len(session.savepoints) - 1, -1, -1):
        if session.savepoints[i].name == name:
            return i
    return None


def _release_savepoint(name: str, session: Session) -> SQLResult:
    """``RELEASE SAVEPOINT name`` — destroy it (and any nested inside it), keeping
    their writes. Merge their pre-image snapshots down into the enclosing
    savepoint so it can still undo them (oldest snapshot per collection wins)."""
    if session.txn_handle is None:
        raise errors.SQLError("25P01", "RELEASE SAVEPOINT can only be used in transaction blocks")
    idx = _find_savepoint(session, name)
    if idx is None:
        raise errors.SQLError("3B001", f'savepoint "{name}" does not exist')
    released = session.savepoints[idx:]
    del session.savepoints[idx:]
    if session.savepoints:
        parent = session.savepoints[-1]
        for fr in released:  # lowest first → its snapshot wins under setdefault
            for coll, snap in fr.snapshots.items():
                parent.snapshots.setdefault(coll, snap)
    return SQLResult(command_tag="RELEASE")


def _rollback_to_savepoint(name: str, storage: Any, db: str, session: Session) -> SQLResult:
    """``ROLLBACK TO SAVEPOINT name`` — undo every write since the savepoint by
    restoring each touched collection to its captured pre-image, discard the
    nested savepoints, un-poison the block, and keep the savepoint itself open."""
    if session.txn_handle is None:
        raise errors.SQLError(
            "25P01", "ROLLBACK TO SAVEPOINT can only be used in transaction blocks"
        )
    idx = _find_savepoint(session, name)
    if idx is None:
        raise errors.SQLError("3B001", f'savepoint "{name}" does not exist')
    # Restore each collection to the OLDEST snapshot among this savepoint and the
    # nested ones (the lowest frame's snapshot == the state at ``name``).
    restore: dict[str, list] = {}
    for fr in session.savepoints[idx:]:
        for coll, snap in fr.snapshots.items():
            restore.setdefault(coll, snap)
    with storage.use_user_transaction(session.txn_handle):
        for coll, snap in restore.items():
            storage.delete_matching(db, coll, {})
            if snap:
                storage.insert(db, coll, [copy.deepcopy(d) for d in snap])
    # Drop the nested savepoints; keep ``name`` (a repeat ROLLBACK TO must work,
    # and its snapshots still hold the pre-``name`` state).
    del session.savepoints[idx + 1 :]
    session.txn_failed = False
    # GUCs set after the savepoint revert with it, and the GUC_REPORT ones are
    # re-reported (pgtest param_status reads them after ROLLBACK TO SAVEPOINT).
    session.restore_savepoint_gucs(session.savepoints[idx].gucs)
    return SQLResult(command_tag="ROLLBACK")


def _capture_savepoint_snapshots(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> None:
    """Before a write runs, snapshot its target collection into every open
    savepoint that hasn't captured it yet — that pins each savepoint's view of the
    collection to its establishment state (nothing wrote to it in between)."""
    if not session.savepoints:
        return
    # A DDL statement snapshots every catalog collection (they're tiny) so the
    # schema change is reverted by ROLLBACK TO SAVEPOINT; a DML statement
    # snapshots only its target collection.
    if _is_ddl(stmt):
        colls: tuple[str, ...] = ALL_CATALOG_COLLECTIONS
    else:
        target = _write_target_collection(stmt, catalog, db, storage)
        if target is None:
            return
        colls = (target,)
    for coll in colls:
        snap: list | None = None
        for fr in session.savepoints:
            if coll in fr.snapshots:
                continue
            if snap is None:
                snap = [copy.deepcopy(d) for d in storage.find_matching(db, coll, {})]
            fr.snapshots[coll] = snap


def _is_ddl(stmt: exp.Expression) -> bool:
    """Whether ``stmt`` changes catalog state (CREATE / DROP / ALTER / COMMENT /
    a CREATE TYPE-style Command) — those need their catalog collections
    snapshotted for savepoint rollback."""
    if isinstance(stmt, (exp.Create, exp.Drop, exp.Alter, exp.Comment)):
        return True
    if isinstance(stmt, exp.Command):
        verb = str(stmt.this).upper()
        return verb in ("CREATE", "DROP", "ALTER", "COMMENT")
    return False


def _write_target_collection(
    stmt: exp.Expression, catalog: Catalog, db: str, storage: Any
) -> str | None:
    """The collection a DML statement writes to (INSERT / UPDATE / DELETE, incl. a
    ``WITH`` prefix), or None for a read / DDL / other statement."""
    if isinstance(stmt, exp.Insert):
        target = stmt.this  # a Schema(Table, cols) or a bare Table
        table_node = target.find(exp.Table) if target is not None else None
    elif isinstance(stmt, (exp.Update, exp.Delete)):
        table_node = stmt.find(exp.Table)
    elif isinstance(stmt, exp.Merge):
        table_node = stmt.this if isinstance(stmt.this, exp.Table) else None
    else:
        return None
    if table_node is None:
        return None
    name = planner.qualified_table_name(table_node)
    table = catalog.get(db, name)
    return table.collection if table is not None else name


# --------------------------------------------------------------------------- #
# Server-side cursors: DECLARE … CURSOR / FETCH / MOVE / CLOSE.
# The query is materialized once at DECLARE; FETCH/MOVE walk a scroll position
# over the stored rows (so forward/backward/absolute/relative all work).
# --------------------------------------------------------------------------- #

_CURSOR_TAIL = re.compile(r"^(?P<opts>.*?)\bFOR\b\s+(?P<query>.*)$", re.IGNORECASE | re.DOTALL)

#: Per-session caps on server-side cursors (each holds its whole result set in
#: memory). Combined with the connection cap in ``pgserver``, these bound total
#: memory under an unauthenticated flood. Generous enough for real use. (#194)
MAX_CURSORS_PER_SESSION = 100
MAX_CURSOR_ROWS = 1_000_000


def _command_tail(stmt: exp.Command) -> str:
    arg = stmt.expression
    return str(arg.name if isinstance(arg, exp.Literal) else (arg or "")).strip()


def _unquote_ident(tok: str) -> str:
    return tok[1:-1] if len(tok) >= 2 and tok[0] == '"' and tok[-1] == '"' else tok


def _declare_cursor(
    stmt: exp.Command, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``DECLARE name [opts] CURSOR [opts] FOR <query>`` — run the query now and
    store its rows under ``name`` for later FETCH/MOVE."""
    tail = _command_tail(stmt)  # e.g. ``c CURSOR FOR SELECT …``
    m = _CURSOR_TAIL.match(tail)
    if m is None:
        raise errors.syntax_error(f"malformed DECLARE CURSOR: {tail}")
    opts = m.group("opts")
    parts = opts.split(None, 1)
    if not parts:
        raise errors.syntax_error("DECLARE CURSOR requires a cursor name")
    name = _unquote_ident(parts[0])
    # Cap the number of open server-side cursors per session — each holds its
    # whole result set in memory, so an unbounded count is a memory-DoS. A
    # re-DECLARE of an existing name replaces it (no net growth). (#194)
    if name not in session.cursors and len(session.cursors) >= MAX_CURSORS_PER_SESSION:
        raise errors.program_limit_exceeded(
            f"too many open cursors (limit {MAX_CURSORS_PER_SESSION}); CLOSE some first"
        )
    hold = re.search(r"\bWITH\s+HOLD\b", opts, re.IGNORECASE) is not None
    if re.search(r"\bNO\s+SCROLL\b", opts, re.IGNORECASE):
        scrollable: bool | None = False
    elif re.search(r"\bSCROLL\b", opts, re.IGNORECASE):
        scrollable = True
    else:
        scrollable = None
    stmts = planner.parse(m.group("query"))
    if len(stmts) != 1:
        raise errors.syntax_error("DECLARE CURSOR expects a single query")
    if not isinstance(stmts[0], (exp.Select, exp.SetOperation, exp.Values)) and not (
        isinstance(stmts[0], exp.Command) and str(stmts[0].this).upper() in ("WITH", "SELECT")
    ):
        # A DECLARE body must be a row-returning query — a bare identifier
        # (``wat``) or a DDL/DML statement is a syntax error (42601 →
        # ProgrammingError), not 0A000.
        raise errors.syntax_error("DECLARE CURSOR must specify a SELECT query")
    materialize_cursor(
        name,
        stmts[0],
        storage,
        db,
        catalog,
        session,
        statement=f"DECLARE {tail}",
        hold=hold,
        scrollable=scrollable,
        skip_cap_check=True,
    )
    return SQLResult(command_tag="DECLARE CURSOR")


def materialize_cursor(
    name: str,
    stmt: exp.Expression,
    storage: Any,
    db: str,
    catalog: Catalog,
    session: Session,
    *,
    statement: str,
    hold: bool = False,
    scrollable: bool | None = None,
    skip_cap_check: bool = False,
) -> None:
    """Run ``stmt`` and store its rows as a named session cursor — the shared
    tail of ``DECLARE … CURSOR FOR`` and plpgsql's ``OPEN <var> FOR``."""
    if (
        not skip_cap_check
        and name not in session.cursors
        and len(session.cursors) >= MAX_CURSORS_PER_SESSION
    ):
        raise errors.program_limit_exceeded(
            f"too many open cursors (limit {MAX_CURSORS_PER_SESSION}); CLOSE some first"
        )
    result = _run_query(stmt, storage, db, catalog, session)
    rows = list(result.rows)
    # Cap the materialized row set a single cursor retains (SecantusDB cursors
    # are eager, unlike mongod's lazy ones). Bounds the memory one connection can
    # pin; the number is generous so real queries aren't affected. (#194)
    if len(rows) > MAX_CURSOR_ROWS:
        raise errors.program_limit_exceeded(
            f"cursor result too large: {len(rows)} rows exceeds the {MAX_CURSOR_ROWS} limit"
        )
    session.cursors[name] = _Cursor(
        name=name,
        columns=result.columns,
        rows=rows,
        pos=-1,
        hold=hold,
        scrollable=scrollable,
        statement=statement,
        created=_dt.datetime.now(_dt.timezone.utc),
    )


_FETCH_DIRECTIONS = frozenset(
    {"NEXT", "PRIOR", "FIRST", "LAST", "ABSOLUTE", "RELATIVE", "FORWARD", "BACKWARD"}
)


def _parse_fetch(tail: str) -> tuple[str, int | None, str]:
    """Parse a FETCH/MOVE tail into ``(kind, count, cursor_name)``. ``kind`` is one
    of forward / backward / absolute / relative; ``count`` is the row count (None =
    ALL). The cursor name is the final token; an optional ``FROM`` / ``IN`` before
    it is dropped."""
    # A quoted trailing cursor name may contain spaces — plpgsql refcursors
    # are named like PG's ``<unnamed portal 1>`` and pgjdbc FETCHes them
    # double-quoted, so a naive whitespace split would truncate the name.
    quoted = re.search(r'"((?:[^"]|"")*)"\s*$', tail.strip())
    if quoted is not None:
        name = quoted.group(1).replace('""', '"')
        toks = tail.strip()[: quoted.start()].split()
    else:
        toks = tail.split()
        if not toks:
            raise errors.syntax_error("FETCH requires a cursor name")
        name = _unquote_ident(toks.pop())
    if toks and toks[-1].upper() in ("FROM", "IN"):
        toks.pop()
    spec = [t.upper() for t in toks]

    def as_count(tokens: list[str]) -> int | None:
        if not tokens:
            return 1
        if tokens[0] == "ALL":
            return None
        try:
            return int(tokens[0])
        except ValueError as exc:
            raise errors.syntax_error(f"invalid FETCH count: {tokens[0]}") from exc

    if not spec:
        return "forward", 1, name  # bare FETCH cursor → next row
    head = spec[0]
    if head not in _FETCH_DIRECTIONS:
        # ``FETCH n`` / ``FETCH ALL`` — a NEGATIVE bare count scans backward
        # ``abs(n)`` rows in the default direction (Postgres semantics).
        cnt = as_count(spec)
        if cnt is not None and cnt < 0:
            return "backward", -cnt, name
        return "forward", cnt, name
    rest = spec[1:]
    if head == "NEXT":
        return "forward", 1, name
    if head == "PRIOR":
        return "backward", 1, name
    if head == "FIRST":
        return "absolute", 1, name
    if head == "LAST":
        return "absolute", -1, name
    if head in ("FORWARD", "BACKWARD"):
        cnt = as_count(rest)
        base = "forward" if head == "FORWARD" else "backward"
        if cnt is not None and cnt < 0:  # a signed count flips the direction
            return ("backward" if base == "forward" else "forward"), -cnt, name
        return base, cnt, name
    # ABSOLUTE / RELATIVE require a count. FORWARD/BACKWARD with a negative
    # count reverse direction (``FORWARD -1`` == ``BACKWARD 1``).
    if not rest:
        raise errors.syntax_error(f"{head} requires a count")
    cnt = int(rest[0])
    if head in ("FORWARD", "BACKWARD") and cnt < 0:
        return ("backward" if head == "FORWARD" else "forward"), -cnt, name
    return head.lower(), cnt, name


def _cursor_slice(cur: Any, kind: str, count: int | None) -> list:
    """Advance ``cur.pos`` per ``(kind, count)`` and return the rows moved over."""
    n = len(cur.rows)
    if kind == "forward":
        start = cur.pos + 1
        end = n if count is None else min(start + count, n)
        rows = cur.rows[start:end] if end > start else []
        cur.pos = end - 1 if rows else n
        return rows
    if kind == "backward":
        start = cur.pos - 1
        stop = -1 if count is None else max(start - count, -1)
        idxs = [i for i in range(start, stop, -1) if 0 <= i < n]
        cur.pos = idxs[-1] if idxs else -1
        return [cur.rows[i] for i in idxs]
    if kind == "absolute":
        if count == 0:
            # ``MOVE ABSOLUTE 0`` positions BEFORE the first row (not at end).
            cur.pos = -1
            return []
        target = count - 1 if count > 0 else n + count  # 1-based; negatives from end
        if 0 <= target < n:
            cur.pos = target
            return [cur.rows[target]]
        cur.pos = -1 if target < 0 else n
        return []
    # relative
    target = cur.pos + count
    if 0 <= target < n:
        cur.pos = target
        return [cur.rows[target]]
    cur.pos = -1 if target < 0 else n
    return []


def _moves_backward(cur: Any, kind: str, count: int | None) -> bool:
    """Whether a ``(kind, count)`` movement scans backward from ``cur.pos`` —
    the check a NO SCROLL cursor rejects."""
    if kind == "backward":
        return count is None or count > 0
    if kind == "absolute":
        n = len(cur.rows)
        if count is None:
            return False
        target = -1 if count == 0 else (count - 1 if count > 0 else n + count)
        return target < cur.pos
    if kind == "relative":
        return (count or 0) < 0
    return False


def _fetch_cursor(stmt: exp.Command, session: Session, *, move: bool = False) -> SQLResult:
    """``FETCH`` returns the moved-over rows; ``MOVE`` performs the same
    positioning but returns only the count (no result set)."""
    kind, count, name = _parse_fetch(_command_tail(stmt))
    cur = session.cursors.get(name)
    if cur is None:
        raise errors.SQLError("34000", f'cursor "{name}" does not exist')
    # A NO SCROLL cursor rejects any movement that would scan backward.
    if cur.scrollable is False and _moves_backward(cur, kind, count):
        raise errors.SQLError("55000", f'cursor "{name}" can only scan forward', position=None)
    rows = _cursor_slice(cur, kind, count)
    verb = "MOVE" if move else "FETCH"
    if move:
        return SQLResult(command_tag=f"{verb} {len(rows)}", rowcount=len(rows))
    return SQLResult(
        command_tag=f"{verb} {len(rows)}", columns=cur.columns, rows=rows, rowcount=len(rows)
    )


def _close_cursor_target(stmt: exp.Expression) -> str | None:
    """A bare ``CLOSE name`` / ``CLOSE ALL`` — parses as a top-level ``Alias``."""
    if not isinstance(stmt, exp.Alias):
        return None
    head = stmt.this
    if isinstance(head, exp.Column) and head.name.upper() == "CLOSE":
        return stmt.alias
    return None


def _close_cursor(name: str, session: Session) -> SQLResult:
    if name.upper() == "ALL":
        session.cursors.clear()
        return SQLResult(command_tag="CLOSE CURSOR")
    if session.cursors.pop(name, None) is None:
        raise errors.SQLError("34000", f'cursor "{name}" does not exist')
    return SQLResult(command_tag="CLOSE CURSOR")


def _close_non_hold_cursors(session: Session) -> None:
    session.cursors = {n: c for n, c in session.cursors.items() if c.hold}


# --------------------------------------------------------------------------- #
# MERGE INTO target USING source ON cond WHEN [NOT] MATCHED THEN <action>.
# For each source row: find the target rows the ON condition matches, then apply
# the first WHEN clause of the right kind (matched / not-matched) whose optional
# AND-condition holds — UPDATE / DELETE / DO NOTHING for a match, INSERT /
# DO NOTHING for a non-match. Matches are taken against the target snapshot at
# MERGE start, and each target row is affected at most once.
# --------------------------------------------------------------------------- #


def _run_merge(
    stmt: exp.Merge, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    from secantus.paths import get_path

    if not isinstance(stmt.this, exp.Table):
        raise errors.feature_not_supported("MERGE target must be a table")
    target = _require_table(catalog, db, planner.qualified_table_name(stmt.this), storage)
    target_alias = (stmt.this.alias or stmt.this.name).lower()
    src_alias, source_rows, source_cols = _merge_source(
        stmt.args["using"], db, catalog, session, storage
    )
    on = stmt.args["on"]
    whens = stmt.args["whens"].expressions
    returning = planner._returning_columns(stmt, target)
    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    target_docs = storage.find_matching(db, target.collection, {})
    affected = 0
    # (image, action, source_row) per affected row for RETURNING — the action
    # feeds ``merge_action()`` and the source row lets RETURNING read source columns.
    affected_rows: list[tuple[dict[str, Any], str, dict[str, Any] | None]] = []
    done: set[int] = set()  # target docs already acted on
    source_matched: set[int] = set()  # target docs some source row matched

    def scope_for(tdoc: dict[str, Any] | None, srow: dict[str, Any] | None):
        def scope(node: Any) -> Any:
            alias = (node.table or "").lower() or None
            name = node.name
            if alias == target_alias or (alias is None and target.column(name) is not None):
                return None if tdoc is None else get_path(tdoc, target.field_for(name))
            if alias == src_alias or alias is None or name in source_cols:
                return srow.get(name) if srow is not None else None
            raise errors.undefined_column(name)

        return scope

    def record(
        result: tuple[int, dict[str, Any] | None],
        when: exp.Expression,
        srow: dict[str, Any] | None,
    ) -> None:
        nonlocal affected
        count, doc = result
        affected += count
        if returning is not None and doc is not None:
            affected_rows.append((doc, _merge_when_action(when), srow))

    for srow in source_rows:
        matched = [
            td
            for td in target_docs
            if scalar._truthy(scalar.evaluate(on, scope_for(td, srow), sctx))
        ]
        for td in matched:
            # Postgres raises cardinality_violation when a target row is matched by
            # more than one source row (it would be affected twice).
            if id(td) in source_matched:
                raise errors.SQLError(
                    "21000",
                    "MERGE command cannot affect row a second time",
                )
            source_matched.add(id(td))
        if matched:
            when = _merge_pick_when(whens, True, False, scope_for(matched[0], srow), sctx)
            if when is None:
                continue
            for td in matched:
                record(
                    _merge_apply_matched(when, target, td, storage, db, scope_for(td, srow), sctx),
                    when,
                    srow,
                )
                done.add(id(td))
        else:
            when = _merge_pick_when(whens, False, False, scope_for(None, srow), sctx)
            if when is not None:
                record(
                    _merge_apply_not_matched(
                        when, target, storage, db, scope_for(None, srow), sctx
                    ),
                    when,
                    srow,
                )

    # WHEN NOT MATCHED BY SOURCE — target rows no source row matched.
    if any(not w.args.get("matched") and w.args.get("source") for w in whens):
        for td in target_docs:
            if id(td) in source_matched or id(td) in done:
                continue
            when = _merge_pick_when(whens, False, True, scope_for(td, None), sctx)
            if when is not None:
                record(
                    _merge_apply_matched(when, target, td, storage, db, scope_for(td, None), sctx),
                    when,
                    None,
                )
                done.add(id(td))

    if returning is not None:
        return _merge_returning_result(affected_rows, returning, target, scope_for, sctx, affected)
    return SQLResult(command_tag=f"MERGE {affected}", rowcount=affected)


def _merge_when_action(when: exp.Expression) -> str:
    """The action a WHEN clause performs, for ``merge_action()``."""
    then = when.args["then"]
    if isinstance(then, exp.Update):
        return "UPDATE"
    if isinstance(then, exp.Insert):
        return "INSERT"
    return "DELETE"  # the only other row-producing action (DO NOTHING records nothing)


def _merge_returning_result(
    affected_rows: list[tuple[dict[str, Any], str, dict[str, Any] | None]],
    returning: list[tuple[str, Any, Any]],
    target: TableDef,
    scope_for: Any,
    sctx: Any,
    affected: int,
) -> SQLResult:
    """Shape a MERGE ``RETURNING`` — like a write's, but ``merge_action()`` resolves
    to the per-row action and other expressions evaluate against a scope that sees
    both the target image and the source row (so ``s.col`` works)."""
    from secantus.paths import get_path
    from secantus.sql.executor import _out_column_descs

    columns = _out_column_descs([(name, col) for name, col, _ in returning], sctx.storage, sctx.db)

    def cell(doc: dict[str, Any], action: str, srow: Any, col: Any, expr: Any) -> Any:
        if expr is None:
            return typemap.to_py(get_path(doc, col.field), col.type_tag)
        if isinstance(expr, exp.Anonymous) and str(expr.this).lower() == "merge_action":
            return action
        return typemap.to_py(scalar.evaluate(expr, scope_for(doc, srow), sctx), col.type_tag)

    rows = [
        tuple(cell(doc, action, srow, col, expr) for _, col, expr in returning)
        for doc, action, srow in affected_rows
    ]
    return SQLResult(command_tag=f"MERGE {affected}", columns=columns, rows=rows, rowcount=affected)


def _merge_source(
    using: exp.Expression, db: str, catalog: Catalog, session: Session, storage: Any
) -> tuple[str, list[dict[str, Any]], set[str]]:
    """Materialize the MERGE source into (alias, rows-by-column-name, column set).
    The source is a table / reflected collection or a ``(SELECT …) alias``."""
    from secantus.paths import get_path

    if isinstance(using, exp.Subquery):
        res = _run_query(using.this, storage, db, catalog, session)
        cols = [c.name for c in res.columns]
        rows = [dict(zip(cols, r, strict=True)) for r in res.rows]
        return (using.alias or "").lower(), rows, set(cols)
    if not isinstance(using, exp.Table):
        raise errors.feature_not_supported(f"unsupported MERGE source: {using.sql()}")
    tdef = catalog.get(db, using.name) or reflect.reflect(storage, db, using.name)
    if tdef is None:
        raise errors.undefined_table(using.name)
    docs = storage.find_matching(db, tdef.collection, {})
    rows = [{c.name: get_path(d, c.field) for c in tdef.columns} for d in docs]
    return (using.alias or using.name).lower(), rows, {c.name for c in tdef.columns}


# --------------------------------------------------------------------------- #
# Join DML: DELETE ... USING and UPDATE ... SET ... FROM
# --------------------------------------------------------------------------- #


def _collect_dml_sources(
    tables: list[exp.Expression], db: str, catalog: Catalog, session: Session, storage: Any
) -> list[tuple[str, list[dict[str, Any]], set[str]]]:
    """Materialize the USING / FROM tables of a join DML into ``(alias, rows,
    cols)`` triples. Each entry may itself carry comma / JOIN-chained tables."""
    out: list[tuple[str, list[dict[str, Any]], set[str]]] = []
    for node in tables:
        chain = [node] + [j.this for j in (node.args.get("joins") or [])]
        for t in chain:
            out.append(_merge_source(t, db, catalog, session, storage))
    return out


def _dml_join_scope(
    tdoc: dict[str, Any] | None,
    binding: dict[str, dict[str, Any]],
    target: TableDef,
    target_alias: str,
    source_cols: dict[str, set[str]],
):
    """A scope resolving a column against the target row (``tdoc``) or a source
    binding (alias → source row). Unqualified names prefer the target."""
    from secantus.paths import get_path

    def scope(node: Any) -> Any:
        alias = (node.table or "").lower() or None
        name = node.name
        if alias == target_alias or (alias is None and target.column(name) is not None):
            return None if tdoc is None else get_path(tdoc, target.field_for(name))
        if alias is not None and alias in binding:
            return binding[alias].get(name)
        if alias is None:
            for a, cols in source_cols.items():
                if name in cols:
                    return binding[a].get(name)
        raise errors.undefined_column(name)

    return scope


def _dml_join_matches(
    target_docs: list[dict[str, Any]],
    sources: list[tuple[str, list[dict[str, Any]], set[str]]],
    where: exp.Expression | None,
    target: TableDef,
    target_alias: str,
    sctx: Any,
) -> list[tuple[dict[str, Any], dict[str, dict[str, Any]]]]:
    """Each target row that joins at least one combination of source rows for which
    the WHERE holds, paired with the *first* such binding (Postgres leaves which
    source row wins unspecified when several match)."""
    import itertools

    aliases = [a for a, _, _ in sources]
    row_lists = [rows for _, rows, _ in sources]
    source_cols = {a: cols for a, _, cols in sources}
    matches: list[tuple[dict[str, Any], dict[str, dict[str, Any]]]] = []
    for tdoc in target_docs:
        for combo in itertools.product(*row_lists):
            binding = dict(zip(aliases, combo, strict=True))
            scope = _dml_join_scope(tdoc, binding, target, target_alias, source_cols)
            if where is None or scalar._truthy(scalar.evaluate(where, scope, sctx)):
                matches.append((tdoc, binding))
                break
    return matches


def _run_delete_using(
    stmt: exp.Delete, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``DELETE FROM t USING u [, v …] WHERE …`` — delete each target row that
    joins a source row satisfying the WHERE (a semi-join)."""
    target = _require_table(catalog, db, planner.qualified_table_name(stmt.this), storage)
    target_alias = (stmt.this.alias or stmt.this.name).lower()
    sources = _collect_dml_sources(stmt.args["using"], db, catalog, session, storage)
    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    where = stmt.args.get("where")
    target_docs = storage.find_matching(db, target.collection, {})
    victims = [
        tdoc
        for tdoc, _ in _dml_join_matches(
            target_docs, sources, where.this if where else None, target, target_alias, sctx
        )
    ]
    if catalog is not None and not getattr(target, "reflected", False):
        executor.enforce_parent_delete(victims, target, storage, db, catalog)
    for tdoc in victims:
        storage.delete_matching(db, target.collection, {"_id": tdoc["_id"]})
    n = len(victims)
    returning = planner._returning_columns(stmt, target)
    if returning is not None:
        return executor._returning_result(victims, returning, f"DELETE {n}", n, target, storage, db)
    return SQLResult(command_tag=f"DELETE {n}", rowcount=n)


def _targets_pg_description(stmt: exp.Expression, catalog: Catalog, db: str) -> bool:
    """A DML statement writing the ``pg_description`` virtual relation (bare or
    ``pg_catalog``-qualified), unless a real user table shadows the name."""
    node = stmt.this if isinstance(stmt.this, exp.Table) else stmt.find(exp.Table)
    if node is None or node.name.lower() != "pg_description":
        return False
    schema = node.args.get("db")
    if schema is not None and schema.name.lower() != "pg_catalog":
        return False
    return schema is not None or catalog.get(db, "pg_description") is None


def _run_pg_description_dml(
    stmt: exp.Update | exp.Delete, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """UPDATE / DELETE against ``pg_description``, persisted as a delta over the
    derived comment rows (see ``virtual._pg_description``).

    Real PG lets a superuser edit the catalog directly; DatabaseMetaDataTest's
    setup moves a function comment onto a table's oid to manufacture a
    duplicate-description row and prove the metadata queries' classoid guards.
    Matched rows are suppressed by key and re-emitted with the assignments
    applied (UPDATE) or just suppressed (DELETE)."""
    from secantus.sql.catalog import DESCRIPTION_DELTA_COLLECTION

    rows = virtual._pg_description(db, session, storage, catalog)
    where = stmt.args.get("where")
    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)

    def scope_for(row: dict[str, Any]):
        def scope(node: Any) -> Any:
            if node.name in row:
                return row[node.name]
            raise errors.SQLError("42703", f'column "{node.name}" does not exist')

        return scope

    matched: list[dict[str, Any]] = []
    for row in rows:
        if where is None or scalar._truthy(scalar.evaluate(where.this, scope_for(row), sctx)):
            matched.append(row)
    docs: list[dict[str, Any]] = []
    for row in matched:
        key = f"{row['objoid']}/{row['classoid']}/{row['objsubid']}"
        docs.append({"_id": f"s/{key}", "kind": "suppress", "key": key})
        if isinstance(stmt, exp.Update):
            new_row = dict(row)
            for assign in stmt.expressions:
                col = assign.this.name
                if col not in new_row:
                    raise errors.SQLError("42703", f'column "{col}" does not exist')
                value = scalar.evaluate(assign.expression, scope_for(row), sctx)
                new_row[col] = int(value) if col != "description" else value
            new_key = f"{new_row['objoid']}/{new_row['classoid']}/{new_row['objsubid']}"
            docs.append(
                {
                    "_id": f"e/{new_key}",
                    "kind": "extra",
                    "objoid": new_row["objoid"],
                    "classoid": new_row["classoid"],
                    "objsubid": new_row["objsubid"],
                    "description": new_row["description"],
                }
            )
        else:
            # DELETE also drops a previously-inserted extra row with this key.
            storage.delete_matching(db, DESCRIPTION_DELTA_COLLECTION, {"_id": f"e/{key}"})
    for doc in docs:
        storage.delete_matching(db, DESCRIPTION_DELTA_COLLECTION, {"_id": doc["_id"]})
    if docs:
        storage.insert(db, DESCRIPTION_DELTA_COLLECTION, docs)
    verb = "UPDATE" if isinstance(stmt, exp.Update) else "DELETE"
    return SQLResult(command_tag=f"{verb} {len(matched)}", rowcount=len(matched))


def _run_update_from(
    stmt: exp.Update, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``UPDATE t SET … FROM u [, v …] WHERE …`` — update each target row that joins
    a source row satisfying the WHERE; the SET right-hand sides may reference the
    source (``SET col = u.col``)."""
    target_node = stmt.this
    target = _require_table(catalog, db, planner.qualified_table_name(target_node), storage)
    target_alias = (target_node.alias or target_node.name).lower()
    from_node = stmt.args["from_"]
    sources = _collect_dml_sources([from_node.this], db, catalog, session, storage)
    source_cols = {a: cols for a, _, cols in sources}
    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    where = stmt.args.get("where")
    target_docs = storage.find_matching(db, target.collection, {})
    matches = _dml_join_matches(
        target_docs, sources, where.this if where else None, target, target_alias, sctx
    )
    returning = planner._returning_columns(stmt, target)
    updated: list[dict[str, Any]] = []
    for tdoc, binding in matches:
        scope = _dml_join_scope(tdoc, binding, target, target_alias, source_cols)
        set_doc: dict[str, Any] = {}
        for eq in stmt.expressions:
            col = eq.this.name
            set_doc[target.field_for(col)] = typemap.coerce(
                scalar.evaluate(eq.expression, scope, sctx), target.type_for(col)
            )
        post = {**tdoc, **set_doc}
        executor.enforce_update_images([post], [tdoc["_id"]], target, storage, db, catalog, session)
        storage.update_matching(db, target.collection, {"_id": tdoc["_id"]}, {"$set": set_doc})
        updated.append(post)
    n = len(updated)
    if returning is not None:
        return executor._returning_result(updated, returning, f"UPDATE {n}", n, target, storage, db)
    return SQLResult(command_tag=f"UPDATE {n}", rowcount=n)


def _merge_pick_when(
    whens: list[exp.Expression], matched: bool, by_source: bool, scope: Any, sctx: Any
) -> exp.Expression | None:
    """The first WHEN clause of the right kind (``matched`` / not, and — for a
    non-match — WHEN NOT MATCHED BY SOURCE vs BY TARGET) whose optional
    ``AND``-condition holds."""
    for w in whens:
        if bool(w.args.get("matched")) != matched or bool(w.args.get("source")) != by_source:
            continue
        cond = w.args.get("condition")
        if cond is None or scalar._truthy(scalar.evaluate(cond, scope, sctx)):
            return w
    return None


def _merge_apply_matched(
    when: exp.Expression, target: TableDef, td: dict[str, Any], storage: Any, db: str, scope, sctx
) -> tuple[int, dict[str, Any] | None]:
    """Apply an UPDATE / DELETE / DO NOTHING to the matched (or not-matched-by-
    source) target row ``td``. Returns ``(rows_affected, image)`` where ``image``
    is the post-update / pre-delete row for a RETURNING projection (None if no
    row changed)."""
    then = when.args["then"]
    if isinstance(then, exp.Update):
        import copy

        from secantus.paths import set_path

        set_doc: dict[str, Any] = {}
        for eq in then.expressions:
            col = eq.this.name
            set_doc[target.field_for(col)] = typemap.coerce(
                scalar.evaluate(eq.expression, scope, sctx), target.type_for(col)
            )
        if not set_doc:
            return 1, td
        # A SET that touches a primary-key column (``_id`` / ``_id.<name>``) can't
        # go through ``$set`` — ``_id`` is immutable — so the row is re-keyed
        # (delete + re-insert), mirroring the plain UPDATE path (#157). Non-PK sets
        # take the in-place ``$set`` fast path.
        id_sets = {k: v for k, v in set_doc.items() if k == "_id" or k.startswith("_id.")}
        other_sets = {k: v for k, v in set_doc.items() if k not in id_sets}
        post = copy.deepcopy(td)
        for k, v in {**other_sets, **id_sets}.items():
            set_path(post, k, v)
        executor.enforce_update_images(
            [post], [td["_id"]], target, storage, db, sctx.catalog, sctx.session
        )
        # Parent-side FK actions (RESTRICT / CASCADE / SET NULL) when a referenced
        # column changed — enforce_update_images leaves these to the caller.
        executor._enforce_fk_on_parent_update([td], [post], target, storage, db, sctx.catalog)
        if id_sets:
            if post["_id"] != td["_id"] and storage.find_matching(
                db, target.collection, {"_id": post["_id"]}
            ):
                raise errors.SQLError(
                    "23505",
                    "duplicate key value violates unique constraint "
                    f'"{target.pk_constraint_name()}"',
                )
            storage.delete_matching(db, target.collection, {"_id": td["_id"]})
            storage.insert(db, target.collection, [post])
        else:
            storage.update_matching(db, target.collection, {"_id": td["_id"]}, {"$set": other_sets})
        return 1, post
    action = then.sql().strip().upper()
    if action == "DELETE":
        executor.enforce_parent_delete([td], target, storage, db, sctx.catalog)
        storage.delete_matching(db, target.collection, {"_id": td["_id"]})
        return 1, td
    if action == "DO NOTHING":
        return 0, None
    raise errors.feature_not_supported(f"unsupported MERGE matched action: {then.sql()}")


def _merge_apply_not_matched(
    when: exp.Expression, target: TableDef, storage: Any, db: str, scope, sctx
) -> tuple[int, dict[str, Any] | None]:
    import bson

    then = when.args["then"]
    if not isinstance(then, exp.Insert):
        if then.sql().strip().upper() == "DO NOTHING":
            return 0, None
        raise errors.feature_not_supported(f"unsupported MERGE not-matched action: {then.sql()}")
    col_node = then.this
    cols = (
        [c.name for c in col_node.expressions]
        if col_node is not None
        else [c.name for c in target.columns]
    )
    values = then.expression.expressions if then.expression is not None else []
    if len(values) != len(cols):
        raise errors.SQLError("42601", "MERGE INSERT has mismatched column and value counts")
    from secantus.paths import set_path

    doc: dict[str, Any] = {}
    for col, vexpr in zip(cols, values, strict=True):
        # A composite-PK column's field is a dotted ``_id.<name>`` path — set_path
        # builds the ``_id`` subdocument instead of a flat ``_id.a`` key.
        set_path(
            doc,
            target.field_for(col),
            typemap.coerce(scalar.evaluate(vexpr, scope, sctx), target.type_for(col)),
        )
    doc.setdefault("_id", bson.ObjectId())
    executor.enforce_insert_rows([doc], target, storage, db, sctx.catalog, sctx.session)
    storage.insert(db, target.collection, [doc])
    return 1, doc


def run_statement(
    storage: Any,
    db: str,
    stmt: exp.Expression,
    session: Session,
    catalog: Catalog | None = None,
) -> SQLResult:
    """Execute a single already-parsed AST statement.

    The extended-protocol path (Parse/Bind/Execute) parses once and binds
    parameters into the AST, then drives this rather than re-parsing SQL text.
    """
    if catalog is None:
        catalog = Catalog(storage)
    with _storage_conflicts_as_sqlstate():
        result = _normalize_result(_dispatch(stmt, storage, db, catalog, session))
    _drain_plpgsql_notices(session, result)
    return result


def describe_statement(
    storage: Any, db: str, stmt: exp.Expression, session: Session, catalog: Catalog
) -> list[ColumnDesc] | None:
    """Resolve a statement's result columns WITHOUT executing it (extended-protocol
    Describe). When a transaction is open the planning reads must run *inside* it,
    so Describe sees the connection's own uncommitted DDL — otherwise a
    parameterised SELECT against a table CREATEd in the same uncommitted
    transaction describes as NoData while Execute (which runs in the transaction)
    emits DataRows, a protocol violation that crashes the client. Read-only:
    Describe never writes."""
    if session.txn_handle is not None:
        with storage.use_user_transaction(session.txn_handle):
            return _describe_statement(storage, db, stmt, session, catalog)
    return _describe_statement(storage, db, stmt, session, catalog)


def _describe_statement(
    storage: Any, db: str, stmt: exp.Expression, session: Session, catalog: Catalog
) -> list[ColumnDesc] | None:
    if not isinstance(stmt, exp.Command):
        # Describe plans without executing, so it needs the same search_path /
        # temp-namespace resolution _run_statement applies at execute time —
        # else a SELECT on a temp table describes as NoData while Execute
        # emits DataRows (a protocol violation).
        planner.qualify_from_search_path(stmt, catalog, db, session)
    if isinstance(stmt, exp.Command) and str(stmt.this).upper() == "FETCH":
        # A binary server cursor's ``FETCH … FROM <name>`` rides the extended
        # protocol; Describe must report the cursor's columns (else Execute
        # sends DataRows without a prior RowDescription — a protocol violation).
        try:
            _kind, _count, cname = _parse_fetch(_command_tail(stmt))
        except errors.SQLError:
            return None
        cur = session.cursors.get(cname)
        return list(cur.columns) if cur is not None else None
    if isinstance(stmt, exp.Command) and str(stmt.this).upper() == "MOVE":
        return None  # MOVE returns no rows
    if isinstance(stmt, exp.Command) and str(stmt.this).upper() == "CALL":
        # A CALL portal describes as its procedure's OUT/INOUT params (a single
        # RowDescription), or NoData when it has none — WITHOUT running the body
        # (so a procedure that COMMITs internally emits no stray RowDescriptions;
        # pgjdbc's #158771).
        return _call_out_columns(_command_tail(stmt), db, catalog)
    if isinstance(stmt, exp.Command) and str(stmt.this).upper() == "EXECUTE":
        # ``EXECUTE name(args)`` through the extended protocol: Describe must
        # report the UNDERLYING prepared statement's shape — a SELECT's
        # RowDescription, not NoData (pgtest execute:70).
        m = _EXECUTE_TAIL.match(_command_tail(stmt))
        entry = session.prepared.get(_unquote_ident(m.group("name"))) if m else None
        if entry is None:
            return None
        query, _count = entry
        args = _execute_args(m.group("args"))
        try:
            bound = _bind_parameter_nodes(query, args)
        except errors.SQLError:
            bound = query.copy()
        return _describe_statement(storage, db, bound, session, catalog)
    if isinstance(stmt, exp.Command) and str(stmt.this).upper() == "SHOW":
        oid = typemap.PG_OID["text"]
        if _show_name(stmt).upper() == "ALL":
            return [
                ColumnDesc("name", "text", oid),
                ColumnDesc("setting", "text", oid),
                ColumnDesc("description", "text", oid),
            ]
        return [ColumnDesc(_show_name(stmt), "text", oid)]
    with_node = _own_with(stmt)
    if with_node is not None:
        # A CTE query's column shape comes from its outer SELECT, planned
        # against synthetic TableDefs for the CTE names (each CTE's own
        # output shape, resolved recursively) — planning only, nothing is
        # materialized. Reporting NoData here and then emitting DataRows at
        # Execute is a protocol violation: pgjdbc rejects it outright
        # ("Received resultset tuples, but no field structure for them"),
        # and a data-modifying CTE (``WITH x AS (INSERT … RETURNING …)
        # SELECT * FROM x``) hits exactly that. Undescribable CTEs still
        # fall back to NoData.
        shape = _describe_with(storage, db, stmt, session, catalog, with_node)
        if shape is not None:
            return shape
        return None
    if isinstance(stmt, exp.SetOperation):
        # A set operation's result shape is its first arm's (descend chained
        # ops; a parenthesized arm parses as a Subquery wrapper).
        arm = stmt
        while isinstance(arm, exp.SetOperation):
            arm = arm.left
        if isinstance(arm, exp.Subquery) and arm.this is not None:
            arm = arm.this
        return _describe_statement(storage, db, arm, session, catalog)
    if isinstance(stmt, (exp.Insert, exp.Update, exp.Delete, exp.Merge)):
        # A DML statement with RETURNING emits DataRows at Execute, so Describe
        # must answer with their RowDescription — a NoData here followed by "D"
        # messages is a protocol violation that crashes libpq clients
        # (psycopg's pipelined executemany was the first to hit it).
        return _describe_returning(storage, db, catalog, stmt)
    if isinstance(stmt, exp.Values):
        # A bare ``VALUES (…)`` emits DataRows at Execute — same protocol rule
        # as RETURNING: NoData followed by "D" crashes libpq's stream mode. The
        # cells are constant expressions, so deriving the shape is side-effect
        # free; unbound placeholders defer to Execute's row description.
        try:
            return list(_run_values(stmt, storage, db, catalog, session).columns)
        except (errors.SQLError, TypeError, ValueError):
            return None
    if not isinstance(stmt, exp.Select):
        return None
    # A SELECT from a declared view describes as the expanded subquery —
    # without this, Describe answers NoData while Execute (which expands)
    # emits DataRows: a protocol violation libpq clients reject. Expand a
    # copy: the prepared statement's stored AST must stay pristine.
    if not isinstance(catalog, _CTECatalog) and stmt.find(exp.Table) is not None:
        expanded = stmt.copy()
        _expand_views(expanded, catalog, db)
        stmt = expanded
    table_node = stmt.find(exp.Table)
    planner.rewrite_pg_typeof(stmt, _pg_typeof_table(storage, db, catalog, table_node))
    # A set-returning row source (``FROM generate_series(…)`` / a bare
    # ``SELECT generate_series(…)``) — the constant-select planner would
    # evaluate the SRF as a scalar and error. Derive the column shape the same
    # way execution does; a shape that needs bound parameters defers to
    # Execute's row description (NoData).
    srf_source = srf.from_source(stmt) or srf.fromless_projection(stmt)
    if srf_source is not None:
        sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
        try:
            _rows, tdef = srf.build(srf_source, sctx, describe_only=True)
            # Mirror _run_srf_select: the result shape is the outer projection
            # planned over the synthetic SRF table, not the SRF's raw columns.
            query = stmt
            if stmt.args.get("from_") is None:
                query = stmt.copy()
                query.set("from", exp.From(this=exp.Table(this=exp.to_identifier(tdef.name))))
                query.set("expressions", [exp.column(tdef.columns[0].name)])
            if planner._stmt_needs_evaluation(query):
                eval_plan = planner._build_evaluated_single(query, tdef)
                return executor._tagged_out_column_descs(
                    eval_plan.out_columns, eval_plan.out_enum_types, storage, db
                )
            srf_plan = planner.plan_select(query, tdef)
        except (errors.SQLError, TypeError, ValueError):
            return None
        if srf_plan.count_star:
            return [ColumnDesc(srf_plan.count_alias, "int8", typemap.PG_OID["int8"])]
        return executor._out_column_descs(srf_plan.out_columns, storage, db)
    if table_node is None and stmt.args.get("from_") is not None:
        # A FROM of derived tables only — ``(VALUES …) AS t1 (…) LEFT JOIN
        # (VALUES …) AS t2 (…)`` (pgjdbc's OuterJoinSyntaxTest; CrystalReports
        # emits this via the {oj} escape). No ``exp.Table`` anywhere, so the
        # branches above skipped it and the constant path below errors —
        # Describe answered NoData while Execute emitted DataRows, a protocol
        # violation pgjdbc rejects. Planning is side-effect-free (derived
        # tables materialize at execution), so derive the shape from the plan.
        try:
            desugared = stmt.copy()
            planner.desugar_join_using(desugared)
            plan = planner.plan_pipeline_select(desugared, db, catalog, storage, session=session)
            if getattr(plan, "count_star", False):
                return [ColumnDesc(plan.count_alias, "int8", typemap.PG_OID["int8"])]
            return executor._tagged_out_column_descs(
                plan.out_columns,
                getattr(plan, "out_enum_types", None) or {},
                storage,
                db,
                out_exprs=getattr(plan, "out_exprs", None),
                base_table=getattr(plan, "base_table", None),
                out_sources=getattr(plan, "out_sources", None) or None,
            )
        except (errors.SQLError, TypeError, ValueError, AttributeError):
            return None
    if table_node is None or stmt.args.get("from_") is None:
        # find() descends into subqueries — a FROM-less outer SELECT (WHERE
        # EXISTS …) describes via the constant path, same as _run_select.
        # Volatile calls must NOT be evaluated at Describe time: the constant
        # planner evaluates the projection, so ``Describe select pg_sleep(5)``
        # SLEPT (and a cancel arriving mid-sleep was swallowed into NoData
        # while Execute later emitted a DataRow — the protocol violation
        # pgjdbc's setQueryTimeout tests crashed on), and ``nextval`` would
        # draw a sequence value. Derive their shapes statically instead.
        if _volatile_call_shape(stmt) is not None:
            return _volatile_call_shape(stmt)
        try:
            plan = planner.plan_constant_select(stmt, session, storage, catalog, db)
        except errors.SQLError:
            # Describe must not need parameter VALUES. ``SELECT $1::inet`` (a
            # bound NULL cast — pgjdbc's PGobject round-trip) has a shape fixed
            # entirely by the cast target, but evaluating it pre-Bind raises.
            # Fall back to a value-free shape derivation; anything that still
            # can't be typed defers to Execute (NoData).
            return _describe_constant_shape(stmt)
        oids = plan.pg_oids or [None] * len(plan.columns)
        typmods = plan.typmods or [-1] * len(plan.columns)
        return [
            ColumnDesc(n, t, oid if oid is not None else typemap.PG_OID.get(t, 25), typmod)
            for (n, t, _), oid, typmod in zip(plan.columns, oids, typmods, strict=True)
        ]
    if planner.select_needs_pipeline(stmt):
        pplan = planner.plan_pipeline_select(stmt, db, catalog, storage)
        return executor._tagged_out_column_descs(
            pplan.out_columns,
            pplan.out_enum_types,
            storage,
            db,
            out_exprs=getattr(pplan, "out_exprs", None),
            base_table=getattr(pplan, "base_table", None),
            out_sources=getattr(pplan, "out_sources", None) or None,
        )
    schema = table_node.args.get("db")
    schema_name = schema.name if schema is not None else None
    vtable = virtual.lookup(schema_name, table_node.name)
    if vtable is not None:
        table = vtable.table_def()
    else:
        _qn = planner.qualified_table_name(table_node)
        table = catalog.get(db, _qn) or reflect.reflect(storage, db, _qn)
    if table is None:
        return None  # undefined table — let Execute raise the real error
    try:
        select_plan = planner.plan_select(stmt, table)
    except errors.SQLError:
        # A WHERE the pushdown can't lower is routed to per-row evaluation at
        # Execute — Describe must not fail on it. The result shape doesn't
        # depend on the WHERE, so re-plan without it; a statement that still
        # can't plan defers to Execute (NoData).
        bare = stmt.copy()
        bare.set("where", None)
        try:
            select_plan = planner.plan_select(bare, table)
        except errors.SQLError:
            return None
    if select_plan.count_star:
        return [ColumnDesc(select_plan.count_alias, "int8", typemap.PG_OID["int8"])]
    # The table is passed so RowDescription carries each column's source table
    # oid and attnum. This is the path the extended protocol describes through,
    # which is the one a JDBC updatable ResultSet reads to resolve column names.
    return executor._out_column_descs(select_plan.out_columns, storage, db, table)


#: Result type tags of volatile / blocking session functions, for Describe:
#: evaluating them at Describe time would sleep, draw sequence values, or
#: take locks. Shapes here mirror what Execute actually returns.
_VOLATILE_FN_TAGS = {
    "pg_sleep": "text",
    "nextval": "int8",
    "setval": "int8",
    "currval": "int8",
    "lastval": "int8",
    "set_config": "text",
    "pg_terminate_backend": "bool",
    "pg_cancel_backend": "bool",
    "pg_advisory_lock": "text",
    "pg_advisory_unlock": "bool",
    "pg_try_advisory_lock": "bool",
    "lo_creat": "oid",
    "lo_create": "oid",
    "lo_unlink": "int4",
}


def _volatile_call_shape(stmt: exp.Select) -> list[ColumnDesc] | None:
    """When any projection calls a volatile function, the whole statement's
    shape derived statically (None when no volatile call is present, or when
    a projection mixes one into an expression we can't type)."""
    calls_volatile = False
    out: list[ColumnDesc] = []
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        while isinstance(target, exp.Paren):
            target = target.this
        if isinstance(target, exp.Dot) and isinstance(target.expression, exp.Anonymous):
            target = target.expression
        if isinstance(target, exp.Anonymous):
            fname = str(target.this).rsplit(".", 1)[-1].lower()
            if fname in _VOLATILE_FN_TAGS:
                calls_volatile = True
                tag = _VOLATILE_FN_TAGS[fname]
                out.append(ColumnDesc(alias or fname, tag, typemap.PG_OID.get(tag, 25)))
                continue
        if any(
            str(f.this).rsplit(".", 1)[-1].lower() in _VOLATILE_FN_TAGS
            for f in target.find_all(exp.Anonymous)
        ):
            return None  # volatile call nested in an untypeable expression
        out.append(None)  # placeholder: typed below only if needed
    if not calls_volatile:
        return None
    # Type the non-volatile projections without evaluating: literals and casts.
    for i, e in enumerate(stmt.expressions):
        if out[i] is not None:
            continue
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        while isinstance(target, exp.Paren):
            target = target.this
        if isinstance(target, exp.Literal):
            if target.is_string:
                out[i] = ColumnDesc(alias or "?column?", "text", 705)
            else:
                text = str(target.this)
                tag = "numeric" if "." in text else "int4"
                out[i] = ColumnDesc(alias or "?column?", tag, typemap.PG_OID[tag])
        elif isinstance(target, exp.Cast) and target.to is not None:
            tag = typemap.type_tag_for_sql(target.to) or "text"
            out[i] = ColumnDesc(
                alias or planner._cast_output_name(target) or "?column?",
                tag,
                typemap.PG_OID.get(tag, 25),
            )
        else:
            return None
    return out  # type: ignore[return-value]


def _describe_constant_shape(stmt: exp.Select) -> list[ColumnDesc] | None:
    """Column shape of a FROM-less SELECT derived WITHOUT evaluating it — used
    when a projection references an unbound parameter. Only casts (whose target
    fixes the type) and plain literals are typed; anything else makes the whole
    statement undescribable (None -> NoData, and Execute answers)."""
    out: list[ColumnDesc] = []
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        while isinstance(target, exp.Paren):
            target = target.this
        if not isinstance(target, exp.Cast) or target.to is None:
            return None
        tag = typemap.type_tag_for_sql(target.to)
        ident = typemap.cast_type_identity(target.to)
        if ident is None and tag is None:
            return None
        oid = ident[0] if ident is not None else typemap.PG_OID.get(tag or "text", 25)
        typmod = ident[1] if ident is not None else -1
        name = alias or planner._cast_output_name(target) or "?column?"
        out.append(ColumnDesc(name, tag or "text", oid, typmod))
    return out or None


def _describe_with(
    storage: Any,
    db: str,
    stmt: exp.Expression,
    session: Session,
    catalog: Catalog,
    with_node: exp.With,
) -> list[ColumnDesc] | None:
    """Result columns of a ``WITH … SELECT`` — the outer query described
    against synthetic tables standing in for the CTEs. Side-effect free: a
    data-modifying CTE is described by its RETURNING shape, never run."""
    defs: dict[str, Any] = {}
    for cte in with_node.expressions:
        alias = cte.alias
        inner = cte.this
        if isinstance(inner, exp.Subquery):
            inner = inner.this
        try:
            if isinstance(inner, (exp.Insert, exp.Update, exp.Delete, exp.Merge)):
                cols = _describe_returning(storage, db, catalog, inner)
            elif isinstance(inner, (exp.Select, exp.SetOperation)):
                cols = _describe_statement(storage, db, inner, session, catalog)
            else:
                cols = None
        except errors.SQLError:
            cols = None
        if not cols:
            return None
        # A ``name(a, b)`` column-alias list renames the CTE's outputs.
        aliases = (
            [c.name for c in (cte.args.get("alias").columns or [])] if cte.args.get("alias") else []
        )
        names = aliases or [c.name for c in cols]
        if len(names) != len(cols):
            return None
        defs[alias] = TableDef(
            name=alias,
            collection=alias,
            columns=[
                Column(n, c.type_tag, n, pk=False, nullable=True)
                for n, c in zip(names, cols, strict=True)
            ],
        )
    if not defs:
        return None
    body = stmt.copy()
    # The WITH arg key varies by sqlglot version (``with`` / ``with_``);
    # clear it by identity, like _own_with finds it. Leaving it set makes
    # the recursive describe below re-enter this function forever.
    for key, value in list(body.args.items()):
        if isinstance(value, exp.With):
            body.set(key, None)
    try:
        return _describe_statement(storage, db, body, session, _CTEDescribeCatalog(catalog, defs))
    except errors.SQLError:
        return None


class _CTEDescribeCatalog:
    """Read-only catalog view that resolves CTE names to their synthetic
    TableDefs and delegates everything else to the real catalog."""

    def __init__(self, base: Catalog, defs: dict[str, Any]) -> None:
        self._base = base
        self._defs = defs

    def get(self, db: str, name: str) -> Any:
        hit = self._defs.get(name)
        return hit if hit is not None else self._base.get(db, name)

    def __getattr__(self, item: str) -> Any:
        return getattr(self._base, item)


def _show_name(stmt: exp.Command) -> str:
    arg = stmt.expression
    if isinstance(arg, exp.Literal):
        return str(arg.this).strip()
    return str(arg.name).strip() if arg is not None else ""


def _require_table(catalog: Catalog, db: str, name: str, storage: Any = None) -> Any:
    table = catalog.get(db, name)
    if table is None and storage is not None:
        # Schema-on-read: a write to an un-declared collection reflects its
        # sampled shape, so INSERT/UPDATE/DELETE reach Mongo data with no DDL.
        table = reflect.reflect(storage, db, name)
    if table is None:
        raise errors.undefined_table(name)
    return table


# --------------------------------------------------------------------------- #
# COPY … FROM/TO STDIN/STDOUT — the wire server drives the streaming; these
# helpers plan the target and convert between copy-stream cells and rows.
# --------------------------------------------------------------------------- #


@dataclass
class CopyPlan:
    table: Any
    columns: list[str]  # target column SQL names (or query output names)
    to_stdout: bool  # True = COPY TO STDOUT, False = COPY FROM STDIN
    fmt: str  # "text" | "csv" | "binary"
    delimiter: str
    null: str
    header: bool
    #: CSV ESCAPE character (None = PG default: the quote char, i.e. "" doubling).
    escape: str | None = None
    #: CSV QUOTE character (None = PG default double-quote).
    quote: str | None = None
    # For ``COPY (SELECT …) TO STDOUT``: the pre-rendered copy-stream cells of the
    # query result (query-form COPY is dump-only; ``table`` is None).
    query_rows: list[list] | None = None
    # Per-column type tags + oids (parallel to ``columns``) — the binary format
    # encodes/decodes each field by its type instead of rendering text.
    col_tags: list[str] = field(default_factory=list)
    col_oids: list[int] = field(default_factory=list)
    # Raw (unrendered) query-form values, kept for binary COPY OUT.
    query_raw_rows: list | None = None


def copy_plan(
    stmt: exp.Copy, storage: Any, db: str, catalog: Catalog, session: Session | None = None
) -> CopyPlan:
    """Resolve a ``COPY`` statement to a :class:`CopyPlan`. Only ``STDIN`` /
    ``STDOUT`` are supported (no server-side file access). ``COPY (query) TO
    STDOUT`` runs the query and dumps its result (query-form COPY)."""
    files = stmt.args.get("files") or []
    target = files[0].name.upper() if files else ""
    if target not in ("STDIN", "STDOUT"):
        raise errors.feature_not_supported("COPY only supports STDIN / STDOUT")
    # COPY rides the wire server's own path, not _run_statement, so it must
    # resolve search_path / the session's temp namespace itself — ``COPY foo
    # FROM STDIN`` after ``CREATE TEMP TABLE foo`` targets the temp table.
    if session is not None:
        planner.qualify_from_search_path(stmt, catalog, db, session)
    to_stdout = not bool(stmt.args.get("kind"))  # kind True = FROM, False = TO
    fmt, delimiter, null, header, escape, quote = _copy_options(stmt)
    if delimiter is None:
        delimiter = "," if fmt == "csv" else "\t"
    if null is None:
        null = "" if fmt == "csv" else "\\N"

    this = stmt.this
    if isinstance(this, (exp.Subquery, exp.Select)):
        if not to_stdout:
            raise errors.syntax_error("COPY (query) must be COPY … TO, not FROM")
        select = this.this if isinstance(this, exp.Subquery) else this
        columns, query_rows, raw_rows, tags, oids = _copy_query_rows(
            select, storage, db, catalog, session, render_text=(fmt != "binary")
        )
        return CopyPlan(
            None,
            columns,
            True,
            fmt,
            delimiter,
            null,
            header,
            escape=escape,
            quote=quote,
            query_rows=query_rows,
            query_raw_rows=raw_rows,
            col_tags=tags,
            col_oids=oids,
        )

    if isinstance(this, exp.Schema):
        tname = planner.qualified_table_name(this.this)
        columns = [c.name for c in this.expressions]
    else:
        tname = planner.qualified_table_name(this) if isinstance(this, exp.Table) else this.name
        columns = None
    table = catalog.get(db, tname) or reflect.reflect(storage, db, tname)
    if table is None:
        raise errors.undefined_table(tname)
    if columns is None:
        cols = list(table.columns)
        if not to_stdout:  # a generated column can't be copied in
            cols = [c for c in cols if c.generated is None and c.identity != "always"]
        columns = [c.name for c in cols]
    col_tags = []
    col_oids = []
    for name in columns:
        col = table.column(name)
        tag = col.type_tag if col is not None else "any"
        col_tags.append(tag)
        if col is not None and getattr(col, "json_plain", False):
            col_oids.append(114)  # plain json: binary form has no version byte
        else:
            col_oids.append(typemap.PG_OID.get(tag, 25))
    return CopyPlan(
        table,
        columns,
        to_stdout,
        fmt,
        delimiter,
        null,
        header,
        escape=escape,
        quote=quote,
        col_tags=col_tags,
        col_oids=col_oids,
    )


def _copy_query_rows(
    select: exp.Expression,
    storage: Any,
    db: str,
    catalog: Catalog,
    session: Session | None,
    *,
    render_text: bool = True,
) -> tuple[list[str], list[list] | None, list | None, list[str], list[int]]:
    """Run a ``COPY (SELECT …) TO`` query, returning ``(column_names,
    text_cells_or_None, raw_rows_or_None, type_tags, pg_oids)``. Text/CSV COPY
    renders each cell to its text form up front; binary COPY keeps the raw
    values so the per-type binary encoders see native values."""
    result = _run_query(select, storage, db, catalog, session or Session(database=db))
    columns = [c.name for c in result.columns]
    # A plain ``json`` output column (oid 114) renders compact ("json_plain"
    # is a render-only tag; jsonb keeps PG's canonical spacing).
    tags = [
        "json_plain" if c.pg_oid == 114 and c.type_tag == "json" else c.type_tag
        for c in result.columns
    ]
    oids = [c.pg_oid for c in result.columns]
    if not render_text:
        return columns, None, list(result.rows), tags, oids
    rows: list[list] = []
    for row in result.rows:
        cells: list = []
        for value, tag in zip(row, tags, strict=True):
            if value is None:
                cells.append(None)
            else:
                rendered = typemap.to_pg_text(value, tag)
                cells.append(rendered.decode() if rendered is not None else None)
        rows.append(cells)
    return columns, rows, None, tags, oids


def _copy_options(
    stmt: exp.Copy,
) -> tuple[str, str | None, str | None, bool, str | None, str | None]:
    """Parse ``FORMAT`` / ``CSV`` / ``BINARY`` / ``DELIMITER`` / ``NULL`` /
    ``HEADER`` / ``ESCAPE`` from a COPY statement's parameter list.

    sqlglot mangles the legacy un-parenthesized option syntax (``CSV NULL 'NS'
    DELIMITER '|'`` parses as CSV(expression=Null()) + Var(NS)(expression=
    DELIMITER) + Var(|)), so the params are flattened back into a token stream
    and scanned keyword-by-keyword — value keywords consume the next token.
    ESCAPE and HEADER are CSV-only (PG's 0A000, pinned by the pgtest copy
    corpus; PG 15's text-format HEADER is not modelled).


    An option keyword outside PG's COPY grammar (crdb's ``WITH destination =
    'nodelocal://…'``) is a 42601 syntax error, raised here — BEFORE the
    target table resolves — so the error class matches real PG (the pgtest
    copy_file_upload corpus pins 42601, not 42P01)."""
    tokens: list[str] = []
    for p in stmt.args.get("params") or []:
        for node in (p.this, p.args.get("expression")):
            if node is None:
                continue
            if isinstance(node, exp.Null):
                tokens.append("NULL")
            elif isinstance(node, exp.Literal):
                tokens.append(str(node.this))
            else:
                tokens.append(str(getattr(node, "name", node)))
    fmt, delimiter, null, header, escape, quote = "text", None, None, False, None, None
    i = 0

    def take_value() -> str:
        nonlocal i
        if i < len(tokens):
            v = tokens[i]
            i += 1
            return v
        return ""

    while i < len(tokens):
        key = tokens[i].upper()
        i += 1
        if key == "FORMAT":
            fmt = take_value().lower()
        elif key == "BINARY":
            # The legacy bare-keyword form (``COPY t FROM STDIN BINARY``,
            # pre-9.0 syntax pgx still emits).
            fmt = "binary"
        elif key == "CSV":
            fmt = "csv"
        elif key == "HEADER":
            header = True
            if i < len(tokens):
                nxt = tokens[i].upper()
                if nxt in ("TRUE", "FALSE", "ON", "OFF", "0", "1"):
                    header = nxt not in ("FALSE", "OFF", "0")
                    i += 1
        elif key == "DELIMITER":
            delimiter = take_value()
        elif key == "NULL":
            null = take_value()
        elif key == "ESCAPE":
            escape = take_value()
        elif key == "QUOTE":
            quote = take_value()
        elif key == "ENCODING":
            take_value()  # accepted; the wire encoding governs the payload
        elif key in ("FREEZE", "OIDS"):
            pass  # legacy bare keywords, no-ops here
        else:
            # PG's COPY grammar rejects unknown option keywords at parse —
            # crdb's ``WITH destination = '…'`` lands here.
            raise errors.syntax_error(f'syntax error at or near "{key.lower()}"')
    if quote is not None:
        if fmt != "csv":
            raise errors.feature_not_supported("COPY quote available only in CSV mode")
        if len(quote) != 1:
            raise errors.SQLError("22023", "COPY quote must be a single one-byte character")
    if escape is not None:
        if fmt != "csv":
            raise errors.feature_not_supported("COPY escape available only in CSV mode")
        if len(escape) != 1:
            raise errors.SQLError("22023", "COPY escape must be a single one-byte character")
    if header and fmt != "csv":
        raise errors.feature_not_supported("COPY HEADER available only in CSV mode")
    return fmt, delimiter, null, header, escape, quote


def _parse_bool_text(cell: str) -> bool:
    return cell.strip().lower() in ("t", "true", "y", "yes", "1", "on")


def copy_insert(
    storage: Any, db: str, catalog: Catalog, session: Session, plan: CopyPlan, rows: list[list]
) -> int:
    """Insert copy-stream rows (lists of string / None cells) into the target,
    coercing each cell to its column type; returns the number of rows inserted."""
    docs = []
    for cells in rows:
        if len(cells) != len(plan.columns):
            raise errors.SQLError(
                "22P04", f"extra or missing columns for COPY (expected {len(plan.columns)})"
            )
        converted = []
        for name, cell in zip(plan.columns, cells, strict=True):
            if cell is None:
                converted.append(None)
                continue
            col = plan.table.column(name)
            # Binary COPY decodes cells to typed Python values already; the
            # text coercion only applies to string cells from text/CSV.
            if col is not None and col.type_tag == "bool" and isinstance(cell, str):
                converted.append(_parse_bool_text(cell))
            else:
                converted.append(cell)
        docs.append(planner.copy_row_doc(plan.columns, converted, plan.table))
    res = executor.execute_insert(
        planner.InsertPlan(table=plan.table, docs=docs), storage, db, catalog, session
    )
    return res.rowcount


def copy_extract(
    storage: Any, db: str, catalog: Catalog, session: Session, plan: CopyPlan
) -> list[list]:
    """Read the target's rows as copy-stream cells (string / None) for COPY TO."""
    from secantus.paths import get_path

    if plan.query_rows is not None:  # COPY (SELECT …) TO — already rendered
        return plan.query_rows

    out: list[list] = []
    for doc in storage.find_matching(db, plan.table.collection, {}):
        cells: list = []
        for name in plan.columns:
            col = plan.table.column(name)
            field = col.field if col is not None else name
            tag = col.type_tag if col is not None else "any"
            if col is not None and getattr(col, "json_plain", False):
                tag = "json_plain"  # plain json renders compact
            value = get_path(doc, field)
            if value is None:
                cells.append(None)
            else:
                rendered = typemap.to_pg_text(value, tag)
                cells.append(rendered.decode() if rendered is not None else None)
        out.append(cells)
    return out


def copy_extract_raw(storage: Any, db: str, plan: CopyPlan) -> list[list]:
    """Read the COPY TO source as raw (unrendered) values for binary COPY —
    the per-type binary encoders need native values, not text cells."""
    from secantus.paths import get_path

    if plan.query_raw_rows is not None:  # COPY (SELECT …) TO
        return [list(row) for row in plan.query_raw_rows]
    out: list[list] = []
    for doc in storage.find_matching(db, plan.table.collection, {}):
        cells: list = []
        for name in plan.columns:
            col = plan.table.column(name)
            field = col.field if col is not None else name
            cells.append(get_path(doc, field))
        out.append(cells)
    return out


def _run_create_table_as(
    stmt: exp.Create,
    source: exp.Expression,
    storage: Any,
    db: str,
    catalog: Catalog,
    session: Session,
) -> SQLResult:
    """``CREATE [TEMP] TABLE name AS <query>`` — run the query once, create the
    table with the result's inferred column names/types, insert the rows, and
    report PG's CTAS tag (``SELECT <n>``, which drivers read as the row count).
    """
    while isinstance(source, exp.Subquery):
        source = source.this
    if not isinstance(source, (exp.Select, exp.SetOperation)):
        raise errors.feature_not_supported("CREATE TABLE AS requires a SELECT source")
    name = planner.qualified_table_name(stmt.this)
    if stmt.args.get("exists") and catalog.get(db, name) is not None:
        return SQLResult(command_tag="CREATE TABLE AS")
    result = _run_query(source, storage, db, catalog, session)
    seen: set[str] = set()
    coldefs: list[str] = []
    for i, c in enumerate(result.columns):
        cname = c.name or f"column{i + 1}"
        if cname in seen:
            raise errors.SQLError("42701", f'column "{cname}" specified more than once')
        seen.add(cname)
        tname = typemap.SQL_TYPE_NAME.get(c.type_tag) or (
            c.type_tag if c.type_tag in typemap.PG_OID else "text"
        )
        coldefs.append('"' + cname.replace('"', '""') + '" ' + tname)
    props = stmt.args.get("properties")
    temp = bool(props) and any(isinstance(e, exp.TemporaryProperty) for e in props.expressions)
    quoted = ".".join('"' + part.replace('"', '""') + '"' for part in name.split("."))
    create_sql = (
        "CREATE "
        + ("TEMPORARY " if temp else "")
        + "TABLE "
        + quoted
        + " ("
        + ", ".join(coldefs)
        + ")"
    )
    created = planner.parse(create_sql)[0]
    planner.qualify_temp_create_target(created, session)
    create_plan = planner.plan_create_table(created)
    executor.execute_create_table(create_plan, catalog, storage, db)
    if create_plan.table.temp:
        session.temp_tables.add((db, create_plan.table.name))
    table = _require_table(catalog, db, create_plan.table.name, storage)
    insert_stmt = exp.Insert(this=exp.to_table(quoted))
    plan = planner.plan_insert_rows(insert_stmt, table, result.rows)
    inserted = executor.execute_insert(plan, storage, db, catalog, session).rowcount
    return SQLResult(command_tag=f"SELECT {inserted}", rowcount=inserted)


def _run_statement(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    planner.qualify_from_search_path(stmt, catalog, db, session)
    if isinstance(stmt, exp.Select):
        # Star merge must see the USING list — the desugar below rewrites it
        # to ON, after which the join shape is indistinguishable.
        planner.expand_using_star(stmt, catalog, db)
        planner.expand_table_stars(stmt, catalog, db)
    planner.desugar_join_using(stmt)
    # Postgres resolves comparison operators during parse analysis, so a
    # cross-category comparison (``text_col = 42``) is a 42883 before any row
    # is read — not a predicate that matches nothing. Sound-but-incomplete:
    # a no-op for anything it cannot decide (see secantus.sql.typecheck).
    # Skipped on the CTE re-entry path — a materialized CTE's TableDef carries
    # types inferred from its own result shape, not declared ones.
    if not isinstance(catalog, _CTECatalog):
        typecheck.check_statement(stmt, catalog, db)
    if isinstance(stmt, exp.Create):
        kind = (stmt.args.get("kind") or "TABLE").upper()
        if kind == "TABLE":
            source = stmt.args.get("expression")
            if source is not None and not isinstance(stmt.this, exp.Schema):
                return _run_create_table_as(stmt, source, storage, db, catalog, session)
            planner.qualify_temp_create_target(stmt, session)
            plan = planner.plan_create_table(stmt)
            res = executor.execute_create_table(plan, catalog, storage, db)
            if plan.table.temp:
                # A temp table dies with its session (drop_session_temp_tables,
                # called by the wire server's connection teardown).
                session.temp_tables.add((db, plan.table.name))
            return res
        if kind == "INDEX":
            index = stmt.this
            tname = planner.qualified_table_name(index.args["table"])
            table = catalog.get(db, tname) or reflect.reflect(storage, db, tname)
            if table is None:
                raise errors.undefined_table(tname)
            return executor.execute_create_index(
                planner.plan_create_index(stmt, table), catalog, storage, db, session
            )
        if kind == "VIEW":
            if _is_materialized(stmt):
                return _create_matview(stmt, storage, db, catalog, session)
            return executor.execute_create_view(stmt, catalog, storage, db)
        if kind == "SEQUENCE":
            return _create_sequence(stmt, db, catalog)
        if kind == "TYPE":
            return _create_type(stmt, db, catalog)
        if kind == "FUNCTION":
            return _create_function(stmt, db, catalog, session)
        if kind == "SCHEMA":
            return _create_schema(stmt, db, catalog)
        if kind == "TRIGGER" and "lo_manage" not in stmt.sql(dialect="postgres").lower():
            return _create_trigger(stmt, db, catalog, session)
        if kind == "TRIGGER" and "lo_manage" in stmt.sql(dialect="postgres").lower():
            # contrib/lo's orphan-cleanup trigger, created verbatim by
            # LO-managing clients (pgjdbc's BlobTransactionTest). Accepted as
            # an inert no-op — the only skipped effect is unlinking replaced
            # large objects, which nothing vacuums here anyway. Every other
            # CREATE TRIGGER stays rejected: silently accepting a trigger
            # that never fires would lie about real user triggers.
            return SQLResult(command_tag="CREATE TRIGGER")
        raise errors.feature_not_supported(f"CREATE {kind} is not supported")

    if isinstance(stmt, exp.Drop):
        kind = (stmt.args.get("kind") or "TABLE").upper()
        if kind == "TABLE":
            plan = planner.plan_drop_table(stmt)
            _check_portal_table_pin(session, plan.name)
            return executor.execute_drop_table(plan, catalog, storage, db)
        if kind == "INDEX":
            return executor.execute_drop_index(planner.plan_drop_index(stmt), catalog, storage, db)
        if kind == "VIEW":
            if stmt.args.get("materialized"):
                return _drop_matview(stmt, storage, db, catalog)
            return executor.execute_drop_view(stmt, catalog, storage, db)
        if kind == "SEQUENCE":
            return _drop_sequence(stmt, db, catalog)
        if kind == "TYPE":
            return _drop_type(stmt, db, catalog)
        if kind == "FUNCTION":
            return _drop_function(stmt, db, catalog)
        if kind == "TRIGGER":
            return _drop_trigger(stmt, db, catalog)
        if kind == "SCHEMA":
            return _drop_schema(stmt, db, catalog, storage)
        raise errors.feature_not_supported(f"DROP {kind} is not supported")

    if isinstance(stmt, exp.Alter):
        return executor.execute_alter_table(planner.plan_alter_table(stmt), catalog, storage, db)

    if isinstance(stmt, exp.TruncateTable):
        return _run_truncate(stmt, storage, db, catalog)

    if isinstance(stmt, exp.Comment):
        return executor.execute_comment(stmt, catalog, storage, db)

    # Expand any referenced views (FROM/JOIN) into inline subqueries before the
    # query dispatch below, so a view reads like the SELECT it stands for. Skip on
    # the re-entrant CTE path (``_run_with`` strips the WITH and re-dispatches with
    # a ``_CTECatalog``): the outer pass already walked the whole tree, and a
    # second pass — now with no CTE names in scope — would let a stored view shadow
    # a same-named CTE.
    if not isinstance(catalog, _CTECatalog) and (
        isinstance(stmt, (exp.Select, exp.SetOperation, exp.Insert)) or _own_with(stmt) is not None
    ):
        _expand_views(stmt, catalog, db)

    if _own_with(stmt) is not None:
        return _run_with(stmt, storage, db, catalog, session)

    # Row-level security (#129): inject each applicable policy's USING predicate
    # into a single-table SELECT / UPDATE / DELETE WHERE before planning.
    _apply_rls_read(stmt, catalog, db, session)

    if isinstance(stmt, exp.SetOperation):
        return _run_set_operation(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Select):
        return _run_select(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Values):
        return _run_values(stmt, storage, db, catalog, session)

    check_pred = None
    if isinstance(stmt, (exp.Insert, exp.Update, exp.Delete)):
        # DML through an (automatically-updatable) view rewrites onto its base
        # table before planning (#146). ``check_pred`` is the view's WITH CHECK
        # OPTION predicate, enforced against each written row.
        stmt, check_pred = _rewrite_write_through_view(stmt, catalog, db)

    if isinstance(stmt, exp.Insert):
        return _run_insert(stmt, storage, db, catalog, session, check_option=check_pred)

    if isinstance(stmt, exp.Update):
        if _targets_pg_description(stmt, catalog, db):
            return _run_pg_description_dml(stmt, storage, db, catalog, session)
        if stmt.args.get("from_") is not None:
            return _run_update_from(stmt, storage, db, catalog, session)
        table = _require_table(
            catalog, db, planner.qualified_table_name(stmt.find(exp.Table)), storage
        )
        plan = planner.plan_update(stmt, table)
        plan.check_option = check_pred
        return executor.execute_update(plan, storage, db, catalog, session)

    if isinstance(stmt, exp.Delete):
        if _targets_pg_description(stmt, catalog, db):
            return _run_pg_description_dml(stmt, storage, db, catalog, session)
        if stmt.args.get("using"):
            return _run_delete_using(stmt, storage, db, catalog, session)
        table = _require_table(
            catalog, db, planner.qualified_table_name(stmt.find(exp.Table)), storage
        )
        return executor.execute_delete(
            planner.plan_delete(stmt, table), storage, db, catalog, session
        )

    if isinstance(stmt, exp.Merge):
        return _run_merge(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Set):
        return _run_set(stmt, session)

    if isinstance(stmt, exp.Command):
        verb = str(stmt.this).upper()
        if verb == "DECLARE":
            return _declare_cursor(stmt, storage, db, catalog, session)
        if verb in ("FETCH", "MOVE"):
            return _fetch_cursor(stmt, session, move=verb == "MOVE")
        if verb == "PREPARE":
            return _prepare_statement(stmt, session)
        if verb == "EXECUTE":
            return _execute_statement(stmt, storage, db, catalog, session)
        if verb == "EXPLAIN":
            return _explain_statement(stmt, storage, db, catalog, session)
        if verb == "REFRESH":
            return _refresh_matview(stmt, storage, db, catalog, session)
        if verb == "CREATE" and _VIEW_CHECK_OPTION_RE.search(stmt.sql(dialect="postgres")):
            return _create_view_check_option_command(stmt, storage, db, catalog)
        if verb == "CREATE" and _command_text(stmt).lstrip().upper().startswith("MATERIALIZED"):
            return _create_matview_command(stmt, storage, db, catalog, session)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("MATERIALIZED"):
            return _alter_matview_command(stmt, storage, db, catalog, session)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("SEQUENCE"):
            return _alter_sequence_command(stmt, db, catalog)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("DATABASE"):
            return _alter_database_command(stmt, storage, db, catalog, session)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("TYPE"):
            return _alter_type_command(stmt, db, catalog)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("DOMAIN"):
            return _alter_domain_command(stmt, storage, db, catalog)
        if verb == "CREATE" and _command_text(stmt).lstrip().upper().startswith("DOMAIN"):
            return _create_domain_command(stmt, db, catalog)
        if verb == "CREATE" and _CREATE_RANGE_RE.match(_command_text(stmt)):
            # ``CREATE TYPE name AS RANGE (subtype = X, …)`` exceeds sqlglot's
            # parser and falls back to a Command.
            return _create_range_type_command(stmt, db, catalog)
        if verb == "DROP" and _command_text(stmt).lstrip().upper().startswith("DOMAIN"):
            return _drop_domain_command(stmt, db, catalog)
        if verb == "COMMENT_CONSTRAINT":
            return _comment_constraint_command(stmt, db, catalog)
        if verb == "COMMENT" and _command_text(stmt).lstrip().upper().startswith("ON DOMAIN"):
            return _comment_domain_command(stmt, db, catalog)
        if verb == "MULTIDROP_TABLE":
            # ``DROP TABLE a, b`` — one statement, one tag; without IF EXISTS
            # every name must resolve BEFORE anything drops (PG atomicity).
            drops = stmt.args.get("drops") or []
            for d in drops:
                plan = planner.plan_drop_table(d)
                _check_portal_table_pin(session, plan.name)
                if not d.args.get("exists") and catalog.get(db, plan.name) is None:
                    raise errors.undefined_table(plan.name)
            for d in drops:
                executor.execute_drop_table(planner.plan_drop_table(d), catalog, storage, db)
            return SQLResult(command_tag="DROP TABLE")
        if verb == "VACUUM":
            # Nothing to vacuum in a surrogate store; accept like real PG
            # (pgbench -i runs ``vacuum analyze`` unconditionally).
            return SQLResult(command_tag="VACUUM")
        if verb == "CREATE" and _command_text(stmt).lstrip().upper().startswith("EXTENSION"):
            return _create_extension_command(stmt)
        if verb == "DROP" and _command_text(stmt).lstrip().upper().startswith("EXTENSION"):
            return _drop_extension_command(stmt)
        if verb == "CREATE" and _command_text(stmt).lstrip().upper().startswith("OPERATOR"):
            return _create_operator_command(stmt, db, catalog)
        if verb == "DROP" and _command_text(stmt).lstrip().upper().startswith("OPERATOR"):
            return _drop_operator_command(stmt, db, catalog)
        if verb == "CREATE" and _command_text(stmt).lstrip().upper().startswith("POLICY"):
            return _create_policy_command(stmt, storage, db, catalog)
        if verb == "DROP" and _command_text(stmt).lstrip().upper().startswith("POLICY"):
            return _drop_policy_command(stmt, db, catalog)
        if verb == "ALTER" and _RLS_ALTER_RE.match(_command_text(stmt)):
            return _alter_rls_command(stmt, storage, db, catalog)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("TABLE"):
            # A *mixed-kind* multi-action ALTER TABLE (e.g. ``ADD COLUMN a, DROP
            # COLUMN b``) exceeds sqlglot's ALTER parser and falls back to a
            # Command. Split the action list and re-parse each action on its own
            # (sqlglot handles any single action), then run the combined ALTER.
            return _run_mixed_alter_table(stmt, storage, db, catalog, session)
        if verb == "SET" and _command_text(stmt).lstrip().upper().startswith("CONSTRAINTS"):
            return _set_constraints_command(stmt, storage, db, catalog, session)
        if verb in ("CREATE", "DROP", "ALTER") and _command_text(stmt).lstrip().upper().startswith(
            ("ROLE ", "USER ", "GROUP ")
        ):
            return _run_role_command(verb, stmt, db, catalog)
        if verb in ("GRANT", "REVOKE"):
            # Role membership (``GRANT <role> TO <member>``) is recorded + reflected
            # via pg_auth_members (#138); other privilege grants that fell back to a
            # Command (e.g. ``GRANT USAGE ON SCHEMA``) aren't enforced — accept no-op.
            return _run_role_membership(verb, stmt, db, catalog, session)
        return _run_command(stmt, session, storage, db, catalog)

    if isinstance(stmt, exp.Grant):
        return _run_grant(stmt, storage, db, catalog, session, revoke=False)

    if isinstance(stmt, exp.Revoke):
        return _run_grant(stmt, storage, db, catalog, session, revoke=True)

    # CLOSE cursor / CLOSE ALL parses as a bare Alias (``CLOSE AS name``).
    close = _close_cursor_target(stmt)
    if close is not None:
        return _close_cursor(close, session)

    # DEALLOCATE / DISCARD parse as a bare Alias in sqlglot's pg dialect (e.g.
    # ``DEALLOCATE "x"`` → ``DEALLOCATE AS "x"``); libpq clients (psycopg) and
    # SQLAlchemy emit them to manage prepared statements. (SAVEPOINT / RELEASE are
    # real commands, handled in ``_dispatch``.)
    dealloc = _deallocate_target(stmt)
    if dealloc is not None:
        return _deallocate_statement(dealloc, session)
    noop = _noop_command_word(stmt)
    if noop is not None:
        return SQLResult(command_tag=noop)

    if is_nonstatement_expression(stmt):
        # Garbage input ("wat", "SYNTAX ERROR") parses as a bare column /
        # aliased expression, not a statement — Postgres raises a syntax
        # error, and clients map 42601 to ProgrammingError (0A000 maps to
        # NotSupportedError). The expression-shaped COMMANDS sqlglot
        # mis-parses the same way (CLOSE / DISCARD / DEALLOCATE) were
        # already handled above and are exempted by the predicate.
        raise errors.SQLError("42601", f'syntax error at or near "{stmt.sql()[:40]}"')
    if isinstance(stmt, exp.Copy):
        # COPY reaching the generic dispatcher means it wasn't the sole
        # statement of a wire-level copy() (a multi-statement string) or came
        # through the embedded API — real PG rejects it as a ProgrammingError-
        # class condition, not a NotSupported one.
        raise errors.SQLError(
            "42601", "COPY ... TO/FROM STDIN/STDOUT must be a standalone statement"
        )
    raise errors.feature_not_supported(f"unsupported statement: {type(stmt).__name__}")


_NOOP_WORDS = {"DISCARD"}

#: Commands sqlglot mis-parses as bare Alias/Column expressions but that ARE
#: real statements with handlers in this engine. Anything else expression-
#: shaped at the top level is garbage input, rejected 42601 like real PG.
# SAVEPOINT / RELEASE also parse as a bare Alias ("SAVEPOINT AS sp1") and are
# rescued by dispatch's _savepoint_command — the Parse-time garbage guard
# (#876) must not reject them (pgjdbc's setSavepoint broke exactly that way).
_EXPRESSION_COMMAND_WORDS = {"CLOSE", "DISCARD", "DEALLOCATE", "SAVEPOINT", "RELEASE"}


def is_nonstatement_expression(stmt: exp.Expression) -> bool:
    """True when ``stmt`` is a bare expression posing as a statement — the
    shape sqlglot produces for garbage input like ``bad`` or ``SYNTAX ERROR``.
    Real PG rejects these at parse time (42601); the extended protocol's
    Parse uses this predicate so pgx's Prepare("SYNTAX ERROR") errors there,
    not silently at Execute."""
    if not isinstance(stmt, (exp.Column, exp.Identifier, exp.Literal, exp.Anonymous, exp.Alias)):
        return False
    head = stmt.this if isinstance(stmt, exp.Alias) else stmt
    name = head.name if isinstance(head, exp.Column) else None
    return not (name is not None and name.upper() in _EXPRESSION_COMMAND_WORDS)


def _noop_command_word(stmt: exp.Expression) -> str | None:
    """The command tag for a no-op ``DISCARD`` statement, or None. Postgres
    echoes the target in the tag — ``DISCARD ALL`` / ``DISCARD PLANS`` /
    ``DISCARD SEQUENCES`` / ``DISCARD TEMP`` (``TEMPORARY`` folds to ``TEMP``)."""
    head = stmt.this if isinstance(stmt, exp.Alias) else stmt
    name = head.name if isinstance(head, exp.Column) else None
    if name is None or name.upper() not in _NOOP_WORDS:
        return None
    target = stmt.alias.upper() if isinstance(stmt, exp.Alias) and stmt.alias else ""
    if target == "TEMPORARY":
        target = "TEMP"
    return f"{name.upper()} {target}".strip()


# ---------------------------------------------------------------------------
# PREPARE / EXECUTE / DEALLOCATE — SQL-level prepared statements.
#
# ``PREPARE name [(argtypes)] AS <query>`` stores the parsed query (with its
# ``$N`` placeholders) on the session; ``EXECUTE name [(args)]`` substitutes the
# supplied argument literals for the placeholders and runs the query, returning
# its result; ``DEALLOCATE name`` / ``DEALLOCATE ALL`` forgets the statement(s).
# These are distinct from the extended wire protocol's Parse/Bind portals
# (``pgextended.py``) — psql's ``PREPARE``/``EXECUTE`` and libpq's
# ``PQprepare`` land here. sqlglot falls back to a ``Command`` for PREPARE/EXECUTE
# (tail carried as a string Literal) and to a bare ``Alias`` for DEALLOCATE.
# ---------------------------------------------------------------------------

_PREPARE_TAIL = re.compile(
    r'^\s*(?P<name>"[^"]+"|[A-Za-z_][A-Za-z0-9_$]*)\s*'
    r"(?:\([^)]*\))?\s+AS\s+(?P<query>.*?)\s*;?\s*$",
    re.IGNORECASE | re.DOTALL,
)
_EXECUTE_TAIL = re.compile(
    r'^\s*(?P<name>"[^"]+"|[A-Za-z_][A-Za-z0-9_$]*)\s*'
    r"(?:\((?P<args>.*)\))?\s*;?\s*$",
    re.IGNORECASE | re.DOTALL,
)


def _prepare_statement(stmt: exp.Command, session: Session) -> SQLResult:
    """``PREPARE name [(types)] AS <query>`` — parse and store the query AST."""
    tail = _command_tail(stmt)
    m = _PREPARE_TAIL.match(tail)
    if m is None:
        raise errors.syntax_error(f"malformed PREPARE: {tail}")
    name = _unquote_ident(m.group("name"))
    parsed = planner.parse(m.group("query"))
    if len(parsed) != 1:
        raise errors.syntax_error("PREPARE expects a single statement")
    query = parsed[0]
    if name in session.prepared:
        raise errors.SQLError("42P05", f'prepared statement "{name}" already exists')
    session.prepared[name] = (query, planner.parameter_count(query))
    return SQLResult(command_tag="PREPARE")


def _execute_statement(
    stmt: exp.Command, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``EXECUTE name [(args)]`` — bind the args into the stored query and run it."""
    tail = _command_tail(stmt)
    m = _EXECUTE_TAIL.match(tail)
    if m is None:
        raise errors.syntax_error(f"malformed EXECUTE: {tail}")
    name = _unquote_ident(m.group("name"))
    entry = session.prepared.get(name)
    if entry is None:
        raise errors.SQLError("26000", f'prepared statement "{name}" does not exist')
    query, param_count = entry
    args = _execute_args(m.group("args"))
    if len(args) != param_count:
        raise errors.SQLError(
            "08P01",
            f"wrong number of parameters for prepared statement "
            f'"{name}": expected {param_count}, got {len(args)}',
        )
    bound = _bind_parameter_nodes(query, args)
    return _run_statement(bound, storage, db, catalog, session)


def _execute_args(raw: str | None) -> list[exp.Expression]:
    """Parse the ``(a, b, …)`` argument list of an EXECUTE into expression nodes."""
    if raw is None or raw.strip() == "":
        return []
    try:
        wrapper = sqlglot.parse_one(f"SELECT {raw}", read="postgres")
    except Exception as exc:  # noqa: BLE001 — surface as a clean SQL syntax error
        raise errors.syntax_error(f"malformed EXECUTE arguments: ({raw})") from exc
    return list(wrapper.expressions)


def _bind_parameter_nodes(query: exp.Expression, args: list[exp.Expression]) -> exp.Expression:
    """Replace ``$N`` placeholders in ``query`` with the EXECUTE argument nodes."""
    query = query.copy()
    for param in list(query.find_all(exp.Parameter)):
        try:
            idx = int(param.name) - 1
        except (TypeError, ValueError) as exc:
            raise errors.syntax_error(f"invalid bind parameter ${param.name}") from exc
        if idx < 0 or idx >= len(args):
            raise errors.syntax_error(f"bind parameter ${param.name} has no value")
        param.replace(args[idx].copy())
    return query


def _explain_statement(
    stmt: exp.Command, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``EXPLAIN [ANALYZE] [(options)] <statement>`` — render the query plan. The
    inner statement is re-run (via ``_run_statement``) only under ANALYZE."""
    tail = _command_tail(stmt)

    def _run(inner: exp.Expression) -> SQLResult:
        return _run_statement(inner, storage, db, catalog, session)

    return explain_mod.explain(tail, storage, db, catalog, session, run_stmt=_run)


def _deallocate_target(stmt: exp.Expression) -> str | None:
    """A bare ``DEALLOCATE name`` / ``DEALLOCATE ALL`` — parses as a top-level
    ``Alias`` (``DEALLOCATE AS name``). Returns the target name / ``"ALL"`` or None."""
    if not isinstance(stmt, exp.Alias):
        return None
    head = stmt.this
    if isinstance(head, exp.Column) and head.name.upper() == "DEALLOCATE":
        return stmt.alias
    return None


def _deallocate_statement(target: str, session: Session) -> SQLResult:
    """``DEALLOCATE name`` forgets one prepared statement; ``DEALLOCATE ALL`` clears
    them all. Unlike Postgres, deallocating an unknown name is a silent no-op here
    (libpq/psycopg fire speculative DEALLOCATEs during connection cleanup)."""
    wire = getattr(session, "wire_prepared", None)
    is_all = target.upper() == "ALL"
    if is_all:
        session.prepared.clear()
        if wire is not None:
            # The extended protocol's server-side prepared statements clear
            # too — psycopg sends DEALLOCATE ALL after DDL to invalidate its
            # cache, and pg_prepared_statements must reflect it.
            wire.clear()
    else:
        name = _unquote_ident(target)
        session.prepared.pop(name, None)
        if wire is not None:
            wire.pop(name, None)
    # Postgres reports "DEALLOCATE ALL" for the ALL form, and drivers key off
    # that exact tag: pgjdbc's QueryExecutor watches for it to learn that its
    # server-side statement cache is gone and re-Parse. Reporting a bare
    # "DEALLOCATE" left it executing statement names the server had dropped,
    # which surfaced as `prepared statement "S_5" does not exist`.
    return SQLResult(command_tag="DEALLOCATE ALL" if is_all else "DEALLOCATE")


def _validate_locks(stmt: exp.Select) -> None:
    """Validate ``FOR UPDATE`` / ``FOR SHARE`` (and NO KEY / KEY variants, with
    SKIP LOCKED / NOWAIT) row-locking clauses (#132). SecantusDB is single-node so
    the lock itself is a no-op — but an ``OF <table>`` target that isn't a relation
    in the query's FROM is a hard error, exactly as Postgres reports it."""
    locks = stmt.args.get("locks")
    if not locks:
        return
    # Scope is the FROM + JOIN relations only (not tables nested inside the lock's
    # own ``OF`` list, which are part of the same AST).
    sources: list[exp.Expression] = []
    frm = stmt.args.get("from_") or stmt.args.get("from")
    if frm is not None:
        sources.append(frm)
    sources.extend(stmt.args.get("joins") or [])
    in_scope: set[str] = set()
    for src in sources:
        for t in src.find_all(exp.Table):
            # An alias masks the base name for FOR UPDATE OF purposes (Postgres).
            in_scope.add(t.alias if t.alias else t.name)
    for lock in locks:
        for tgt in lock.args.get("expressions") or []:
            name = tgt.name if isinstance(tgt, exp.Table) else str(getattr(tgt, "name", tgt))
            if name and name not in in_scope:
                raise errors.SQLError(
                    "42P01",
                    f'relation "{name}" in FOR UPDATE/SHARE clause not found in FROM clause',
                )


def _describe_returning(
    storage: Any, db: str, catalog: Catalog, stmt: exp.Expression
) -> list[ColumnDesc] | None:
    """Result columns of a DML statement's RETURNING clause, or None when there
    is no RETURNING (or the target can't be resolved — Execute then raises the
    real error)."""
    if stmt.args.get("returning") is None:
        return None
    table_node = stmt.find(exp.Table)
    if table_node is None:
        return None
    _qn = planner.qualified_table_name(table_node)
    tdef = catalog.get(db, _qn) or reflect.reflect(storage, db, _qn)
    if tdef is None:
        return None
    try:
        items = planner._returning_columns(stmt, tdef)
    except errors.SQLError:
        return None
    if items is None:
        return None
    return executor._out_column_descs([(name, col) for name, col, _expr in items], storage, db)


def _pg_typeof_table(storage: Any, db: str, catalog: Catalog, table_node: exp.Table | None):
    """Best-effort TableDef for ``rewrite_pg_typeof``'s column resolution — None
    for FROM-less selects and relations we can't resolve (the rewrite then skips
    any call it can't type and the normal path surfaces the real error)."""
    if table_node is None or table_node.args.get("db"):
        return None
    try:
        return catalog.get(db, table_node.name) or reflect.reflect(storage, db, table_node.name)
    except errors.SQLError:
        return None


def _record_srf_call(node: exp.Expression) -> exp.Expression | None:
    """The record-SRF call inside ``node`` when node IS such a call — either a
    bare ``Anonymous`` (``_pg_expandarray(x)``) or the schema-qualified
    ``Dot(Identifier(information_schema), Anonymous(...))`` sqlglot produces."""
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        node = node.expression
    if isinstance(node, exp.Anonymous) and srf._is_record_srf(node):
        return node
    return None


def _projection_record_srf_spec(stmt: exp.Select) -> dict | None:
    """Detect record SRFs in the projection list; None when there are none (or
    a shape we don't expand — nested uses fall through to the normal error).

    Recognized projection shapes:
    * ``[alias =] SRF(arr)`` — a composite (x, n) column, one row per element;
    * ``[alias =] (SRF(arr)).<field>`` — immediate field access.

    Multiple references to the same call (same argument text) expand in
    lockstep from ONE evaluation, PG's multi-SRF row pairing for the identical-
    call case (different-argument SRFs pad with NULLs to the longest).
    """
    if stmt.args.get("group") or stmt.args.get("distinct") or stmt.args.get("having"):
        return None
    plan: list[tuple] = []
    keys: dict[str, exp.Expression] = {}  # arg-sql -> arg expression
    found = False
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        call = _record_srf_call(target)
        if call is not None:
            arg = call.expressions[0] if call.expressions else exp.Null()
            key = arg.sql(dialect="postgres")
            keys.setdefault(key, arg)
            name = alias or str(call.this).rsplit(".", 1)[-1].lower()
            plan.append(("record", key, name))
            found = True
            continue
        if isinstance(target, exp.Dot) and isinstance(target.this, exp.Paren):
            call = _record_srf_call(target.this.this)
            if call is not None:
                field = target.expression.name.lower()
                if field not in ("x", "n"):
                    return None
                arg = call.expressions[0] if call.expressions else exp.Null()
                key = arg.sql(dialect="postgres")
                keys.setdefault(key, arg)
                plan.append(("field", key, alias or field, field))
                found = True
                continue
        if list(
            srf._is_record_srf(n) for n in target.find_all(exp.Anonymous) if srf._is_record_srf(n)
        ):
            return None  # record SRF nested somewhere we don't expand
        plan.append(("copy", e))
    if not found:
        return None
    return {"stmt": stmt, "plan": plan, "keys": keys}


def _run_selectlist_srf(
    spec: dict, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Execute a SELECT whose projection list contains record SRFs: run the
    query with each SRF call replaced by its ARRAY argument, then expand each
    result row to one row per element, building the composite / field cells."""
    stmt: exp.Select = spec["stmt"]
    plan = spec["plan"]
    inner = stmt.copy()
    projections: list[exp.Expression] = []
    copy_idx: dict[int, int] = {}  # plan position -> inner column index
    for pos, op in enumerate(plan):
        if op[0] == "copy":
            copy_idx[pos] = len(projections)
            projections.append(op[1].copy())
    key_idx: dict[str, int] = {}
    for k, arg in spec["keys"].items():
        key_idx[k] = len(projections)
        projections.append(
            exp.Alias(this=arg.copy(), alias=exp.to_identifier(f"__srf_arg{len(key_idx)}"))
        )
    inner.set("expressions", projections)
    res = _run_select(inner, storage, db, catalog, session)

    out_rows: list[tuple] = []
    for row in res.rows:
        elems: dict[str, list] = {}
        for k, idx in key_idx.items():
            v = row[idx]
            elems[k] = list(v) if isinstance(v, (list, tuple)) else []
        height = max((len(v) for v in elems.values()), default=0)
        # PG: an SRF returning zero rows eliminates the input row entirely.
        for i in range(height):
            cells: list = []
            for pos, op in enumerate(plan):
                if op[0] == "copy":
                    cells.append(row[copy_idx[pos]])
                elif op[0] == "record":
                    items = elems[op[1]]
                    if i < len(items):
                        cells.append(typemap.RecordValue((("x", items[i]), ("n", i + 1))))
                    else:
                        cells.append(None)
                else:  # field
                    items = elems[op[1]]
                    field = op[3]
                    if i >= len(items):
                        cells.append(None)
                    else:
                        cells.append(items[i] if field == "x" else i + 1)
            out_rows.append(tuple(cells))

    columns: list[ColumnDesc] = []
    for pos, op in enumerate(plan):
        if op[0] == "copy":
            columns.append(res.columns[copy_idx[pos]])
        elif op[0] == "record":
            columns.append(ColumnDesc(op[2], "composite", typemap.PG_OID["composite"]))
        else:
            name = op[2]
            if op[3] == "n":
                columns.append(ColumnDesc(name, "int4", typemap.PG_OID["int4"]))
            else:
                columns.append(ColumnDesc(name, "any", 0))
    return SQLResult(
        command_tag=f"SELECT {len(out_rows)}",
        columns=columns,
        rows=out_rows,
        rowcount=len(out_rows),
    )


def _run_select(
    stmt: exp.Select, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    _validate_locks(stmt)  # FOR UPDATE / SHARE: single-node no-op, but OF-targets validated.
    planner.unwrap_paren_join_from(stmt)  # FROM (a JOIN b) — grouping parens, not a derived table
    planner.rewrite_pg_typeof(stmt, _pg_typeof_table(storage, db, catalog, stmt.find(exp.Table)))
    # A record SRF (``information_schema._pg_expandarray``) in the SELECT list —
    # pgjdbc's DatabaseMetaData PK/index queries put it there, both bare
    # (composite column) and with immediate field access (``(SRF(x)).n``).
    srf_expansion = _projection_record_srf_spec(stmt)
    if srf_expansion is not None:
        return _run_selectlist_srf(srf_expansion, storage, db, catalog, session)
    # A base-less set-returning function as the row source: ``FROM generate_series(…)``
    # / ``FROM unnest(…)`` / … or a bare ``SELECT generate_series(…)``.
    srf_source = srf.from_source(stmt) or srf.fromless_projection(stmt)
    if srf_source is not None:
        return _run_srf_select(srf_source, stmt, storage, db, catalog, session)

    table_node = stmt.find(exp.Table)
    # ``find`` descends into subqueries — a FROM-less outer SELECT whose WHERE
    # contains ``EXISTS (SELECT … FROM t)`` still takes the constant path (the
    # scalar evaluator runs the subquery with the real storage in scope). A
    # table-less FROM (``FROM (VALUES …) AS alias(cols)``) is a derived table,
    # routed through the pipeline path below.
    from_node = stmt.args.get("from_")
    if from_node is None or (
        table_node is None and not isinstance(from_node.this, (exp.Subquery, exp.Values))
    ):
        return executor.execute_constant_select(
            planner.plan_constant_select(stmt, session, storage, catalog, db)
        )

    # A WITH NO DATA materialized view is not scannable until its first REFRESH.
    if (
        table_node is not None
        and not table_node.args.get("db")
        and catalog.get_matview(db, table_node.name) is not None
        and not catalog.matview_populated(db, table_node.name)
    ):
        raise errors.SQLError(
            "55000",
            f'materialized view "{table_node.name}" has not been populated',
        )

    # JOIN / GROUP BY / aggregates compile to an aggregation pipeline. Route it
    # through a CatalogBackend so the pipeline can read pg_catalog /
    # information_schema relations (the joins interactive psql's \d emits) as
    # well as real collections.
    if planner.select_needs_pipeline(stmt):
        backend = virtual.CatalogBackend(storage, catalog, session, db)
        plan = planner.plan_pipeline_select(stmt, db, catalog, storage, session=session)
        sctx = scalar.ScalarContext(storage=backend, catalog=catalog, db=db, session=session)
        if isinstance(plan, planner.EvaluatedSelectPlan):
            return executor.execute_evaluated_select(plan, backend, db, sctx)
        return executor.execute_pipeline_select(plan, backend, db, sctx)

    schema = table_node.args.get("db")
    schema_name = schema.name if schema is not None else None
    vtable = virtual.lookup(schema_name, table_node.name)
    if vtable is not None:
        rows = vtable.builder(db, session, storage, catalog)
        backend = virtual.MemoryBackend(rows)
        tdef = vtable.table_def()
        # Publish the subquery context so the pushdown can resolve
        # catalog-dependent constants (``WHERE t.oid = to_regtype('mood')`` —
        # a user-declared enum's oid lives in the real catalog).
        token = planner._pipeline_subctx.set(
            planner.SubqueryCtx(storage=storage, db=db, catalog=catalog, session=session)
        )
        try:
            if planner._stmt_needs_evaluation(stmt) or planner.where_needs_per_row(
                stmt, tdef, catalog, db
            ):
                # Computed projections — or a WHERE the pushdown can't lower —
                # over a catalog table need per-row evaluation with the REAL
                # catalog in scope.
                mem_sctx = scalar.ScalarContext(
                    storage=backend, catalog=catalog, db=db, session=session
                )
                return executor.execute_evaluated_select(
                    planner._build_evaluated_single(stmt, tdef), backend, db, mem_sctx
                )
            plan = planner.plan_select(stmt, tdef)
            return executor.execute_select(plan, backend, db)
        finally:
            planner._pipeline_subctx.reset(token)

    # A declared table, else a reflected (schema-on-read) view of an existing
    # Mongo collection — the dual-protocol read path.
    _qn = planner.qualified_table_name(table_node)
    table = catalog.get(db, _qn) or reflect.reflect(storage, db, _qn)
    if table is None:
        raise errors.undefined_table(_qn)
    # A WHERE with EXISTS or a correlated subquery can't lower to a pushdown
    # filter — evaluate it per row (the inner query reads through the same
    # storage view, with outer-row references resolved by the scalar evaluator).
    if planner.where_needs_per_row(stmt, table, catalog, db):
        subctx = planner.SubqueryCtx(storage=storage, db=db, catalog=catalog, session=session)
        plan = planner.plan_correlated_select(stmt, table, subctx)
        return executor.execute_correlated_select(plan, storage, db, catalog, session)
    # A non-correlated WHERE subquery (`x IN (SELECT ...)`, `x = (SELECT ...)`) is
    # pre-evaluated by the planner, which runs the inner SELECT through the engine.
    subctx = planner.SubqueryCtx(storage=storage, db=db, catalog=catalog, session=session)
    return executor.execute_select(planner.plan_select(stmt, table, subctx), storage, db)


def _run_srf_select(
    source: srf.SrfSource,
    stmt: exp.Select,
    storage: Any,
    db: str,
    catalog: Catalog,
    session: Session,
) -> SQLResult:
    """Materialize a base-less SRF's rows into an in-memory table and run the rest
    of the query (projection / WHERE / ORDER BY / LIMIT) over it via the normal
    select planner + executor."""
    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)

    def _toplevel_agg(e: exp.Expression) -> bool:
        # Only aggregates belonging to THIS select — one inside a scalar
        # subquery projection aggregates the subquery's rows, not the SRF's.
        return any(agg.find_ancestor(exp.Select) is stmt for agg in e.find_all(exp.AggFunc))

    if stmt.args.get("from_") is not None and any(_toplevel_agg(e) for e in stmt.expressions):
        # An aggregate over an SRF row source (``SELECT string_agg(word, ',')
        # FROM pg_get_keywords() …`` — pgjdbc's getSQLKeywords) exceeds the
        # single-table SRF planner. The derived-table pipeline handles
        # aggregates over materialized rows already, so wrap the SRF FROM in a
        # subquery and re-dispatch through the normal query path.
        rewritten = stmt.copy()
        from_node = rewritten.args["from_"]
        srf_table = from_node.this
        alias = (
            srf_table.alias if hasattr(srf_table, "alias") and srf_table.alias else "srf_agg_src"
        )
        inner = exp.Select(expressions=[exp.Star()])
        inner.set("from_", exp.From(this=srf_table.copy()))
        sub = exp.Subquery(this=inner, alias=exp.TableAlias(this=exp.to_identifier(alias)))
        rewritten.set("from_", exp.From(this=sub))
        return _run_query(rewritten, storage, db, catalog, session)
    rows, tdef = srf.build(source, sctx)
    query = stmt
    if stmt.args.get("from_") is None:
        # ``SELECT generate_series(…)`` — retarget the projection at the value
        # column so the standard column-projection path handles it.
        query = stmt.copy()
        query.set("from", exp.From(this=exp.Table(this=exp.to_identifier(tdef.name))))
        query.set("expressions", [exp.column(tdef.columns[0].name)])
    backend = virtual.MemoryBackend(rows)
    if planner._stmt_needs_evaluation(query):
        # A computed projection (``SELECT x * 2 FROM generate_series(…) t(x)``)
        # needs per-row evaluation over the materialized rows. The rows ride in
        # via ``backend``; the scalar context keeps the REAL storage so a
        # scalar subquery in the projection can dispatch through the engine
        # (``SELECT (SELECT string_agg(…) FROM generate_series(…)) FROM
        # generate_series(…)`` — RefCursorFetchTest's seeding INSERT).
        mem_sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
        return executor.execute_evaluated_select(
            planner._build_evaluated_single(query, tdef), backend, db, mem_sctx
        )
    return executor.execute_select(planner.plan_select(query, tdef), backend, db)


def _run_insert(
    stmt: exp.Insert,
    storage: Any,
    db: str,
    catalog: Catalog,
    session: Session,
    check_option: Any = None,
) -> SQLResult:
    """Dispatch an INSERT: ``VALUES`` plans directly; ``INSERT … SELECT`` runs the
    source query first (it may join / aggregate / be a set operation), then maps
    its result rows positionally onto the target columns. ``check_option`` is an
    auto-updatable view's WITH CHECK OPTION predicate, enforced per inserted row."""
    target = stmt.this
    name = (
        planner.qualified_table_name(target.this)
        if isinstance(target, exp.Schema)
        else planner.qualified_table_name(target)
        if isinstance(target, exp.Table)
        else target.name
    )
    table = _require_table(catalog, db, name, storage)
    source = stmt.expression
    if isinstance(source, (exp.Select, exp.SetOperation)):
        result = _run_query(source, storage, db, catalog, session)
        ncols = len(planner.insert_target_columns(stmt, table))
        if len(result.columns) != ncols:
            raise errors.SQLError(
                "42601",
                f"INSERT has {ncols} target columns but the source query "
                f"returns {len(result.columns)}",
            )
        plan = planner.plan_insert_rows(stmt, table, result.rows)
    else:
        plan = planner.plan_insert(
            stmt,
            table,
            planner.SubqueryCtx(storage=storage, db=db, catalog=catalog, session=session),
        )
    plan.check_option = check_option
    return executor.execute_insert(plan, storage, db, catalog, session)


def run_inner_select(
    stmt: exp.Select, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Run a non-correlated subquery's inner SELECT and return its result rows.

    Used by the planner's WHERE-subquery evaluation; reuses the full SELECT path
    so the inner query may itself aggregate / filter / join."""
    return _run_select(stmt, storage, db, catalog, session)


def _run_query(
    node: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Evaluate a SELECT or a (possibly nested) set operation to a SQLResult."""
    if _own_with(node) is not None:
        return _run_with(node, storage, db, catalog, session)
    if isinstance(node, exp.SetOperation):
        return _run_set_operation(node, storage, db, catalog, session)
    if isinstance(node, exp.Select):
        return _run_select(node, storage, db, catalog, session)
    if isinstance(node, exp.Values):
        return _run_values(node, storage, db, catalog, session)
    if isinstance(node, exp.Subquery) and node.this is not None:
        # A parenthesized arm — ``(SELECT … ORDER BY … LIMIT 1) UNION …`` —
        # parses as a Subquery wrapper; its ORDER BY / LIMIT apply within the
        # arm, exactly what running the inner query yields.
        return _run_query(node.this, storage, db, catalog, session)
    raise errors.feature_not_supported(f"unsupported set-operation arm: {type(node).__name__}")


_MATVIEW_NAME_RE = re.compile(r"(?is)^\s*MATERIALIZED\s+VIEW\s+(?:CONCURRENTLY\s+)?(.+?)\s*;?\s*$")


def _is_materialized(stmt: exp.Create) -> bool:
    props = stmt.args.get("properties")
    return bool(props) and any(isinstance(p, exp.MaterializedProperty) for p in props.expressions)


def _matview_docs(result: SQLResult) -> list[dict[str, Any]]:
    """Turn a query result into snapshot docs keyed by output column name. Storage
    assigns each an ``_id``; the matview's own columns are the SELECT outputs."""
    names = [c.name for c in result.columns]
    return [{n: v for n, v in zip(names, row, strict=False)} for row in result.rows]


def _matview_table_def(name: str, result: SQLResult) -> TableDef:
    return TableDef(
        name=name,
        collection=name,
        columns=[
            Column(name=c.name, type_tag=c.type_tag, field=c.name, pk=False, nullable=True)
            for c in result.columns
        ],
    )


def _materialize(name: str, definition: str, storage: Any, db: str, catalog, session) -> SQLResult:
    """(Re)compute a materialized view's snapshot: run its SELECT and replace the
    rows in the backing collection (named after the matview). Returns the result."""
    inner = sqlglot.parse_one(definition, read="postgres")
    result = _run_query(inner, storage, db, catalog, session)
    storage.delete_matching(db, name, {})
    docs = _matview_docs(result)
    if docs:
        storage.insert(db, name, docs)
    return result


def _create_matview(
    stmt: exp.Create, storage: Any, db: str, catalog, session, populate: bool = True
) -> SQLResult:
    name = stmt.this.name
    if catalog.exists(db, name) or catalog.get_matview(db, name) is not None:
        raise errors.SQLError("42P07", f'relation "{name}" already exists')
    definition = stmt.expression.sql(dialect="postgres")
    storage.create_collection(db, name)
    # Run the SELECT for its column shape either way; only write rows for WITH
    # DATA (the default). A WITH NO DATA matview is registered but unpopulated —
    # querying it errors until the first REFRESH.
    inner = sqlglot.parse_one(definition, read="postgres")
    result = _run_query(inner, storage, db, catalog, session)
    # Register the snapshot's shape as a catalog table so ``SELECT *`` projects the
    # SELECT's output columns (not the storage-assigned _id), and flag it a matview
    # so pg_class reports relkind 'm'.
    catalog.put(db, _matview_table_def(name, result))
    if populate:
        docs = _matview_docs(result)
        if docs:
            storage.insert(db, name, docs)
    catalog.put_matview(db, name, definition, populated=populate)
    if populate:
        return SQLResult(command_tag=f"SELECT {len(result.rows)}", rowcount=len(result.rows))
    return SQLResult(command_tag="CREATE MATERIALIZED VIEW")


def _refresh_matview(stmt: exp.Command, storage: Any, db: str, catalog, session) -> SQLResult:
    # The Command's argument is the raw tail text (e.g. "MATERIALIZED VIEW mv" or
    # "MATERIALIZED VIEW CONCURRENTLY mv"); a Literal carries it verbatim.
    arg = stmt.expression
    text = str(arg.this if isinstance(arg, exp.Literal) else arg)
    m = _MATVIEW_NAME_RE.match(text)
    if m is None:
        raise errors.feature_not_supported(f"unsupported REFRESH: {stmt.sql()}")
    name = m.group(1).strip().strip('"')
    concurrently = re.search(r"(?i)\bCONCURRENTLY\b", text) is not None
    definition = catalog.get_matview(db, name)
    if definition is None:
        raise errors.SQLError("42P01", f'materialized view "{name}" does not exist')
    if concurrently:
        # Postgres requires CONCURRENTLY to diff against an existing snapshot keyed
        # by a unique index, so it rejects an unpopulated view and one with no
        # unique index (same 0A000 + hint mongod-side clients would see).
        if not catalog.matview_populated(db, name):
            raise errors.feature_not_supported(
                f'CONCURRENTLY cannot be used on materialized view "{name}" '
                "before it has been populated by a non-concurrent REFRESH"
            )
        if not any(ix.get("unique") for ix in storage.list_indexes(db, name)):
            raise errors.feature_not_supported(
                f'cannot refresh materialized view "{name}" concurrently\n'
                "HINT: Create a unique index with no WHERE clause on one or more "
                "columns of the materialized view."
            )
    _materialize(name, definition, storage, db, catalog, session)
    catalog.set_matview_populated(db, name, True)  # a WITH NO DATA matview is now scannable
    return SQLResult(command_tag="REFRESH MATERIALIZED VIEW")


_MATVIEW_CREATE_RE = re.compile(
    r"(?is)^\s*MATERIALIZED\s+VIEW\s+(.*?)\s+WITH\s+(NO\s+)?DATA\s*;?\s*$"
)
_MATVIEW_ALTER_RE = re.compile(
    r"(?is)^\s*MATERIALIZED\s+VIEW\s+(?:IF\s+EXISTS\s+)?(.+?)\s+RENAME\s+TO\s+(.+?)\s*;?\s*$"
)


def _command_text(stmt: exp.Command) -> str:
    arg = stmt.expression
    return arg.this if isinstance(arg, exp.Literal) else str(arg or "")


_ALTER_TABLE_MULTI_RE = re.compile(
    r"(?is)^\s*ALTER\s+TABLE\s+(?P<ifexists>IF\s+EXISTS\s+)?"
    r'(?P<name>"[^"]+"|[A-Za-z_][\w$]*)\s+(?P<rest>.+?)\s*;?\s*$'
)


def _split_top_level_commas(s: str) -> list[str]:
    """Split ``s`` on commas that are outside any parentheses/brackets and outside
    single/double quotes — the ALTER TABLE action separator."""
    parts: list[str] = []
    buf: list[str] = []
    depth = 0
    in_sq = in_dq = False
    for ch in s:
        if in_sq:
            buf.append(ch)
            in_sq = ch != "'"
            continue
        if in_dq:
            buf.append(ch)
            in_dq = ch != '"'
            continue
        if ch == "'":
            in_sq = True
        elif ch == '"':
            in_dq = True
        elif ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        if ch == "," and depth == 0:
            parts.append("".join(buf))
            buf = []
            continue
        buf.append(ch)
    if buf:
        parts.append("".join(buf))
    return [p.strip() for p in parts if p.strip()]


def _run_mixed_alter_table(
    stmt: exp.Command, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Handle a mixed-kind multi-action ``ALTER TABLE`` (which sqlglot mis-parses as
    a Command) by splitting the action list, re-parsing each action as its own
    single-action ``ALTER TABLE`` (sqlglot handles any single action), then routing
    the combined ``exp.Alter`` through the normal single-ALTER path (#145)."""
    text = ("ALTER " + _command_text(stmt)).strip()
    m = _ALTER_TABLE_MULTI_RE.match(text)
    if m is None:
        raise errors.feature_not_supported(f"ALTER TABLE not supported: {text}")
    prefix = "ALTER TABLE " + ("IF EXISTS " if m.group("ifexists") else "") + m.group("name") + " "
    segments = _split_top_level_commas(m.group("rest"))
    combined: exp.Alter | None = None
    for seg in segments:
        pk_using = _ADD_PK_USING_INDEX_RE.match(seg)
        if pk_using is not None:
            if len(segments) != 1:
                raise errors.feature_not_supported(
                    "ADD PRIMARY KEY USING INDEX cannot be combined with other actions"
                )
            return _add_pk_using_index(
                _unquote_ident(m.group("name")),
                _unquote_ident(pk_using.group("index")),
                storage,
                db,
                catalog,
            )
        parsed = planner.parse(prefix + seg)
        node = parsed[0] if parsed else None
        if not isinstance(node, exp.Alter):
            raise errors.feature_not_supported(f"ALTER TABLE action not supported: {seg}")
        if combined is None:
            combined = node
        else:
            combined.args.setdefault("actions", []).extend(node.args.get("actions") or [])
    if combined is None:
        raise errors.feature_not_supported(f"ALTER TABLE not supported: {text}")
    return _run_statement(combined, storage, db, catalog, session)


_ADD_PK_USING_INDEX_RE = re.compile(
    r'(?is)^ADD\s+(?:CONSTRAINT\s+(?:"[^"]+"|\w+)\s+)?PRIMARY\s+KEY\s+'
    r'USING\s+INDEX\s+(?P<index>"[^"]+"|\w+)$'
)


def _add_pk_using_index(
    table_name: str, index_name: str, storage: Any, db: str, catalog: Catalog
) -> SQLResult:
    """``ALTER TABLE t ADD PRIMARY KEY USING INDEX idx`` — promote an existing
    unique index to the table's primary key (the constraint takes the index's
    name, like real PG). The key columns come from the index's key spec;
    INCLUDE columns stay non-key."""
    table = _require_table(catalog, db, table_name, storage)
    index = next(
        (ix for ix in storage.list_indexes(db, table.collection) if ix.get("name") == index_name),
        None,
    )
    if index is None:
        raise errors.SQLError(
            "42704", f'index "{index_name}" for table "{table_name}" does not exist'
        )
    if not index.get("unique"):
        raise errors.SQLError("42809", f'"{index_name}" is not a unique index')
    field_to_col = {c.field: c.name for c in table.columns}
    pk_cols = [field_to_col.get(f, f) for f in (index.get("key") or {})]
    executor._add_primary_key(table, pk_cols, index_name, storage, db)
    catalog.replace(db, table, old_name=table.name)
    return SQLResult(command_tag="ALTER TABLE")


# CREATE/DROP/ALTER ROLE | USER | GROUP arrive as a Command; the tail carries the
# name + attribute keywords. ``USER`` implies LOGIN (Postgres).
_ROLE_HEAD_RE = re.compile(
    r"(?is)^\s*(ROLE|USER|GROUP)\s+(?:IF\s+(NOT\s+)?EXISTS\s+)?"
    r'("[^"]+"|\w+)\s*(.*?)\s*;?\s*$'
)
# Boolean role attributes: keyword -> (field, value). NO<attr> negates.
_ROLE_FLAGS = {
    "LOGIN": ("login", True),
    "NOLOGIN": ("login", False),
    "SUPERUSER": ("superuser", True),
    "NOSUPERUSER": ("superuser", False),
    "CREATEDB": ("createdb", True),
    "NOCREATEDB": ("createdb", False),
    "CREATEROLE": ("createrole", True),
    "NOCREATEROLE": ("createrole", False),
    "INHERIT": ("inherit", True),
    "NOINHERIT": ("inherit", False),
    "REPLICATION": ("replication", True),
    "NOREPLICATION": ("replication", False),
}


def _parse_role_attrs(rest: str) -> dict[str, Any]:
    """Parse the attribute keywords after a role name (``LOGIN SUPERUSER PASSWORD
    'x' CONNECTION LIMIT 5``) into catalog fields. Unknown keywords are ignored
    (Postgres accepts a broad grammar; we record the ones that reflect)."""
    attrs: dict[str, Any] = {}
    tokens = rest.replace("WITH", " ").split()
    i = 0
    while i < len(tokens):
        tok = tokens[i].upper()
        if tok in _ROLE_FLAGS:
            field, value = _ROLE_FLAGS[tok]
            attrs[field] = value
        elif tok == "PASSWORD":
            attrs["password_set"] = True
            i += 1  # skip the password literal
        elif tok in ("CONNECTION", "LIMIT"):
            # CONNECTION LIMIT n
            if tok == "LIMIT" and i + 1 < len(tokens):
                try:
                    attrs["connlimit"] = int(tokens[i + 1])
                    i += 1
                except ValueError:
                    pass
        i += 1
    return attrs


def _grant_privileges(stmt: exp.Expression) -> tuple[list[str], dict[str, list[str]]] | None:
    """Split a ``GRANT``/``REVOKE``'s privileges into whole-table privileges and a
    per-column map (``GRANT SELECT (a, b)`` → ``{"a": ["SELECT"], ...}``). Returns
    None when any privilege isn't a recognised table privilege (schema/database
    grants etc. stay no-ops). ``ALL`` expands to every table privilege."""
    table_privs: list[str] = []
    column_privs: dict[str, list[str]] = {}
    for gp in stmt.args.get("privileges") or []:
        this = gp.this if isinstance(gp, exp.GrantPrivilege) else gp
        name = str(getattr(this, "name", this)).upper()
        if name in ("ALL", "ALL PRIVILEGES"):
            names = list(Catalog.TABLE_PRIVILEGES)
        elif name in Catalog.TABLE_PRIVILEGES:
            names = [name]
        else:
            return None
        cols = gp.args.get("expressions") if isinstance(gp, exp.GrantPrivilege) else None
        if cols:
            for c in cols:
                col = c.name if isinstance(c, exp.Column) else str(getattr(c, "name", c))
                column_privs.setdefault(col, []).extend(names)
        else:
            table_privs.extend(names)
    return table_privs, column_privs


def _grant_principals(stmt: exp.Expression) -> list[str]:
    """The grantee role names of a ``GRANT``/``REVOKE`` (``PUBLIC`` kept as-is).

    ``PUBLIC`` is a keyword in Postgres rather than a role name, and
    ``information_schema.role_table_grants`` reports it upper-case however it
    was spelled. Identifier folding lower-cases it like any other unquoted
    name, so it is restored here — which is what the "kept as-is" above has
    always promised.
    """
    out: list[str] = []
    for gp in stmt.args.get("principals") or []:
        ident = gp.this if isinstance(gp, exp.GrantPrincipal) else gp
        name = str(getattr(ident, "name", ident))
        out.append("PUBLIC" if name.lower() == "public" else name)
    return out


def _run_grant(
    stmt: exp.Expression,
    storage: Any,
    db: str,
    catalog: Catalog,
    session: Session | None = None,
    *,
    revoke: bool,
) -> SQLResult:
    """``GRANT``/``REVOKE`` <privs> ``ON`` <table> ``TO``/``FROM`` <role> ... —
    persist per-``(table, grantee)`` table privileges the authz gate enforces.

    Only object grants on a *table* for the SELECT/INSERT/UPDATE/DELETE (or ALL)
    privileges are recorded; anything else (schema/database grants, role
    membership, unsupported privileges) is accepted as a no-op, matching the
    prior permissive behaviour."""
    tag = "REVOKE" if revoke else "GRANT"
    securable = stmt.args.get("securable")
    parsed = _grant_privileges(stmt)
    if not isinstance(securable, exp.Table) or parsed is None:
        return SQLResult(command_tag=tag)  # not a recorded table privilege — no-op
    table_privs, column_privs = parsed
    table_name = securable.name
    # The table must exist (declared or reflectable) — mirrors CREATE INDEX.
    if catalog.get(db, table_name) is None and reflect.reflect(storage, db, table_name) is None:
        raise errors.undefined_table(table_name)
    owner = getattr(session, "user", None) if session is not None else None
    for grantee in _grant_principals(stmt):
        if table_privs:
            if revoke:
                catalog.revoke_table_privileges(db, table_name, grantee, table_privs)
            else:
                catalog.grant_table_privileges(
                    db,
                    table_name,
                    grantee,
                    table_privs,
                    grant_option=bool(stmt.args.get("grant_option")),
                )
            # Any table-level grant/revoke MATERIALIZES the relation's ACL
            # (relacl flips from NULL to an aclitem array). The owner's implicit
            # privileges become explicit and adjust when the operation targets
            # the owner — so ``REVOKE ALL … FROM <owner>`` leaves the owner with
            # nothing (pg's getTablePrivileges then reports no rows).
            if owner is not None:
                state = catalog._relation_acl_state(db, table_name)
                held = (
                    {p.upper() for p in state["owner_privs"]}
                    if state is not None
                    else set(catalog.TABLE_PRIVILEGES)
                )
                if grantee.lower() == owner.lower():
                    delta = {p.upper() for p in table_privs}
                    held = (held - delta) if revoke else (held | delta)
                catalog.materialize_relation_owner_privileges(db, table_name, owner, sorted(held))
        for column, privs in column_privs.items():
            if revoke:
                catalog.revoke_column_privileges(db, table_name, grantee, column, privs)
            else:
                catalog.grant_column_privileges(db, table_name, grantee, column, privs)
    return SQLResult(command_tag=tag)


def _apply_rls_read(stmt: exp.Expression, catalog: Catalog, db: str, session: Session) -> None:
    """Inject the RLS USING predicate into a single-table SELECT / UPDATE / DELETE
    WHERE. Multi-table SELECTs (joins) are left untouched (documented limitation)."""
    if not rls.enforced(session):
        return
    if isinstance(stmt, exp.Select):
        if stmt.args.get("joins"):
            return
        frm = stmt.args.get("from_") or stmt.args.get("from")
        tbl = frm.this if frm is not None else None
    elif isinstance(stmt, (exp.Update, exp.Delete)):
        tbl = stmt.find(exp.Table)
    else:
        return
    if isinstance(tbl, exp.Table):
        rls.apply_read(stmt, tbl.name, catalog, db, session)


# -- Row-level security (RLS) DDL: ALTER TABLE … ROW LEVEL SECURITY, CREATE /
#    DROP POLICY. All arrive as an ``exp.Command`` (sqlglot has no dedicated node),
#    so the tail is regex-parsed. #129
_RLS_ALTER_RE = re.compile(
    r'(?is)^\s*TABLE\s+(?:ONLY\s+)?(?:IF\s+EXISTS\s+)?("[^"]+"|[\w.]+)\s+'
    r"(ENABLE|DISABLE|NO\s+FORCE|FORCE)\s+ROW\s+LEVEL\s+SECURITY\s*;?\s*$"
)
_CREATE_POLICY_RE = re.compile(
    r'(?is)^\s*POLICY\s+(?P<name>"[^"]+"|\w+)\s+ON\s+(?P<table>"[^"]+"|[\w.]+)\s*(?P<rest>.*)$'
)
_DROP_POLICY_RE = re.compile(
    r'(?is)^\s*POLICY\s+(?:IF\s+EXISTS\s+)?(?P<name>"[^"]+"|\w+)\s+ON\s+'
    r'(?P<table>"[^"]+"|[\w.]+)\s*;?\s*$'
)


def _unquote_name(text: str) -> str:
    text = text.strip().strip('"')
    return text.rsplit(".", 1)[-1]  # drop any schema qualifier


def _paren_after(text: str, keyword: str) -> str | None:
    """The balanced-parenthesis substring following ``keyword`` (e.g. ``USING`` /
    ``WITH CHECK``) in ``text``, or None if the keyword isn't present."""
    m = re.search(r"(?i)\b" + keyword + r"\s*\(", text)
    if m is None:
        return None
    start = m.end() - 1  # index of the opening '('
    depth = 0
    for j in range(start, len(text)):
        if text[j] == "(":
            depth += 1
        elif text[j] == ")":
            depth -= 1
            if depth == 0:
                return text[start + 1 : j].strip()
    return None


def _alter_rls_command(stmt: exp.Command, storage: Any, db: str, catalog: Catalog) -> SQLResult:
    m = _RLS_ALTER_RE.match(_command_text(stmt))
    if m is None:
        raise errors.feature_not_supported(f"unsupported RLS command: {stmt.sql()}")
    table = _unquote_name(m.group(1))
    if catalog.get(db, table) is None and reflect.reflect(storage, db, table) is None:
        raise errors.undefined_table(table)
    action = re.sub(r"\s+", " ", m.group(2).upper())
    state = catalog.get_rls(db, table)
    if action == "ENABLE":
        state["enabled"] = True
    elif action == "DISABLE":
        state["enabled"] = False
    elif action == "FORCE":
        state["forced"] = True
    elif action == "NO FORCE":
        state["forced"] = False
    catalog.set_rls(db, table, enabled=state["enabled"], forced=state["forced"])
    return SQLResult(command_tag="ALTER TABLE")


def _create_policy_command(stmt: exp.Command, storage: Any, db: str, catalog: Catalog) -> SQLResult:
    text = _command_text(stmt)
    m = _CREATE_POLICY_RE.match(text)
    if m is None:
        raise errors.feature_not_supported(f"unsupported CREATE POLICY: {stmt.sql()}")
    name, table = _unquote_name(m.group("name")), _unquote_name(m.group("table"))
    if catalog.get(db, table) is None and reflect.reflect(storage, db, table) is None:
        raise errors.undefined_table(table)
    rest = m.group("rest")
    permissive = re.search(r"(?i)\bAS\s+RESTRICTIVE\b", rest) is None
    cmd_m = re.search(r"(?i)\bFOR\s+(ALL|SELECT|INSERT|UPDATE|DELETE)\b", rest)
    command = cmd_m.group(1).upper() if cmd_m else "ALL"
    roles_m = re.search(r"(?i)\bTO\s+(.+?)(?=\s+USING\b|\s+WITH\s+CHECK\b|$)", rest)
    if roles_m:
        roles = [r.strip().strip('"') for r in roles_m.group(1).split(",") if r.strip()]
    else:
        roles = ["public"]
    doc = {
        "name": name,
        "table": table,
        "command": command,
        "roles": roles,
        "permissive": permissive,
        "using": _paren_after(rest, "USING"),
        "check": _paren_after(rest, r"WITH\s+CHECK"),
    }
    catalog.create_policy(db, doc)
    return SQLResult(command_tag="CREATE POLICY")


def _drop_policy_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    text = _command_text(stmt)
    m = _DROP_POLICY_RE.match(text)
    if m is None:
        raise errors.feature_not_supported(f"unsupported DROP POLICY: {stmt.sql()}")
    name, table = _unquote_name(m.group("name")), _unquote_name(m.group("table"))
    if not catalog.drop_policy(db, table, name) and "IF EXISTS" not in text.upper():
        raise errors.SQLError("42704", f'policy "{name}" for table "{table}" does not exist')
    return SQLResult(command_tag="DROP POLICY")


def _run_truncate(stmt: exp.TruncateTable, storage: Any, db: str, catalog: Catalog) -> SQLResult:
    """``TRUNCATE [TABLE] t [, …] [RESTART | CONTINUE IDENTITY] [CASCADE | RESTRICT]``
    (#133) — empty each table fast. ``RESTART IDENTITY`` resets owned sequences;
    ``CASCADE`` also truncates referencing tables (transitive), while the default
    ``RESTRICT`` errors if a table is referenced from outside the truncate set."""
    exists = bool(stmt.args.get("exists"))  # TRUNCATE … IF EXISTS
    named: list[str] = []
    for t in stmt.args.get("expressions") or []:
        name = planner.qualified_table_name(t)
        if catalog.get(db, name) is None and reflect.reflect(storage, db, name) is None:
            if exists:
                continue
            raise errors.undefined_table(name)
        named.append(name)

    restart = str(stmt.args.get("identity") or "").upper() == "RESTART"
    cascade = str(stmt.args.get("option") or "").upper() == "CASCADE"
    to_truncate: set[str] = set(named)
    if cascade:
        queue = list(named)
        while queue:
            parent = queue.pop()
            for child, _fk in executor._referencing_fks(catalog, db, parent):
                if child.name not in to_truncate:
                    to_truncate.add(child.name)
                    queue.append(child.name)
    else:  # RESTRICT (default): a reference from outside the set is an error.
        for parent in named:
            for child, _fk in executor._referencing_fks(catalog, db, parent):
                if child.name not in to_truncate:
                    raise errors.SQLError(
                        "0A000",
                        f"cannot truncate a table referenced in a foreign key constraint\n"
                        f'DETAIL: Table "{child.name}" references "{parent}".',
                    )

    for name in to_truncate:
        tdef = catalog.get(db, name) or reflect.reflect(storage, db, name)
        storage.delete_matching(db, tdef.collection, {})
        if restart:
            for col in tdef.columns:
                if col.sequence and catalog.sequence_exists(db, col.sequence):
                    catalog.alter_sequence(db, col.sequence, {"restart": None})
    return SQLResult(command_tag="TRUNCATE TABLE")


def _run_role_command(verb: str, stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    """``CREATE`` / ``DROP`` / ``ALTER`` ``ROLE`` | ``USER`` | ``GROUP``. Roles are
    recorded in the catalog for reflection (``pg_roles``); ``USER`` implies LOGIN."""
    text = _command_text(stmt)
    m = _ROLE_HEAD_RE.match(text)
    if m is None:
        raise errors.feature_not_supported(f"unsupported role command: {stmt.sql()}")
    kind, not_exists, name, rest = m.group(1).upper(), m.group(2), m.group(3).strip('"'), m.group(4)
    tag = f"{verb} {kind}"
    if verb == "DROP":
        if (
            not catalog.drop_role(db, name)
            and not_exists is None
            and "IF EXISTS" not in text.upper()
        ):
            raise errors.SQLError("42704", f'role "{name}" does not exist')
        return SQLResult(command_tag=tag)
    attrs = _parse_role_attrs(rest)
    if kind == "USER":
        attrs.setdefault("login", True)  # CREATE USER == CREATE ROLE ... LOGIN
    if verb == "ALTER":
        existing = catalog.get_role(db, name)
        if existing is None:
            raise errors.SQLError("42704", f'role "{name}" does not exist')
        merged = {k: existing[k] for k in catalog.ROLE_DEFAULTS if k in existing}
        merged.update(attrs)
        catalog.put_role(db, name, merged)
        return SQLResult(command_tag=tag)
    if catalog.role_exists(db, name):
        raise errors.SQLError("42710", f'role "{name}" already exists')
    catalog.put_role(db, name, attrs)
    return SQLResult(command_tag=tag)


# Role membership: ``GRANT <roles> TO <members> [WITH ADMIN OPTION]`` /
# ``REVOKE [ADMIN OPTION FOR] <roles> FROM <members> [CASCADE|RESTRICT]`` (#138).
_GRANT_MEMBER_RE = re.compile(
    r"(?is)^\s*(?P<roles>.+?)\s+TO\s+(?P<members>.+?)"
    r"(?:\s+WITH\s+ADMIN\s+OPTION)?\s*;?\s*$"
)
_REVOKE_MEMBER_RE = re.compile(
    r"(?is)^\s*(?:ADMIN\s+OPTION\s+FOR\s+)?(?P<roles>.+?)\s+FROM\s+(?P<members>.+?)"
    r"(?:\s+(?:CASCADE|RESTRICT))?\s*;?\s*$"
)


def _split_role_names(text: str) -> list[str]:
    return [_unquote_ident(n.strip()) for n in text.split(",") if n.strip()]


def _run_role_membership(
    verb: str, stmt: exp.Command, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``GRANT``/``REVOKE`` <role> ``TO``/``FROM`` <member> — role membership (#138).
    A privilege grant that fell back to a Command (it carries an ``ON`` target) is
    accepted as a no-op, preserving prior behaviour."""
    tail = _command_text(stmt)
    if re.search(r"(?is)\bON\b", tail):
        return SQLResult(command_tag=verb)  # privilege grant (not membership) — no-op
    if verb == "GRANT":
        m = _GRANT_MEMBER_RE.match(tail)
        if m is None:
            return SQLResult(command_tag=verb)
        admin = re.search(r"(?is)\bWITH\s+ADMIN\s+OPTION\s*;?\s*$", tail) is not None
        for role in _split_role_names(m.group("roles")):
            for member in _split_role_names(m.group("members")):
                catalog.grant_role_membership(db, role, member, admin_option=admin)
        return SQLResult(command_tag="GRANT ROLE")
    m = _REVOKE_MEMBER_RE.match(tail)
    if m is None:
        return SQLResult(command_tag=verb)
    # REVOKE ADMIN OPTION FOR clears just the admin option; a plain REVOKE removes
    # the membership.
    admin_only = re.match(r"(?is)^\s*ADMIN\s+OPTION\s+FOR\b", tail) is not None
    for role in _split_role_names(m.group("roles")):
        for member in _split_role_names(m.group("members")):
            if admin_only:
                catalog.revoke_role_admin_option(db, role, member)
            else:
                catalog.revoke_role_membership(db, role, member)
    return SQLResult(command_tag="REVOKE ROLE")


_SET_CONSTRAINTS_RE = re.compile(r"(?is)^\s*CONSTRAINTS\s+(.+?)\s+(DEFERRED|IMMEDIATE)\s*;?\s*$")


def _set_constraints_command(
    stmt: exp.Command, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``SET CONSTRAINTS { ALL | name [, ...] } { DEFERRED | IMMEDIATE }`` — set the
    per-session deferral mode. Switching to IMMEDIATE re-checks the affected
    deferred constraints right away (a surviving violation raises, poisoning the
    block, as Postgres does)."""
    m = _SET_CONSTRAINTS_RE.match(_command_text(stmt))
    if m is None:
        raise errors.feature_not_supported(f"unsupported SET CONSTRAINTS: {stmt.sql()}")
    targets, mode = m.group(1).strip(), m.group(2).upper()
    deferred = mode == "DEFERRED"
    if targets.upper() == "ALL":
        session.deferred_all = deferred
        if not deferred and session.pending_deferred:
            executor.flush_deferred(session, storage, db, catalog)
    else:
        names = {t.strip().strip('"') for t in targets.split(",") if t.strip()}
        for name in names:
            session.deferred_names[name] = deferred
        if not deferred and session.pending_deferred:
            executor.flush_deferred(session, storage, db, catalog, names=names)
    return SQLResult(command_tag="SET CONSTRAINTS")


def _create_matview_command(
    stmt: exp.Command, storage: Any, db: str, catalog, session
) -> SQLResult:
    """``CREATE MATERIALIZED VIEW … WITH [NO] DATA`` — sqlglot can't parse the WITH
    clause, so it arrives as a Command. Strip the suffix, re-parse the bare CREATE,
    and delegate with the populate flag."""
    text = _command_text(stmt)
    m = _MATVIEW_CREATE_RE.match(text)
    if m is None:
        return _run_command(stmt, session)
    populate = m.group(2) is None  # "WITH NO DATA" → don't populate
    inner = sqlglot.parse_one(f"CREATE MATERIALIZED VIEW {m.group(1)}", read="postgres")
    return _create_matview(inner, storage, db, catalog, session, populate=populate)


def _alter_matview_command(stmt: exp.Command, storage: Any, db: str, catalog, session) -> SQLResult:
    """``ALTER MATERIALIZED VIEW name RENAME TO newname`` (arrives as a Command)."""
    m = _MATVIEW_ALTER_RE.match(_command_text(stmt))
    if m is None:
        return _run_command(stmt, session)
    old = m.group(1).strip().strip('"')
    new = m.group(2).strip().strip('"')
    definition = catalog.get_matview(db, old)
    if definition is None:
        raise errors.SQLError("42P01", f'materialized view "{old}" does not exist')
    if catalog.exists(db, new) or catalog.get_matview(db, new) is not None:
        raise errors.SQLError("42P07", f'relation "{new}" already exists')
    ok, err = storage.rename_collection(db, old, db, new)
    if not ok:
        raise errors.SQLError("42P07", err or f'relation "{new}" already exists')
    populated = catalog.matview_populated(db, old)
    table = catalog.get(db, old)
    if table is not None:
        table.name, table.collection = new, new
        catalog.replace(db, table, old_name=old)
    catalog.drop_matview(db, old)
    catalog.put_matview(db, new, definition, populated=populated)
    return SQLResult(command_tag="ALTER MATERIALIZED VIEW")


def _drop_matview(stmt: exp.Drop, storage: Any, db: str, catalog) -> SQLResult:
    name = stmt.this.name
    if not catalog.drop_matview(db, name):
        if stmt.args.get("exists"):
            return SQLResult(command_tag="DROP MATERIALIZED VIEW")
        raise errors.SQLError("42P01", f'materialized view "{name}" does not exist')
    catalog.drop(db, name)
    storage.drop_collection(db, name)
    return SQLResult(command_tag="DROP MATERIALIZED VIEW")


def _seq_prop_int(props: Any, key: str) -> int | None:
    """Read an int-valued ``SequenceProperties`` field (start / increment / …)."""
    if props is None:
        return None
    for e in props.expressions:
        if isinstance(e, exp.SequenceProperties):
            val = e.args.get(key)
            if isinstance(val, exp.Literal):
                return int(val.this)
            if isinstance(val, exp.Neg) and isinstance(val.this, exp.Literal):
                return -int(val.this.this)
            # ``NO MINVALUE`` / ``NO MAXVALUE`` parse as a non-literal node —
            # "no bound" is the default, exactly what None means here.
            return None
    return None


def _seq_has_cycle(props: Any) -> bool:
    if props is None:
        return False
    for e in props.expressions:
        if isinstance(e, exp.SequenceProperties):
            for opt in e.args.get("options") or []:
                if str(getattr(opt, "this", opt)).upper() == "CYCLE":
                    return True
    return False


def _create_sequence(stmt: exp.Create, db: str, catalog: Catalog) -> SQLResult:
    """``CREATE SEQUENCE [IF NOT EXISTS] name [START WITH n] [INCREMENT BY n]
    [MINVALUE n] [MAXVALUE n] [CYCLE]``."""
    name = planner.qualified_table_name(stmt.this)
    if catalog.sequence_exists(db, name):
        if stmt.args.get("exists"):
            return SQLResult(command_tag="CREATE SEQUENCE")
        raise errors.SQLError("42P07", f'relation "{name}" already exists')
    props = stmt.args.get("properties")
    increment = _seq_prop_int(props, "increment") or 1
    start = _seq_prop_int(props, "start")
    if start is None:
        start = 1 if increment > 0 else -1
    catalog.create_sequence(
        db,
        name,
        start=start,
        increment=increment,
        minvalue=_seq_prop_int(props, "minvalue"),
        maxvalue=_seq_prop_int(props, "maxvalue"),
        cycle=_seq_has_cycle(props),
    )
    return SQLResult(command_tag="CREATE SEQUENCE")


def _drop_sequence(stmt: exp.Drop, db: str, catalog: Catalog) -> SQLResult:
    name = planner.qualified_table_name(stmt.this)
    if not catalog.drop_sequence(db, name) and not stmt.args.get("exists"):
        raise errors.SQLError("42P01", f'sequence "{name}" does not exist')
    return SQLResult(command_tag="DROP SEQUENCE")


_BUILTIN_SCHEMAS = ("public", "pg_catalog", "information_schema")


def _create_schema(stmt: exp.Create, db: str, catalog: Catalog) -> SQLResult:
    """``CREATE SCHEMA [IF NOT EXISTS] name`` — a namespace for user-declared
    types (schema-qualified tables stay 0A000 for now)."""
    name = stmt.this.args["db"].name
    if name in _BUILTIN_SCHEMAS or catalog.schema_exists(db, name):
        if stmt.args.get("exists"):  # IF NOT EXISTS
            return SQLResult(command_tag="CREATE SCHEMA")
        raise errors.SQLError("42P06", f'schema "{name}" already exists')
    catalog.create_schema(db, name)
    return SQLResult(command_tag="CREATE SCHEMA")


def _drop_schema(stmt: exp.Drop, db: str, catalog: Catalog, storage: Any = None) -> SQLResult:
    """``DROP SCHEMA [IF EXISTS] name [CASCADE]`` — CASCADE drops the schema's
    types; without it, a non-empty schema is a 2BP01 dependency error."""
    name = stmt.this.args["db"].name
    if not catalog.schema_exists(db, name):
        if stmt.args.get("exists"):
            return SQLResult(command_tag="DROP SCHEMA")
        raise errors.SQLError("3F000", f'schema "{name}" does not exist')
    prefix = f"{name}."
    enums = [n for n in catalog.list_enums(db) if n.startswith(prefix)]
    composites = [n for n in catalog.list_composites(db) if n.startswith(prefix)]
    domains = [n for n in catalog.list_domains(db) if n.startswith(prefix)]
    tables = [n for n in catalog.list_tables(db) if n.startswith(prefix)]
    if (enums or composites or domains or tables) and not stmt.args.get("cascade"):
        raise errors.SQLError(
            "2BP01", f'cannot drop schema "{name}" because other objects depend on it'
        )
    for n in enums:
        catalog.drop_enum(db, n)
    for n in composites:
        catalog.drop_composite(db, n)
    for n in domains:
        catalog.drop_domain(db, n)
    for n in tables:
        executor.execute_drop_table(
            planner.DropTablePlan(name=n, if_exists=True), catalog, storage, db
        )
    catalog.drop_schema(db, name)
    return SQLResult(command_tag="DROP SCHEMA")


def _qualified_type_name(node: exp.Expression, db: str, catalog: Catalog) -> str:
    """The (possibly schema-qualified) name a CREATE/DROP TYPE targets. Types in
    a user schema are stored under their dotted name; ``public.`` is the
    default namespace and stays bare. An unknown schema is a 3F000 error."""
    schema_id = node.args.get("db")
    if schema_id is None:
        return node.name
    schema = schema_id.name
    if schema == "public":
        return node.name
    if not catalog.schema_exists(db, schema):
        raise errors.SQLError("3F000", f'schema "{schema}" does not exist')
    return f"{schema}.{node.name}"


def _create_type(stmt: exp.Create, db: str, catalog: Catalog) -> SQLResult:
    """``CREATE TYPE name AS ENUM ('a', 'b', …)`` records the enum's label list;
    ``CREATE TYPE name AS (field type, …)`` records a composite type's ordered
    fields. A schema-qualified name stores under its dotted form. Range / base
    types are not supported."""
    name = _qualified_type_name(stmt.this, db, catalog)
    expr = stmt.args.get("expression")
    # Composite form: sqlglot parses the ``(field type, …)`` body as a Schema of
    # ColumnDefs.
    if isinstance(expr, exp.Schema):
        _check_type_name_free(catalog, db, name)
        fields = _composite_fields_from_schema(expr, name, catalog, db)
        catalog.create_composite(db, name, fields)
        return SQLResult(command_tag="CREATE TYPE")
    if not (isinstance(expr, exp.DataType) and expr.this and expr.this.name == "ENUM"):
        raise errors.feature_not_supported(
            "only CREATE TYPE … AS ENUM and CREATE TYPE … AS (…) are supported "
            "(range / base types are not)"
        )
    _check_type_name_free(catalog, db, name)
    labels = [e.this if isinstance(e, exp.Literal) else str(e) for e in expr.expressions]
    catalog.create_enum(db, name, labels)
    return SQLResult(command_tag="CREATE TYPE")


def _check_type_name_free(catalog: Catalog, db: str, name: str) -> None:
    if (
        catalog.enum_exists(db, name)
        or catalog.composite_exists(db, name)
        or catalog.domain_exists(db, name)
    ):
        raise errors.SQLError("42710", f'type "{name}" already exists')


def _composite_fields_from_schema(
    schema: exp.Schema, type_name: str, catalog: Catalog, db: str
) -> list[tuple]:
    """Build a composite type's ordered fields from ``CREATE TYPE t AS (…)``. A
    field whose type is a builtin gets ``(name, tag, None)``; a field whose type is
    another (already-declared) composite gets ``(name, subtype_name, subfields)``
    with the referenced type's fields embedded (resolved once, since composite
    types don't support ALTER)."""
    fields: list[tuple] = []
    for coldef in schema.expressions:
        if not isinstance(coldef, exp.ColumnDef):
            raise errors.feature_not_supported(
                f"unsupported composite type element: {coldef.sql()}"
            )
        kind = coldef.args["kind"]
        tag = typemap.type_tag_for_sql(kind)
        if tag is not None:
            fields.append((coldef.name, tag, None))
            continue
        # A non-builtin field type may be another composite (nested composite).
        subtype = kind.sql(dialect="postgres").lower().strip().strip('"')
        if subtype == type_name.lower():
            raise errors.feature_not_supported(
                f'composite type "{type_name}" cannot contain itself'
            )
        sub = catalog.get_composite(db, subtype)
        if sub is not None:
            fields.append((coldef.name, subtype, tuple(sub)))
            continue
        raise errors.feature_not_supported(
            f'unsupported field type in composite type "{type_name}": {kind.sql()}'
        )
    return fields


# ``CREATE/DROP EXTENSION`` — Command tails sqlglot can't parse. SecantusDB
# ships the functionality of a few extensions built in (citext, hstore, and
# plpgsql, which real Postgres preinstalls), so installing them is a no-op
# that succeeds; anything else is honestly unavailable (0A000, so driver
# suites that probe with CREATE EXTENSION read it as a skippable gap rather
# than a failure). The WITH SCHEMA / VERSION / CASCADE tail is accepted and
# ignored — there is no schema placement or versioning to do.
_AVAILABLE_EXTENSIONS = frozenset({"citext", "hstore", "plpgsql"})

_EXTENSION_RE = re.compile(
    r'(?is)^\s*EXTENSION\s+(?:(?P<ifclause>IF\s+(?:NOT\s+)?EXISTS)\s+)?"?(?P<name>[\w-]+)"?'
)


def _create_extension_command(stmt: exp.Command) -> SQLResult:
    m = _EXTENSION_RE.match(_command_text(stmt))
    if m is None:
        raise errors.syntax_error(f"unparseable CREATE EXTENSION: {stmt.sql()}")
    name = m.group("name").lower()
    if name not in _AVAILABLE_EXTENSIONS:
        raise errors.feature_not_supported(f'extension "{name}" is not available')
    return SQLResult(command_tag="CREATE EXTENSION")


_COMMENT_DOMAIN_RE = re.compile(
    r"(?is)^ON\s+DOMAIN\s+(?P<name>\"[^\"]+\"|\w+)\s+IS\s+(?P<value>NULL|'(?:[^']|'')*')\s*;?\s*$"
)


def _comment_domain_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    """``COMMENT ON DOMAIN d IS '…'`` (sqlglot Command fallback) — store on the
    domain doc; surfaces via pg_description (classoid pg_type) and
    obj_description, which is how pgjdbc's getUDTs reads REMARKS."""
    m = _COMMENT_DOMAIN_RE.match(_command_text(stmt).strip())
    if m is None:
        raise errors.syntax_error(f"unparseable COMMENT ON DOMAIN: {stmt.sql()}")
    name = _unquote_ident(m.group("name")).lower()
    raw = m.group("value")
    text = None if raw.upper() == "NULL" else raw[1:-1].replace("''", "'")
    if text == planner.UNCOMMENT_SENTINEL:
        # planner.parse rewrites ``IS NULL`` into this sentinel literal so the
        # exp.Comment path can tell removal from absence; decode it here too.
        text = None
    if not catalog.set_domain_comment(db, name, text):
        raise errors.SQLError("42704", f'type "{name}" does not exist')
    return SQLResult(command_tag="COMMENT")


def _comment_constraint_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    """``COMMENT ON CONSTRAINT c ON t IS '…'`` — store the comment on the
    named check / unique / foreign-key / primary-key constraint (``IS NULL``
    removes it). Routed here by ``planner.parse`` because sqlglot's Comment
    node can't express the two-name form."""
    import dataclasses

    raw = str(stmt.expression.this)
    m = planner.COMMENT_CONSTRAINT_RE.match(raw)
    assert m is not None  # gated by the dispatcher's parse
    cname = m.group("name").strip('"')
    parts = [seg.strip('"') for seg in m.group("table").split(".")]
    # Same key scheme as everywhere: public stays bare, other schemas dotted.
    tname = parts[-1] if len(parts) == 1 or parts[0] == "public" else ".".join(parts)
    value = m.group("value")
    text = None if value.upper() == "NULL" else value[1:-1].replace("''", "'")
    table = catalog.get(db, tname)
    if table is None:
        raise errors.undefined_table(tname)
    for attr in ("check_constraints", "unique_constraints", "foreign_keys"):
        cons = getattr(table, attr)
        if any(c.name == cname for c in cons):
            updated = [dataclasses.replace(c, comment=text) if c.name == cname else c for c in cons]
            catalog.replace(db, dataclasses.replace(table, **{attr: updated}))
            return SQLResult(command_tag="COMMENT")
    if table.pk_columns and cname == table.pk_constraint_name():
        catalog.replace(db, dataclasses.replace(table, pk_comment=text))
        return SQLResult(command_tag="COMMENT")
    raise errors.SQLError("42704", f'constraint "{cname}" for table "{tname}" does not exist')


def _drop_extension_command(stmt: exp.Command) -> SQLResult:
    m = _EXTENSION_RE.match(_command_text(stmt))
    if m is None:
        raise errors.syntax_error(f"unparseable DROP EXTENSION: {stmt.sql()}")
    name = m.group("name").lower()
    if name not in _AVAILABLE_EXTENSIONS:
        if m.group("ifclause") is not None:
            return SQLResult(command_tag="DROP EXTENSION")
        raise errors.SQLError("42704", f'extension "{name}" does not exist')
    return SQLResult(command_tag="DROP EXTENSION")


_OPERATOR_NAME = r"[+\-*/<>=~!@#%^&|`?]+"
_CREATE_OPERATOR_RE = re.compile(
    rf"^OPERATOR\s+(?P<name>{_OPERATOR_NAME})\s*\((?P<opts>.*)\)\s*;?\s*$",
    re.IGNORECASE | re.DOTALL,
)
_DROP_OPERATOR_RE = re.compile(
    rf"^OPERATOR\s+(?P<ifclause>IF\s+EXISTS\s+)?(?P<name>{_OPERATOR_NAME})\s*"
    r"\(\s*(?P<left>[^,]+?)\s*,\s*(?P<right>[^)]+?)\s*\)\s*(?:CASCADE|RESTRICT)?\s*;?\s*$",
    re.IGNORECASE,
)


def _create_operator_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    """``CREATE OPERATOR & (LEFTARG = numeric, RIGHTARG = integer, PROCEDURE =
    f6)`` — registered in the catalog so the DDL round-trips (pgjdbc's
    DatabaseMetaDataTest creates one in setup); expression evaluation does not
    consult user operators."""
    m = _CREATE_OPERATOR_RE.match(_command_text(stmt).strip())
    if m is None:
        raise errors.syntax_error(f"unparseable CREATE OPERATOR: {stmt.sql()}")
    opts: dict[str, str] = {}
    for part in m.group("opts").split(","):
        if "=" in part:
            k, v = part.split("=", 1)
            opts[k.strip().lower()] = v.strip()
    proc = opts.get("procedure") or opts.get("function")
    left, right = opts.get("leftarg"), opts.get("rightarg")
    if proc is None or left is None or right is None:
        raise errors.syntax_error("CREATE OPERATOR needs LEFTARG, RIGHTARG and PROCEDURE")
    if not any(f["name"] == proc.lower() and f["nargs"] == 2 for f in catalog.list_functions(db)):
        raise errors.SQLError("42883", f"function {proc}({left}, {right}) does not exist")
    catalog.put_operator(
        db,
        {"name": m.group("name"), "leftarg": left, "rightarg": right, "procedure": proc},
    )
    return SQLResult(command_tag="CREATE OPERATOR")


def _drop_operator_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    m = _DROP_OPERATOR_RE.match(_command_text(stmt).strip())
    if m is None:
        raise errors.syntax_error(f"unparseable DROP OPERATOR: {stmt.sql()}")
    name, left, right = m.group("name"), m.group("left"), m.group("right")
    if not catalog.drop_operator(db, name, left, right) and m.group("ifclause") is None:
        raise errors.SQLError("42883", f"operator does not exist: {left} {name} {right}")
    return SQLResult(command_tag="DROP OPERATOR")


# ``TYPE <name> AS RANGE (<options>)`` — the Command tail of a CREATE that
# sqlglot can't parse. Options are ``key = value`` pairs; only ``subtype``
# affects behaviour (collation / opclass / canonical are accepted, ignored).
_CREATE_RANGE_RE = re.compile(
    r'(?is)^\s*TYPE\s+((?:"[^"]+"|[\w$]+)(?:\.(?:"[^"]+"|[\w$]+))?)\s+AS\s+RANGE\s*\((.*)\)\s*;?\s*$'
)


def _create_range_type_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    m = _CREATE_RANGE_RE.match(_command_text(stmt))
    assert m is not None  # gated by the dispatcher's match
    from secantus.sql.catalog import fold_type_name

    name = fold_type_name(m.group(1))
    subtype_tag: str | None = None
    for part in m.group(2).split(","):
        if "=" not in part:
            continue
        key, _, value = part.partition("=")
        if key.strip().lower() == "subtype":
            spelling = value.strip().strip('"')
            oid = typemap.oid_for_regtype(spelling)
            subtype_tag = typemap.OID_TO_TAG.get(oid) if oid is not None else None
    if subtype_tag is None:
        raise errors.SQLError(
            "42704", f"CREATE TYPE … AS RANGE requires a recognised subtype: {m.group(2)!r}"
        )
    _check_type_name_free(catalog, db, name)
    if catalog.range_type_exists(db, name):
        raise errors.SQLError("42710", f'type "{name}" already exists')
    catalog.create_range_type(db, name, subtype_tag)
    return SQLResult(command_tag="CREATE TYPE")


def _drop_type(stmt: exp.Drop, db: str, catalog: Catalog) -> SQLResult:
    try:
        name = _qualified_type_name(stmt.this, db, catalog)
    except errors.SQLError:
        if stmt.args.get("exists"):  # DROP TYPE IF EXISTS tolerates a missing schema
            return SQLResult(command_tag="DROP TYPE")
        raise
    dropped = (
        catalog.drop_enum(db, name)
        or catalog.drop_composite(db, name)
        or catalog.drop_range_type(db, name)
    )
    if not dropped and not stmt.args.get("exists"):
        raise errors.SQLError("42704", f'type "{name}" does not exist')
    return SQLResult(command_tag="DROP TYPE")


def _function_body_text(node: exp.Expression | None) -> str:
    """The SQL text of a function body — a dollar-quoted ``$$…$$`` (parses as a
    ``Heredoc``) or a single-quoted string ``Literal``."""
    if node is None:
        return ""
    if isinstance(node, exp.Literal):
        return str(node.this)
    inner = node.this if isinstance(node, exp.Heredoc) else node
    return inner if isinstance(inner, str) else str(inner)


def _function_params(udf: exp.Expression) -> list[str | None]:
    """Parameter names of a ``CREATE FUNCTION`` signature; an unnamed
    (type-only) parameter contributes ``None`` so positional ``$N`` still lines up."""
    names: list[str | None] = []
    for p in udf.expressions or []:
        if isinstance(p, exp.ColumnDef):
            names.append(p.this.name)  # named param: name + type
        else:
            names.append(None)  # bare type (unnamed) — referenced only as $N
    return names


def _function_input_nargs(udf: exp.Expression) -> int:
    """The number of INPUT parameters (IN / INOUT / VARIADIC) — PG's function
    identity excludes OUT-only parameters, so ``f3(IN a int, INOUT b varchar,
    OUT c timestamptz)`` is ``f3(int, varchar)`` to DROP FUNCTION and callers."""
    n = 0
    for p in udf.expressions or []:
        out_only = False
        for c in p.args.get("constraints") or [] if isinstance(p, exp.ColumnDef) else []:
            if isinstance(c, exp.InOutColumnConstraint):
                out_only = bool(c.args.get("output")) and not bool(c.args.get("input_"))
        if not out_only:
            n += 1
    return n


def _function_param_types(udf: exp.Expression) -> list[str | None]:
    """Parameter type tags of a ``CREATE FUNCTION`` signature (positional), for
    ``pg_proc`` / ``information_schema.parameters`` reflection. Unknown → None."""
    types: list[str | None] = []
    for p in udf.expressions or []:
        dt = p.args.get("kind") if isinstance(p, exp.ColumnDef) else p
        types.append(typemap.type_tag_for_sql(dt) if isinstance(dt, exp.DataType) else None)
    return types


def _create_function(
    stmt: exp.Create, db: str, catalog: Catalog, session: Session | None = None
) -> SQLResult:
    """``CREATE [OR REPLACE] FUNCTION name(params) RETURNS t AS $$ body $$
    LANGUAGE sql`` — store the parsed body for the scalar evaluator to invoke."""
    udf = stmt.this
    name = udf.this.name
    # A pg_temp-homed function keys under the session's temp namespace (the
    # qualify pass already rewrote the ``pg_temp`` qualifier on the Table
    # node) — CREATE TRIGGER resolves ``pg_temp.fn()`` against the same key.
    fn_schema = udf.this.args.get("db")
    if fn_schema is not None and fn_schema.name.startswith("pg_temp_"):
        name = f"{fn_schema.name}.{name}"
    params = _function_params(udf)
    nargs = _function_input_nargs(udf)

    language = "sql"
    return_tag = None
    is_table = False
    returns_trigger = False
    for prop in stmt.args.get("properties").expressions if stmt.args.get("properties") else []:
        if isinstance(prop, exp.LanguageProperty):
            language = str(prop.this.name if hasattr(prop.this, "name") else prop.this).lower()
        elif isinstance(prop, exp.ReturnsProperty):
            is_table = bool(prop.args.get("is_table"))
            if isinstance(prop.this, exp.DataType):
                kind = prop.this.args.get("kind")
                if (
                    prop.this.this == exp.DataType.Type.USERDEFINED
                    and isinstance(kind, exp.Identifier)
                    and kind.name.lower() == "trigger"
                ):
                    # ``RETURNS trigger`` (planner pre-parse quotes it so
                    # sqlglot accepts the statement).
                    returns_trigger = True
                else:
                    return_tag = typemap.type_tag_for_sql(prop.this)

    if language == "c" and stmt.this.this.name.lower() == "lo_manage":
        # contrib/lo's orphan-cleanup trigger function, created verbatim by
        # clients that manage large objects (pgjdbc's BlobTransactionTest).
        # Accepted as a recognized no-op: skipping the cleanup only leaves
        # orphaned large objects behind, which nothing vacuums here anyway.
        # Every other LANGUAGE C function stays rejected.
        language = "sql"
        stmt.set("expression", exp.Literal.string("SELECT NULL"))
    elif language not in ("sql", "plpgsql"):
        raise errors.feature_not_supported(
            f"CREATE FUNCTION LANGUAGE {language} is not supported (only LANGUAGE sql / plpgsql)"
        )

    body = _function_body_text(stmt.expression).strip()
    if language == "plpgsql":
        # Validate the procedural body up front; the interpreter re-parses at call.
        from secantus.sql import plpgsql

        plpgsql.parse(body)
    else:
        parsed = planner.parse(body)
        if len(parsed) != 1:
            raise errors.feature_not_supported("a SQL function body must be a single statement")

    if not stmt.args.get("replace") and catalog.function_exists(db, name, nargs):
        raise errors.SQLError("42723", f'function "{name}" already exists with same argument types')
    catalog.put_function(
        db,
        {
            "name": name,
            "nargs": nargs,
            "params": params,
            "param_types": _function_param_types(udf),
            "return_tag": return_tag,
            "is_table": is_table,
            "body": body,
            "language": language,
            "returns_trigger": returns_trigger,
        },
    )
    return SQLResult(command_tag="CREATE FUNCTION")


_PROC_MODE_KW = {"in", "out", "inout", "variadic"}


def _parse_proc_params(params_text: str) -> list[dict]:
    """Parse a procedure parameter list into ``[{name, mode, type_tag}]``.
    Postgres accepts the argmode before OR after the name (``a INOUT int`` and
    ``INOUT a int`` are both valid); a bare ``type`` is an unnamed IN param."""
    out: list[dict] = []
    for part in _split_top_level_commas(params_text):
        part = part.strip()
        if not part:
            continue
        toks = part.split()
        mode = "IN"
        kept: list[str] = []
        for t in toks:
            if t.lower() in _PROC_MODE_KW and mode == "IN":
                mode = t.upper()
            else:
                kept.append(t)
        name = None
        type_toks = kept
        if len(kept) >= 2:
            name, type_toks = kept[0], kept[1:]
        tag = None
        if type_toks:
            try:
                dt = sqlglot.parse_one(f"CAST(NULL AS {' '.join(type_toks)})", read="postgres").to
                tag = typemap.type_tag_for_sql(dt)
            except Exception:  # noqa: BLE001 — unknown type spelling → text
                tag = None
        out.append(
            {"name": name.strip('"') if name else None, "mode": mode, "type_tag": tag or "text"}
        )
    return out


def _create_procedure(raw: str, db: str, catalog: Catalog, session: Session | None) -> SQLResult:
    """``CREATE [OR REPLACE] PROCEDURE name(params) [LANGUAGE x] AS <body>`` —
    parsed here (not via sqlglot, which rejects the ``a INOUT int`` argmode) and
    stored like a function with ``is_procedure`` + per-param modes."""
    text = raw.strip().rstrip(";").strip()
    m = re.match(r"(?is)^create\s+(?P<repl>or\s+replace\s+)?procedure\s+", text)
    if m is None:
        raise errors.syntax_error("malformed CREATE PROCEDURE")
    or_replace = bool(m.group("repl"))
    nm = re.match(r'(?is)\s*(?P<name>"[^"]+"|[\w.]+)\s*\(', text[m.end() :])
    if nm is None:
        raise errors.syntax_error("CREATE PROCEDURE requires a parameter list")
    name = nm.group("name").strip('"')
    pos = m.end() + nm.end()  # just past the opening '('
    depth, i = 1, pos
    while i < len(text) and depth:
        depth += 1 if text[i] == "(" else -1 if text[i] == ")" else 0
        i += 1
    params = _parse_proc_params(text[pos : i - 1])
    rest = text[i:]
    lang_m = re.search(r"(?is)\blanguage\s+(?P<lang>\w+)", rest)
    language = lang_m.group("lang").lower() if lang_m else "sql"
    if language not in ("sql", "plpgsql"):
        raise errors.feature_not_supported(
            f"CREATE PROCEDURE LANGUAGE {language} is not supported (only sql / plpgsql)"
        )
    body_re = r"(?is)\bas\s+(?P<body>\$(?P<tag>\w*)\$.*?\$(?P=tag)\$|'(?:[^']|'')*')"
    body_m = re.search(body_re, rest)
    if body_m is None:
        raise errors.syntax_error("CREATE PROCEDURE requires an AS body")
    body_raw = body_m.group("body")
    if body_raw.startswith("$"):
        body = re.sub(r"(?is)^\$\w*\$(.*)\$\w*\$$", r"\1", body_raw)
    else:
        body = body_raw[1:-1].replace("''", "'")
    body = body.strip()
    if language == "plpgsql":
        from secantus.sql import plpgsql

        plpgsql.parse(body)
    # CALL supplies an argument for every parameter, including OUT ones (a
    # placeholder), so a procedure is keyed by its TOTAL parameter count — that
    # is what the CALL-site lookup matches.
    nargs = len(params)
    if not or_replace and catalog.function_exists(db, name, nargs):
        raise errors.SQLError("42723", f'function "{name}" already exists with same argument types')
    catalog.put_function(
        db,
        {
            "name": name,
            "nargs": nargs,
            "params": [p["name"] for p in params],
            "param_types": [p["type_tag"] for p in params],
            "param_modes": [p["mode"] for p in params],
            "return_tag": None,
            "is_table": False,
            "body": body,
            "language": language,
            "returns_trigger": False,
            "is_procedure": True,
        },
    )
    return SQLResult(command_tag="CREATE PROCEDURE")


def _drop_procedure(raw: str, db: str, catalog: Catalog) -> SQLResult:
    """``DROP PROCEDURE [IF EXISTS] name`` — no arg list needed; drops the single
    stored procedure of that name."""
    m = re.match(
        r'(?is)^\s*drop\s+procedure\s+(?P<exists>if\s+exists\s+)?(?P<name>"[^"]+"|[\w.]+)', raw
    )
    if m is None:
        raise errors.syntax_error("malformed DROP PROCEDURE")
    name = m.group("name").strip('"')
    dropped = False
    for fn in catalog.list_functions(db):
        if fn.get("is_procedure") and fn.get("name", "").lower() == name.lower():
            dropped = catalog.drop_function(db, name, fn["nargs"])
            break
    if not dropped and not m.group("exists"):
        raise errors.SQLError("42883", f'procedure "{name}" does not exist')
    return SQLResult(command_tag="DROP PROCEDURE")


def _call_procedure(
    tail: str, session: Session, storage: Any, db: str, catalog: Catalog
) -> SQLResult:
    """``CALL name(args)`` — run the procedure body; its OUT / INOUT parameters
    (after execution) form the single result row, like Postgres."""
    from secantus.sql import plpgsql, scalar

    m = re.match(r'(?is)^\s*(?P<name>"[^"]+"|[\w.]+)\s*\((?P<args>.*)\)\s*;?\s*$', tail.strip())
    if m is None:
        raise errors.syntax_error(f"malformed CALL statement: {tail}")
    name = m.group("name").strip('"')
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    arg_vals: list[Any] = []
    args_text = m.group("args").strip()
    if args_text:
        for part in _split_top_level_commas(args_text):
            node = sqlglot.parse_one(f"SELECT {part}", read="postgres").expressions[0]
            arg_vals.append(scalar.evaluate(node, planner._const_scope, ctx))
    func = catalog.get_function(db, name, len(arg_vals))
    if func is None or not func.get("is_procedure"):
        raise errors.SQLError("42883", f"procedure {name} does not exist")
    if func.get("language") == "plpgsql":
        env = plpgsql.invoke_procedure(func, arg_vals, ctx)
    else:
        raise errors.feature_not_supported("only LANGUAGE plpgsql procedures are callable")
    cols = _procedure_out_columns(func)
    if cols:
        params = func.get("params") or []
        modes = func.get("param_modes") or []
        vals = [
            env.get(str(pname).lower()) if pname else None
            for pname, mode in zip(params, modes, strict=False)
            if mode in ("OUT", "INOUT")
        ]
        return SQLResult(command_tag="CALL", columns=cols, rows=[tuple(vals)], rowcount=1)
    return SQLResult(command_tag="CALL")


def _procedure_out_columns(func: dict) -> list[ColumnDesc]:
    """The result-row columns of a CALL: the procedure's OUT / INOUT parameters,
    in declaration order (empty for a procedure with no output params)."""
    params = func.get("params") or []
    modes = func.get("param_modes") or []
    types = func.get("param_types") or []
    return [
        ColumnDesc(pname or "?column?", tag, typemap.PG_OID.get(tag, 25))
        for pname, mode, tag in zip(params, modes, types, strict=False)
        if mode in ("OUT", "INOUT")
    ]


def _call_out_columns(tail: str, db: str, catalog: Catalog) -> list[ColumnDesc] | None:
    """Result columns for a ``CALL name(args)`` WITHOUT executing it — the
    procedure's OUT/INOUT params, or None (NoData) when it has none or isn't a
    known procedure (extended-protocol Describe of a CALL portal)."""
    m = re.match(r'(?is)^\s*(?P<name>"[^"]+"|[\w.]+)\s*\((?P<args>.*)\)\s*;?\s*$', tail.strip())
    if m is None:
        return None
    args_text = m.group("args").strip()
    nargs = len(_split_top_level_commas(args_text)) if args_text else 0
    func = catalog.get_function(db, m.group("name").strip('"'), nargs)
    if func is None or not func.get("is_procedure"):
        return None
    return _procedure_out_columns(func) or None


def _create_trigger(stmt: exp.Create, db: str, catalog: Catalog, session: Session) -> SQLResult:
    """``CREATE TRIGGER name BEFORE INSERT ON table FOR EACH ROW EXECUTE
    PROCEDURE fn()`` — the supported shape (pgx's tsvector-maintenance
    trigger). Every other timing / event / level is rejected faithfully
    rather than stored-and-never-fired, which would lie about user triggers."""
    trigger_name = stmt.this.name
    props = stmt.args.get("properties")
    tp = next(
        (p for p in (props.expressions if props else []) if isinstance(p, exp.TriggerProperties)),
        None,
    )
    if tp is None:
        raise errors.feature_not_supported("CREATE TRIGGER shape is not supported")
    timing = str(tp.args.get("timing") or "").upper()
    for_each = str(tp.args.get("for_each") or "").upper()
    events = [
        str(e.this).upper() for e in tp.args.get("events") or [] if isinstance(e, exp.TriggerEvent)
    ]
    table_node = tp.args.get("table")
    if timing != "BEFORE" or for_each != "ROW" or events != ["INSERT"] or table_node is None:
        raise errors.feature_not_supported("only BEFORE INSERT FOR EACH ROW triggers are supported")
    tname = planner.qualified_table_name(table_node)
    if catalog.get(db, tname) is None:
        raise errors.undefined_table(tname)
    execute = tp.args.get("execute")
    fn_name = None
    target = execute.this if execute is not None else None
    if isinstance(target, exp.Dot):
        qualifier = target.this.name if isinstance(target.this, exp.Identifier) else None
        leaf = target.expression
        leaf_name = leaf.name if hasattr(leaf, "name") else None
        if qualifier == "pg_temp":
            # ``EXECUTE PROCEDURE pg_temp.fn()`` — the Dot is not a Table
            # node, so the temp-namespace qualify pass never rewrote it.
            fn_name = f"{session.ensure_temp_schema()}.{leaf_name}"
        elif qualifier is not None and qualifier.startswith("pg_temp_"):
            fn_name = f"{qualifier}.{leaf_name}"
        elif qualifier in (None, "public"):
            fn_name = leaf_name
        else:
            fn_name = f"{qualifier}.{leaf_name}"
    elif target is not None and hasattr(target, "name"):
        fn_name = target.name
    if not fn_name:
        raise errors.feature_not_supported("CREATE TRIGGER EXECUTE shape is not supported")
    func = catalog.get_function(db, fn_name, 0)
    if func is None:
        raise errors.SQLError("42883", f"function {fn_name}() does not exist")
    if not func.get("returns_trigger") or func.get("language") != "plpgsql":
        raise errors.SQLError("42P17", f"function {fn_name} must return type trigger")
    if catalog.trigger_exists(db, tname, trigger_name):
        raise errors.SQLError(
            "42710", f'trigger "{trigger_name}" for relation "{tname}" already exists'
        )
    catalog.put_trigger(
        db,
        {
            "name": trigger_name,
            "table": tname,
            "timing": timing,
            "event": "INSERT",
            "level": "ROW",
            "function": fn_name,
        },
    )
    return SQLResult(command_tag="CREATE TRIGGER")


def _drop_trigger(stmt: exp.Drop, db: str, catalog: Catalog) -> SQLResult:
    """``DROP TRIGGER [IF EXISTS] name ON table`` — sqlglot carries the ``ON
    table`` in the ``cluster`` OnProperty. Removes the stored trigger (triggers
    are keyed per-table)."""
    trigger_name = stmt.this.name
    on_prop = stmt.args.get("cluster")
    target = on_prop.this if isinstance(on_prop, exp.OnProperty) else None
    if target is None:
        raise errors.syntax_error("DROP TRIGGER requires ON <table>")
    tname = planner.qualified_table_name(target) if isinstance(target, exp.Table) else target.name
    if not catalog.drop_trigger(db, tname, trigger_name):
        if stmt.args.get("exists"):
            return SQLResult(command_tag="DROP TRIGGER")
        raise errors.SQLError(
            "42704", f'trigger "{trigger_name}" for relation "{tname}" does not exist'
        )
    return SQLResult(command_tag="DROP TRIGGER")


def _drop_function(stmt: exp.Drop, db: str, catalog: Catalog) -> SQLResult:
    name = stmt.this.name
    nargs = len(stmt.args.get("expressions") or [])
    dropped = catalog.drop_function(db, name, nargs)
    # No arg list given (``DROP FUNCTION name``): drop any single overload.
    if not dropped and not stmt.args.get("expressions"):
        for fn in catalog.list_functions(db):
            if fn.get("name", "").lower() == name.lower():
                dropped = catalog.drop_function(db, name, fn["nargs"])
                break
    if not dropped and not stmt.args.get("exists"):
        raise errors.SQLError("42883", f'function "{name}" does not exist')
    return SQLResult(command_tag="DROP FUNCTION")


# ``CREATE DOMAIN name [AS] base_type [DEFAULT expr] [ [CONSTRAINT c] { NOT NULL |
# NULL | CHECK (expr) } … ]`` — sqlglot doesn't model the grammar, so it arrives
# as a Command. The base type + constraints are re-parsed as a column definition
# (``CREATE TABLE _ (value <body>)``) which reuses sqlglot's column-constraint
# grammar for CHECK / NOT NULL / DEFAULT.
_CREATE_DOMAIN_RE = re.compile(
    r'(?is)^\s*DOMAIN\s+(?:IF\s+NOT\s+EXISTS\s+)?("[^"]+"|[\w.]+)\s+(?:AS\s+)?(.*?)\s*;?\s*$'
)
_DROP_DOMAIN_RE = re.compile(
    r'(?is)^\s*DOMAIN\s+(IF\s+EXISTS\s+)?("[^"]+"|[\w.]+)'
    r"(?:\s*,\s*(?:\"[^\"]+\"|[\w.]+))*\s*(?:CASCADE|RESTRICT)?\s*;?\s*$"
)


def _create_domain_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    m = _CREATE_DOMAIN_RE.match(_command_text(stmt))
    if m is None:
        raise errors.feature_not_supported(f"unsupported CREATE DOMAIN: {stmt.sql()}")
    name = _unquote_ident(m.group(1))
    if catalog.domain_exists(db, name) or catalog.enum_exists(db, name):
        raise errors.SQLError("42710", f'type "{name}" already exists')
    coldef = _parse_domain_body(name, m.group(2))
    kind = coldef.args["kind"]
    base_tag = typemap.type_tag_for_sql(kind)
    if base_tag is None:
        # An unrecognised bare identifier reads as an undefined type (42704);
        # a known-but-unmapped type (e.g. a range) is unsupported (0A000).
        if isinstance(kind, exp.DataType) and kind.this and kind.this.name == "USERDEFINED":
            raise errors.SQLError("42704", f'type "{kind.sql(dialect="postgres")}" does not exist')
        raise errors.feature_not_supported(
            f'unsupported base type for domain "{name}": {kind.sql()}'
        )
    # The base type's declared identity — a domain over ``varbit(3)`` /
    # ``numeric(8,3)`` carries the length/precision on the domain's pg_type
    # (typtypmod), which is where getColumns reads COLUMN_SIZE for a domain
    # column. Captured before the constraint loop reassigns ``kind``.
    base_ident = planner._decl_identity(kind) if isinstance(kind, exp.DataType) else {}
    not_null = False
    checks: list[dict[str, str]] = []
    has_default, default = False, None
    for con in coldef.args.get("constraints") or []:
        kind = con.kind
        if isinstance(kind, exp.NotNullColumnConstraint) and not kind.args.get("allow_null"):
            not_null = True
        elif isinstance(kind, exp.CheckColumnConstraint):
            cname = con.args.get("this")
            checks.append(
                {
                    "name": (cname.name if cname is not None else f"{name}_check"),
                    "expression": kind.this.sql(dialect="postgres"),
                }
            )
        elif isinstance(kind, exp.DefaultColumnConstraint):
            has_default, default = planner._literal_default(kind.this, base_tag)
    catalog.create_domain(
        db,
        name,
        base_tag,
        not_null=not_null,
        checks=checks,
        has_default=has_default,
        default=default,
        typmod=base_ident.get("typmod", -1),
        base_oid=base_ident.get("decl_oid"),
    )
    return SQLResult(command_tag="CREATE DOMAIN")


def _parse_domain_body(name: str, body: str) -> exp.ColumnDef:
    """Re-parse a domain's ``base_type [constraints…]`` tail as a column
    definition, reusing sqlglot's column-constraint grammar (CHECK / NOT NULL /
    DEFAULT). The ``VALUE`` keyword in a domain CHECK becomes a column reference."""
    try:
        tbl = sqlglot.parse_one(f"CREATE TABLE _d (value {body})", read="postgres")
        coldef = tbl.this.expressions[0]
    except Exception as exc:  # noqa: BLE001 — surface a clean SQL error
        raise errors.feature_not_supported(
            f'could not parse domain "{name}" definition: {body}'
        ) from exc
    if not isinstance(coldef, exp.ColumnDef):
        raise errors.feature_not_supported(f'could not parse domain "{name}" definition: {body}')
    return coldef


def _drop_domain_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    text = _command_text(stmt)
    m = _DROP_DOMAIN_RE.match(text)
    if m is None:
        raise errors.feature_not_supported(f"unsupported DROP DOMAIN: {stmt.sql()}")
    if_exists = m.group(1) is not None
    name = _unquote_ident(m.group(2))
    if not catalog.drop_domain(db, name) and not if_exists:
        raise errors.SQLError("42704", f'type "{name}" does not exist')
    return SQLResult(command_tag="DROP DOMAIN")


# ``ALTER DOMAIN name <action>`` — sqlglot doesn't model the grammar, so it
# arrives as a Command. Peel ``DOMAIN <name>`` then dispatch on the action tail.
_ALTER_DOMAIN_RE = re.compile(r'(?is)^\s*DOMAIN\s+("[^"]+"|[\w.]+)\s+(.*?)\s*;?\s*$')


def _alter_domain_command(stmt: exp.Command, storage: Any, db: str, catalog: Catalog) -> SQLResult:
    m = _ALTER_DOMAIN_RE.match(_command_text(stmt))
    if m is None:
        raise errors.feature_not_supported(f"unsupported ALTER DOMAIN: {stmt.sql()}")
    name = _unquote_ident(m.group(1))
    action = m.group(2).strip()
    domain = catalog.get_domain(db, name)
    if domain is None:
        raise errors.SQLError("42704", f'type "{name}" does not exist')
    doc = {
        "base_tag": domain["base_tag"],
        "not_null": bool(domain.get("not_null")),
        "checks": list(domain.get("checks") or []),
        "has_default": bool(domain.get("has_default")),
        "default": domain.get("default"),
    }
    upper = action.upper()

    if upper.startswith("ADD"):
        _alter_domain_add_constraint(storage, db, catalog, name, doc, action)
    elif upper.startswith("DROP CONSTRAINT"):
        _alter_domain_drop_constraint(name, doc, action)
    elif upper.startswith("SET DEFAULT"):
        expr = action[len("SET DEFAULT") :].strip()
        node = sqlglot.parse_one(expr, read="postgres")
        doc["has_default"], doc["default"] = planner._literal_default(node, doc["base_tag"])
    elif upper == "DROP DEFAULT":
        doc["has_default"], doc["default"] = False, None
    elif upper == "SET NOT NULL":
        _revalidate_domain_not_null(storage, db, catalog, name)
        doc["not_null"] = True
    elif upper == "DROP NOT NULL":
        doc["not_null"] = False
    elif upper.startswith("RENAME TO"):
        new = _unquote_ident(action[len("RENAME TO") :].strip())
        return _alter_domain_rename(db, catalog, name, new, doc)
    elif upper.startswith("VALIDATE CONSTRAINT"):
        # We validate eagerly on ADD, so VALIDATE is a no-op acceptance.
        return SQLResult(command_tag="ALTER DOMAIN")
    else:
        raise errors.feature_not_supported(f"unsupported ALTER DOMAIN action: {action}")

    catalog.update_domain(db, name, doc)
    return SQLResult(command_tag="ALTER DOMAIN")


_ADD_NOT_VALID_RE = re.compile(r"(?is)\s+NOT\s+VALID\s*$")


def _alter_domain_add_constraint(
    storage: Any, db: str, catalog: Catalog, name: str, doc: dict[str, Any], action: str
) -> None:
    """``ADD [CONSTRAINT c] CHECK (expr) [NOT VALID]`` — append a CHECK to the
    domain. Existing rows are re-validated unless ``NOT VALID`` is given."""
    body = action[len("ADD") :].strip()
    not_valid = bool(_ADD_NOT_VALID_RE.search(body))
    body = _ADD_NOT_VALID_RE.sub("", body).strip()
    coldef = _parse_domain_body(name, f"int {body}")
    check = None
    for con in coldef.args.get("constraints") or []:
        if isinstance(con.kind, exp.CheckColumnConstraint):
            cname = con.args.get("this")
            check = {
                "name": cname.name if cname is not None else None,  # None → auto-name below
                "expression": con.kind.this.sql(dialect="postgres"),
            }
    if check is None:
        raise errors.feature_not_supported(
            "only ALTER DOMAIN ADD [CONSTRAINT c] CHECK (...) is supported"
        )
    existing = {c["name"] for c in doc["checks"]}
    if check["name"] is None:
        # Unnamed CHECK: Postgres auto-generates <domain>_check, _check1, _check2…
        base = f"{name}_check"
        candidate = base
        i = 0
        while candidate in existing:
            i += 1
            candidate = f"{base}{i}"
        check["name"] = candidate
    elif check["name"] in existing:
        raise errors.SQLError(
            "42710", f'constraint "{check["name"]}" for domain "{name}" already exists'
        )
    if not not_valid:
        _revalidate_domain_check(storage, db, catalog, name, check)
    doc["checks"].append(check)


def _alter_domain_drop_constraint(name: str, doc: dict[str, Any], action: str) -> None:
    """``DROP CONSTRAINT [IF EXISTS] c [RESTRICT|CASCADE]``."""
    m = re.match(
        r'(?is)^DROP\s+CONSTRAINT\s+(IF\s+EXISTS\s+)?("[^"]+"|[\w]+)'
        r"(?:\s+(?:RESTRICT|CASCADE))?\s*$",
        action,
    )
    if m is None:
        raise errors.feature_not_supported(f"unsupported ALTER DOMAIN action: {action}")
    if_exists = m.group(1) is not None
    cname = _unquote_ident(m.group(2))
    before = len(doc["checks"])
    doc["checks"] = [c for c in doc["checks"] if c["name"] != cname]
    if len(doc["checks"]) == before and not if_exists:
        raise errors.SQLError("42704", f'constraint "{cname}" of domain "{name}" does not exist')


def _alter_domain_rename(
    db: str, catalog: Catalog, name: str, new: str, doc: dict[str, Any]
) -> SQLResult:
    """``RENAME TO new`` — re-key the domain and repoint every column that
    references it (columns store ``domain_type`` by name)."""
    if catalog.domain_exists(db, new) or catalog.enum_exists(db, new):
        raise errors.SQLError("42710", f'type "{new}" already exists')
    catalog.drop_domain(db, name)
    catalog.update_domain(db, new, doc)
    for tname in catalog.list_tables(db):
        table = catalog.get(db, tname)
        if table is None or not any(c.domain_type == name for c in table.columns):
            continue
        from dataclasses import replace as _replace

        table.columns = [
            _replace(c, domain_type=new) if c.domain_type == name else c for c in table.columns
        ]
        catalog.replace(db, table)
    return SQLResult(command_tag="ALTER DOMAIN")


def _domain_columns(catalog: Catalog, db: str, name: str) -> list[tuple[Any, Any]]:
    """Every (table, column) pair whose column is typed with domain ``name``."""
    out = []
    for tname in catalog.list_tables(db):
        table = catalog.get(db, tname)
        if table is None:
            continue
        for col in table.columns:
            if col.domain_type == name:
                out.append((table, col))
    return out


def _revalidate_domain_check(
    storage: Any, db: str, catalog: Catalog, name: str, check: dict[str, str]
) -> None:
    """Re-check every stored row of every column typed with domain ``name`` against
    a new CHECK; raise ``23514`` if any row violates it (NULL passes)."""
    predicate = sqlglot.parse_one(check["expression"], read="postgres")
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=None)
    for table, col in _domain_columns(catalog, db, name):
        for doc in storage.find_matching(db, table.collection, {}):
            from secantus.paths import get_path

            value = get_path(doc, col.field)
            if value is None:
                continue

            def scope(node: Any, _v: Any = value) -> Any:
                return _v if node.name.lower() == "value" else None

            result = scalar.evaluate(predicate, scope, ctx)
            if result is not None and not scalar._truthy(result):
                raise errors.SQLError(
                    "23514",
                    f'column "{col.name}" of table "{table.name}" contains values that '
                    f"violate the new constraint",
                )


def _revalidate_domain_not_null(storage: Any, db: str, catalog: Catalog, name: str) -> None:
    """Raise ``23502`` if any stored row of a column typed with domain ``name``
    holds a NULL (blocks ``SET NOT NULL`` when existing data would violate it)."""
    from secantus.paths import get_path

    for table, col in _domain_columns(catalog, db, name):
        for doc in storage.find_matching(db, table.collection, {}):
            if get_path(doc, col.field) is None:
                raise errors.SQLError(
                    "23502",
                    f'column "{col.name}" of table "{table.name}" contains null values',
                )


# ``ALTER TYPE name ADD VALUE [IF NOT EXISTS] 'label' [BEFORE|AFTER 'other']``
# falls back to a Command (sqlglot doesn't model the enum grammar).
_ALTER_TYPE_ADD_RE = re.compile(
    r"(?is)^\s*TYPE\s+(\"[^\"]+\"|\w+)\s+ADD\s+VALUE\s+(IF\s+NOT\s+EXISTS\s+)?"
    r"'((?:[^']|'')*)'\s*(?:(BEFORE|AFTER)\s+'((?:[^']|'')*)')?\s*;?\s*$"
)


def _alter_type_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    """``ALTER TYPE name ADD VALUE [IF NOT EXISTS] 'label' [BEFORE|AFTER 'other']``
    — extend an enum with a new label, optionally positioned relative to an
    existing one. Other ALTER TYPE forms are unsupported."""
    m = _ALTER_TYPE_ADD_RE.match(_command_text(stmt))
    if m is None:
        raise errors.feature_not_supported(
            "only ALTER TYPE … ADD VALUE is supported (RENAME / composite alters are not)"
        )
    name = _unquote_ident(m.group(1))
    if_not_exists = m.group(2) is not None
    label = m.group(3).replace("''", "'")
    keyword, neighbour = m.group(4), m.group(5)
    before = neighbour.replace("''", "'") if keyword and keyword.upper() == "BEFORE" else None
    after = neighbour.replace("''", "'") if keyword and keyword.upper() == "AFTER" else None
    catalog.alter_enum_add_value(
        db, name, label, before=before, after=after, if_not_exists=if_not_exists
    )
    return SQLResult(command_tag="ALTER TYPE")


_ALTER_SEQUENCE_RE = re.compile(
    r"(?is)^\s*SEQUENCE\s+(?:IF\s+EXISTS\s+)?(\"[^\"]+\"|\w+)\s+(.*?)\s*;?\s*$"
)


_ALTER_DATABASE_RE = re.compile(
    r'(?is)^\s*DATABASE\s+(?P<name>"[^"]+"|[\w$]+)\s+'
    r"(?:(?P<reset>RESET)\s+(?P<rname>ALL|[\w.]+)"
    r"|SET\s+(?P<sname>[\w.]+)\s*(?:=|\s+TO\s+)\s*(?P<value>.+?))\s*;?\s*$"
)


def _alter_database_command(
    stmt: exp.Command, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``ALTER DATABASE <db> SET <guc> TO <value>`` / ``RESET <guc>|ALL`` — a
    database-level GUC default. PG applies it to NEW sessions only, never to
    already-open ones, so it is stored in the catalog and merged into a
    session's settings at connect (see ``session.apply_database_defaults``)."""
    m = _ALTER_DATABASE_RE.match(_command_text(stmt))
    if m is None:
        raise errors.feature_not_supported(f"unsupported ALTER DATABASE: {stmt.sql()}")
    name = m.group("name").strip('"')
    if name != db:
        # Single-node: only the connected database exists.
        raise errors.SQLError("3D000", f'database "{name}" does not exist')
    if m.group("reset"):
        target = m.group("rname")
        if target.upper() == "ALL":
            for key in list(catalog.db_settings(db)):
                catalog.set_db_setting(db, key, None)
        else:
            catalog.set_db_setting(db, sql_session.canonical_guc_name(target), None)
        return SQLResult(command_tag="ALTER DATABASE")
    guc = sql_session.canonical_guc_name(m.group("sname"))
    raw = m.group("value").strip()
    if raw.upper() == "DEFAULT":
        catalog.set_db_setting(db, guc, None)
        return SQLResult(command_tag="ALTER DATABASE")
    if len(raw) >= 2 and raw[0] == raw[-1] and raw[0] in "'\"":
        raw = raw[1:-1].replace(raw[0] * 2, raw[0])
    catalog.set_db_setting(db, guc, raw)
    return SQLResult(command_tag="ALTER DATABASE")


def _alter_sequence_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    """``ALTER SEQUENCE [IF EXISTS] name { RESTART [WITH n] | INCREMENT BY n |
    MINVALUE n | MAXVALUE n | START WITH n | [NO] CYCLE }…``. Arrives as a
    Command (sqlglot doesn't model the grammar)."""
    m = _ALTER_SEQUENCE_RE.match(_command_text(stmt))
    if m is None:
        raise errors.feature_not_supported(f"unsupported ALTER SEQUENCE: {stmt.sql()}")
    parts = [seg.strip().strip('"') for seg in m.group(1).split(".")]
    name = parts[-1] if len(parts) == 1 or parts[0] == "public" else ".".join(parts)
    if not catalog.sequence_exists(db, name):
        if "IF EXISTS" in _command_text(stmt).upper():
            return SQLResult(command_tag="ALTER SEQUENCE")
        raise errors.SQLError("42P01", f'relation "{name}" does not exist')
    changes = _parse_alter_sequence_opts(m.group(2))
    catalog.alter_sequence(db, name, changes)
    return SQLResult(command_tag="ALTER SEQUENCE")


def _parse_alter_sequence_opts(rest: str) -> dict[str, Any]:
    """Parse the option keywords after ``ALTER SEQUENCE name`` into catalog
    changes (``restart`` / ``increment`` / ``min_value`` / ``max_value`` /
    ``start`` / ``cycle``)."""
    tokens = rest.split()
    changes: dict[str, Any] = {}
    i = 0

    def _int_after(j: int) -> tuple[int | None, int]:
        # value at tokens[j], optionally preceded by BY/WITH — return (value, new_j).
        if j < len(tokens) and tokens[j].upper() in ("BY", "WITH"):
            j += 1
        if j < len(tokens):
            try:
                return int(tokens[j].rstrip(";")), j
            except ValueError:
                return None, j
        return None, j

    while i < len(tokens):
        tok = tokens[i].upper()
        if tok == "RESTART":
            # RESTART [WITH n] — n optional (None → restart at the sequence's start).
            if i + 1 < len(tokens) and (
                tokens[i + 1].upper() == "WITH" or tokens[i + 1].lstrip("-").isdigit()
            ):
                val, i = _int_after(i + 1)
                changes["restart"] = val
            else:
                changes["restart"] = None
        elif tok == "INCREMENT":
            val, i = _int_after(i + 1)
            if val is not None:
                changes["increment"] = val
        elif tok == "MINVALUE":
            val, i = _int_after(i + 1)
            changes["min_value"] = val
        elif tok == "MAXVALUE":
            val, i = _int_after(i + 1)
            changes["max_value"] = val
        elif tok == "START":
            val, i = _int_after(i + 1)
            if val is not None:
                changes["start"] = val
        elif tok == "CYCLE":
            changes["cycle"] = True
        elif tok == "NO" and i + 1 < len(tokens) and tokens[i + 1].upper() == "CYCLE":
            changes["cycle"] = False
            i += 1
        i += 1
    return changes


_MAX_VIEW_DEPTH = 32


def _expand_views(
    stmt: exp.Expression, catalog: Catalog, db: str, _depth: int = 0
) -> exp.Expression:
    """Rewrite FROM/JOIN references to declared views into inline subqueries.

    A view is a stored SELECT: ``SELECT ... FROM v`` becomes
    ``SELECT ... FROM (<view def>) AS v``. A view whose definition references
    another view expands recursively. CTE names defined in the same statement
    shadow views and are left alone."""
    if _depth > _MAX_VIEW_DEPTH:
        raise errors.feature_not_supported("view nesting too deep (possible cycle)")
    cte_names = {cte.alias for w in stmt.find_all(exp.With) for cte in w.expressions}
    for holder in list(stmt.find_all(exp.From, exp.Join)):
        src = holder.this
        if not isinstance(src, exp.Table):
            continue
        _schema = src.args.get("db")
        _sname = _schema.name if _schema is not None else None
        if _sname in ("pg_catalog", "information_schema"):
            continue  # system catalogs are virtual tables, never stored views
        if _sname is None and src.name in cte_names:
            continue
        vdef = catalog.get_view(db, planner.qualified_table_name(src))
        if vdef is None:
            continue
        inner = sqlglot.parse_one(vdef, read="postgres")
        _expand_views(inner, catalog, db, _depth + 1)
        alias = src.alias or src.name
        holder.set(
            "this",
            exp.Subquery(this=inner, alias=exp.TableAlias(this=exp.to_identifier(alias))),
        )
    return stmt


# ``CREATE VIEW … WITH [LOCAL|CASCADED] CHECK OPTION`` exceeds sqlglot's parser
# (it falls back to a Command), so match + strip the trailing clause and re-parse
# the inner ``CREATE VIEW … AS SELECT …`` on its own.
_VIEW_CHECK_OPTION_RE = re.compile(
    r"(?is)\bVIEW\b.*?(?P<clause>\bWITH\s+(?:(?P<mode>LOCAL|CASCADED)\s+)?CHECK\s+OPTION)\s*$"
)


def _create_view_check_option_command(
    stmt: exp.Command, storage: Any, db: str, catalog: Catalog
) -> SQLResult:
    """``CREATE [OR REPLACE] VIEW v AS SELECT … WITH [LOCAL|CASCADED] CHECK
    OPTION`` — strip the check-option suffix, re-parse the inner CREATE VIEW, and
    store it with its check-option mode so write-through enforces the predicate."""
    full = stmt.sql(dialect="postgres")
    m = _VIEW_CHECK_OPTION_RE.search(full)
    mode = (m.group("mode") or "CASCADED").upper()
    inner_sql = full[: m.start("clause")].rstrip()
    inner = sqlglot.parse_one(inner_sql, read="postgres")
    if not isinstance(inner, exp.Create) or (inner.args.get("kind") or "").upper() != "VIEW":
        raise errors.feature_not_supported("unsupported CREATE VIEW … WITH CHECK OPTION form")
    return executor.execute_create_view(inner, catalog, storage, db, check_option=mode)


def _updatable_view_base(vdef_sql: str) -> tuple[str, exp.Expression | None] | None:
    """For an *automatically-updatable* view, return ``(base_table, where_cond)``;
    None if the view isn't simple enough to write through.

    Postgres auto-updatable rules (the subset we support): exactly one base table
    (no join / set-op), no DISTINCT / GROUP BY / HAVING / window / LIMIT / OFFSET /
    WITH, and every output column is a **plain, unaliased** base column (or ``*``).
    That keeps view column names identical to base column names, so DML needs no
    column remapping — only a table retarget and (for UPDATE/DELETE) AND-ing the
    view's WHERE. Anything else raises (PG would require an INSTEAD OF trigger)."""
    sel = sqlglot.parse_one(vdef_sql, read="postgres")
    if not isinstance(sel, exp.Select):
        return None
    if any(
        sel.args.get(k)
        for k in ("group", "having", "distinct", "qualify", "windows", "laterals", "limit", "with")
    ):
        return None
    if sel.args.get("joins"):
        return None
    # The FROM arg key varies across sqlglot versions ("from" / "from_").
    from_ = sel.args.get("from") or sel.args.get("from_")
    if from_ is None or not isinstance(from_.this, exp.Table) or from_.this.args.get("db"):
        return None
    projs = sel.expressions
    if not (len(projs) == 1 and isinstance(projs[0], exp.Star)):
        for proj in projs:
            # A plain column with no alias keeps view name == base name.
            if not isinstance(proj, exp.Column):
                return None
    where = sel.args.get("where")
    return from_.this.name, (where.this if isinstance(where, exp.Where) else None)


def _rewrite_write_through_view(
    stmt: exp.Expression, catalog: Catalog, db: str
) -> tuple[exp.Expression, tuple[exp.Expression, str] | None]:
    """If an INSERT/UPDATE/DELETE targets a view, rewrite it onto the view's base
    table (#146). Non-updatable views raise a faithful ``0A000``. Returns
    ``(stmt, check_option_predicate)`` — the predicate (the view's WHERE) is
    non-None when the view carries ``WITH CHECK OPTION`` and the statement is an
    INSERT / UPDATE, so the executor validates each written row against it."""
    if isinstance(stmt, exp.Insert):
        tgt = stmt.this
        table_node = tgt.this if isinstance(tgt, exp.Schema) else tgt
    else:
        table_node = stmt.find(exp.Table)
    if not isinstance(table_node, exp.Table):
        return stmt, None
    name = table_node.name
    vdef = catalog.get_view(db, name)
    if vdef is None:
        return stmt, None  # a real table (or reflected collection) — unchanged
    spec = _updatable_view_base(vdef)
    if spec is None:
        verb = {exp.Insert: "INSERT into", exp.Update: "UPDATE", exp.Delete: "DELETE from"}[
            type(stmt)
        ]
        raise errors.feature_not_supported(
            f'cannot {verb} view "{name}": it is not an automatically-updatable view'
        )
    base, view_cond = spec
    table_node.set("this", exp.to_identifier(base, quoted=table_node.this.quoted))
    if view_cond is not None and isinstance(stmt, (exp.Update, exp.Delete)):
        # The view's WHERE restricts which base rows the DML may touch. AND it in.
        existing = stmt.args.get("where")
        merged = view_cond.copy()
        if isinstance(existing, exp.Where) and existing.this is not None:
            merged = exp.and_(existing.this, merged)
        stmt.set("where", exp.Where(this=merged))
    # WITH CHECK OPTION: the written row must remain visible through the view, i.e.
    # satisfy its WHERE. Applies to INSERT / UPDATE (a DELETE removes rows, so it
    # can't create a row that violates the view predicate).
    check_pred = None
    if (
        view_cond is not None
        and isinstance(stmt, (exp.Insert, exp.Update))
        and catalog.get_view_check_option(db, name) is not None
    ):
        check_pred = (view_cond, name)
    return stmt, check_pred


def _own_with(stmt: exp.Expression) -> exp.With | None:
    """The ``WITH`` clause attached directly to ``stmt`` (not one nested inside a
    subquery), or None. Found by identity among the statement's own args so it's
    robust to the sqlglot arg-key name."""
    for value in stmt.args.values():
        if isinstance(value, exp.With):
            return value
    return None


class _CTECatalog(Catalog):
    """A catalog overlay that resolves CTE names to their materialized TableDefs
    and delegates everything else to the base catalog (so declared / reflected
    tables still resolve). Scoped to one statement's execution."""

    def __init__(self, base: Catalog, ctes: dict[str, TableDef]) -> None:
        self._base = base
        self._ctes = ctes
        # Inherited Catalog methods (trigger lookups, sequences, …) read
        # self._storage directly — share the base's so they behave exactly
        # like the base catalog rather than crashing on a missing attribute.
        self._storage = base._storage

    def get(self, db: str, table: str) -> TableDef | None:
        if table in self._ctes:
            return self._ctes[table]
        return self._base.get(db, table)

    def exists(self, db: str, table: str) -> bool:
        return table in self._ctes or self._base.exists(db, table)

    def put(self, db: str, table: TableDef) -> None:
        self._base.put(db, table)

    def drop(self, db: str, table: str) -> bool:
        return self._base.drop(db, table)

    def list_tables(self, db: str) -> list[str]:
        return self._base.list_tables(db)

    def get_view(self, db: str, name: str) -> str | None:
        return self._base.get_view(db, name)

    def get_view_check_option(self, db: str, name: str) -> str | None:
        return self._base.get_view_check_option(db, name)

    def list_views(self, db: str) -> list[str]:
        return self._base.list_views(db)

    def get_matview(self, db: str, name: str) -> str | None:
        return self._base.get_matview(db, name)

    def matview_populated(self, db: str, name: str) -> bool:
        return self._base.matview_populated(db, name)


def _run_with(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Run a ``WITH name AS (...) [, ...] <query>`` statement.

    Each CTE is materialized to rows and registered as an ephemeral collection on
    a ``CatalogBackend``; a catalog overlay maps the CTE names to TableDefs built
    from each inner query's result shape. The WITH is then stripped and the main
    statement runs against that backend + overlay, so CTE names resolve like
    tables in every path (single-table, pipeline/join, set operations, and the
    ``INSERT``/``UPDATE``/``DELETE`` write bodies). CTEs are materialized in
    order, so a later CTE may reference an earlier one."""
    with_node = _own_with(stmt)
    recursive = bool(with_node.args.get("recursive"))
    is_write = isinstance(stmt, (exp.Insert, exp.Update, exp.Delete, exp.Merge))
    if not isinstance(stmt, (exp.Select, exp.SetOperation)) and not is_write:
        raise errors.feature_not_supported(
            "WITH is supported only with SELECT / set-operation / INSERT / UPDATE / DELETE / MERGE"
        )

    backend = virtual.CatalogBackend(storage, catalog, session, db)
    cte_defs: dict[str, TableDef] = {}
    cte_catalog = _CTECatalog(catalog, cte_defs)
    for cte in with_node.expressions:
        name = cte.alias
        if not name:
            raise errors.feature_not_supported("a CTE must be named")
        col_aliases = _cte_column_aliases(cte)
        if recursive and _is_recursive_cte(cte, name):
            result = _run_recursive_cte(
                cte, name, col_aliases, backend, db, cte_catalog, cte_defs, session
            )
        elif isinstance(cte.this, (exp.Insert, exp.Update, exp.Delete)):
            # Data-modifying CTE (``WITH x AS (INSERT/UPDATE/DELETE … RETURNING …)``).
            # The write executes for its side effects against the backend (which
            # forwards to real storage); its RETURNING rows materialize as the CTE.
            # A body with no RETURNING still runs and registers an empty relation.
            result = _dispatch_cte_write(cte.this, backend, db, cte_catalog, session)
        else:
            result = _run_query(cte.this, backend, db, cte_catalog, session)
        _register_cte(backend, cte_defs, name, result.columns, result.rows, col_aliases)

    with_node.pop()  # detach the WITH so the body plans as a plain statement
    if is_write:
        # A write body reads CTEs through the backend (INSERT … SELECT FROM cte)
        # or a WHERE subquery over one. Publish the CTE-aware context so an UPDATE
        # / DELETE WHERE subquery resolves the CTE, and dispatch the write against
        # the backend (its writes forward to real storage) + overlay catalog.
        return _dispatch_cte_write(stmt, backend, db, cte_catalog, session)
    return _run_query(stmt, backend, db, cte_catalog, session)


def _dispatch_cte_write(
    stmt: exp.Expression, backend: Any, db: str, cte_catalog: Catalog, session: Session
) -> SQLResult:
    """Dispatch a write statement (an outer WITH … <write> body, or a
    data-modifying CTE's own INSERT/UPDATE/DELETE) against the CTE-aware backend +
    overlay catalog, with the CTE-aware ``SubqueryCtx`` published so any WHERE /
    source subquery over a CTE resolves."""
    token = planner._pipeline_subctx.set(
        planner.SubqueryCtx(storage=backend, db=db, catalog=cte_catalog, session=session)
    )
    try:
        return _run_statement(stmt, backend, db, cte_catalog, session)
    finally:
        planner._pipeline_subctx.reset(token)


# Guard against a runaway recursive CTE (a cyclic graph under UNION ALL recurses
# forever). Postgres relies on the user, but a surrogate must fail loudly rather
# than hang.
_MAX_RECURSION_ROWS = 1_000_000


def _cte_column_aliases(cte: exp.Expression) -> list[str]:
    """The explicit column names of ``WITH name(a, b, …) AS (…)``, or ``[]``."""
    ta = cte.args.get("alias")
    return [c.name for c in ta.columns] if ta is not None and ta.columns else []


def _is_recursive_cte(cte: exp.Expression, name: str) -> bool:
    """A CTE is recursive when its body is a UNION whose recursive term references
    the CTE's own name (a ``WITH RECURSIVE`` may still hold non-recursive CTEs)."""
    body = cte.this
    if not isinstance(body, exp.SetOperation):
        return False
    return any(t.name == name for t in body.right.find_all(exp.Table))


def _register_cte(
    backend: Any,
    cte_defs: dict[str, TableDef],
    name: str,
    columns: list[Any],
    rows: list[tuple[Any, ...]],
    col_aliases: list[str],
) -> None:
    """Materialize a CTE's rows into an ephemeral collection and record its
    TableDef. Explicit column aliases (``WITH name(a, b) AS …``) rename the inner
    query's output columns; otherwise the inner names carry through."""
    if col_aliases and len(col_aliases) != len(columns):
        raise errors.SQLError(
            "42601",
            f'WITH query "{name}" has {len(col_aliases)} columns available '
            f"but {len(columns)} columns specified",
        )
    names = col_aliases or [c.name for c in columns]
    backend.register_ephemeral(name, [dict(zip(names, row, strict=True)) for row in rows])
    # reflected=True so any column resolves; the explicit columns carry the
    # inner query's names + types (and make `SELECT *` / an empty CTE work).
    cte_defs[name] = TableDef(
        name=name,
        collection=name,
        columns=[
            Column(nm, c.type_tag, nm, pk=False, nullable=True)
            for nm, c in zip(names, columns, strict=True)
        ],
        reflected=True,
    )


def _run_recursive_cte(
    cte: exp.Expression,
    name: str,
    col_aliases: list[str],
    backend: Any,
    db: str,
    cte_catalog: Catalog,
    cte_defs: dict[str, TableDef],
    session: Session,
) -> SQLResult:
    """Evaluate a recursive CTE by semi-naive iteration: run the anchor term for
    the seed rows, then repeatedly run the recursive term against just the rows
    produced by the previous step (registered under the CTE name) until it yields
    nothing new. ``UNION`` dedups against all rows seen; ``UNION ALL`` keeps every
    row."""
    body = cte.this
    if not isinstance(body, exp.Union):
        raise errors.feature_not_supported(
            "a recursive CTE must be UNION [ALL] of an anchor and a recursive term"
        )
    union_all = not bool(body.args.get("distinct"))

    anchor = _run_query(body.left, backend, db, cte_catalog, session)
    columns = anchor.columns
    if col_aliases and len(col_aliases) != len(columns):
        raise errors.SQLError(
            "42601",
            f'WITH query "{name}" has {len(columns)} columns available '
            f"but {len(col_aliases)} columns specified",
        )
    names = col_aliases or [c.name for c in columns]
    all_rows: list[tuple[Any, ...]] = list(anchor.rows)
    seen = {_setop_key(r) for r in all_rows}
    if not union_all:
        all_rows = _dedup_rows(all_rows)
        working = list(all_rows)
    else:
        working = list(all_rows)

    while working:
        backend.register_ephemeral(name, [dict(zip(names, row, strict=True)) for row in working])
        cte_defs[name] = TableDef(
            name=name,
            collection=name,
            columns=[
                Column(nm, c.type_tag, nm, pk=False, nullable=True)
                for nm, c in zip(names, columns, strict=True)
            ],
            reflected=True,
        )
        step = _run_query(body.right, backend, db, cte_catalog, session)
        if len(step.columns) != len(columns):
            raise errors.SQLError(
                "42601", f'recursive query "{name}" column count does not match the anchor'
            )
        fresh: list[tuple[Any, ...]] = []
        for row in step.rows:
            if union_all:
                fresh.append(row)
            else:
                key = _setop_key(row)
                if key not in seen:
                    seen.add(key)
                    fresh.append(row)
        all_rows.extend(fresh)
        if len(all_rows) > _MAX_RECURSION_ROWS:
            raise errors.SQLError(
                "54001",
                f'recursive query "{name}" exceeded {_MAX_RECURSION_ROWS} rows '
                "(possible infinite recursion)",
            )
        working = fresh

    return SQLResult(command_tag=f"SELECT {len(all_rows)}", columns=columns, rows=all_rows)


def _run_set_operation(
    stmt: exp.SetOperation, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Run ``A UNION|INTERSECT|EXCEPT [ALL] B`` (chains nest on the left).

    Each arm runs through the full SELECT path; the rows are combined with the
    operation's multiset semantics, the output columns are taken from the first
    arm (Postgres' rule), and any ORDER BY / LIMIT on the set-operation node
    applies to the combined result."""
    left = _run_query(stmt.left, storage, db, catalog, session)
    right = _run_query(stmt.right, storage, db, catalog, session)
    op = type(stmt).__name__.upper()
    if len(left.columns) != len(right.columns):
        raise errors.SQLError("42601", f"each {op} query must have the same number of columns")
    distinct = bool(stmt.args.get("distinct"))
    rows = _combine_setop_rows(stmt, left.rows, right.rows, distinct)
    rows = _setop_order_limit(stmt, rows, left.columns)
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=left.columns, rows=rows, rowcount=len(rows)
    )


def _setop_key(row: tuple[Any, ...]) -> tuple[str, ...]:
    """A hashable identity for a result row (matches the SELECT DISTINCT dedup)."""
    return tuple(repr(v) for v in row)


def _dedup_rows(rows: list[tuple[Any, ...]]) -> list[tuple[Any, ...]]:
    seen: set = set()
    out: list[tuple[Any, ...]] = []
    for row in rows:
        key = _setop_key(row)
        if key not in seen:
            seen.add(key)
            out.append(row)
    return out


def _multiset_filter(
    left: list[tuple[Any, ...]],
    right: list[tuple[Any, ...]],
    *,
    keep_when_present: bool,
    distinct: bool,
) -> list[tuple[Any, ...]]:
    """INTERSECT (``keep_when_present=True``) / EXCEPT (``False``) over rows.

    DISTINCT collapses the result to set semantics; ``ALL`` keeps multiplicities
    (min of the two counts for INTERSECT, left-minus-right for EXCEPT)."""
    if distinct:
        right_keys = {_setop_key(r) for r in right}
        seen: set = set()
        out: list[tuple[Any, ...]] = []
        for row in left:
            key = _setop_key(row)
            if (key in right_keys) == keep_when_present and key not in seen:
                seen.add(key)
                out.append(row)
        return out
    from collections import Counter

    counts = Counter(_setop_key(r) for r in right)
    out = []
    for row in left:
        key = _setop_key(row)
        if counts[key] > 0:
            counts[key] -= 1
            if keep_when_present:  # INTERSECT ALL: a matched copy survives
                out.append(row)
        elif not keep_when_present:  # EXCEPT ALL: an unmatched copy survives
            out.append(row)
    return out


def _combine_setop_rows(
    stmt: exp.SetOperation,
    left: list[tuple[Any, ...]],
    right: list[tuple[Any, ...]],
    distinct: bool,
) -> list[tuple[Any, ...]]:
    if isinstance(stmt, exp.Union):
        rows = left + right
        return _dedup_rows(rows) if distinct else rows
    if isinstance(stmt, exp.Intersect):
        return _multiset_filter(left, right, keep_when_present=True, distinct=distinct)
    if isinstance(stmt, exp.Except):
        return _multiset_filter(left, right, keep_when_present=False, distinct=distinct)
    raise errors.feature_not_supported(f"unsupported set operation: {type(stmt).__name__}")


def _setop_order_index(node: exp.Expression, columns: list[Any]) -> int:
    """Resolve an ORDER BY term on a set operation to an output-column index:
    an integer literal is an ordinal position; otherwise it matches a column by
    output name (set-operation ORDER BY can only reference the result columns)."""
    if isinstance(node, exp.Literal) and not node.is_string:
        i = int(node.this) - 1
        if not 0 <= i < len(columns):
            raise errors.SQLError("42P10", f"ORDER BY position {i + 1} is not in select list")
        return i
    name = node.name if isinstance(node, exp.Column) else None
    if name is not None:
        for i, col in enumerate(columns):
            if col.name == name:
                return i
    raise errors.SQLError("42703", f'ORDER BY column "{node.sql()}" does not exist')


def _setop_order_limit(
    stmt: exp.SetOperation, rows: list[tuple[Any, ...]], columns: list[Any]
) -> list[tuple[Any, ...]]:
    order = stmt.args.get("order")
    if order is not None:
        idxs = [_setop_order_index(o.this, columns) for o in order.expressions]
        specs = [
            (-1 if o.args.get("desc") else 1, planner._nulls_first(o)) for o in order.expressions
        ]
        rows = list(rows)
        executor._pg_sort(rows, lambda r: tuple(r[i] for i in idxs), specs)
    limit, skip = planner._limit_skip(stmt)
    if skip:
        rows = rows[skip:]
    if limit:
        rows = rows[:limit]
    return rows


def _values_column_names(stmt: exp.Values, ncols: int) -> list[str]:
    """Output column names for a ``VALUES`` list: an explicit ``AS t(a, b)`` alias's
    columns if present, else Postgres's default ``column1`` … ``columnN``."""
    alias = stmt.args.get("alias")
    cols = [c.name for c in alias.columns] if alias is not None and alias.columns else []
    names = list(cols[:ncols])
    names += [f"column{j + 1}" for j in range(len(names), ncols)]
    return names


def _run_values(
    stmt: exp.Values, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Evaluate a ``VALUES (…), (…) [ORDER BY …] [LIMIT …]`` constant table — a
    standalone query or a set-operation arm.

    Each cell is a constant expression (evaluated with no row scope); every row must
    have the same width. Columns are named ``column1`` … ``columnN`` (or the
    ``AS t(…)`` alias's names) and typed from the first non-NULL value in each
    position. Postgres allows only an ordinal / output-column ``ORDER BY`` on a
    ``VALUES`` list, which ``_setop_order_limit`` enforces (an expression ``ORDER BY``
    is rejected there, matching Postgres)."""
    tuples = stmt.expressions
    if not tuples:
        raise errors.SQLError("42601", "VALUES lists must have at least one row")
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    ncols = len(tuples[0].expressions)
    rows: list[tuple[Any, ...]] = []
    for t in tuples:
        cells = t.expressions
        if len(cells) != ncols:
            raise errors.SQLError("42601", "VALUES lists must all be the same length")
        rows.append(tuple(scalar.evaluate(c, planner._const_scope, ctx) for c in cells))
    names = _values_column_names(stmt, ncols)
    columns: list[ColumnDesc] = []
    for j in range(ncols):
        val = next((r[j] for r in rows if r[j] is not None), None)
        tag = planner._infer_value_tag(val)
        columns.append(ColumnDesc(names[j], tag, typemap.PG_OID.get(tag, typemap.PG_OID["text"])))
    rows = _setop_order_limit(stmt, rows, columns)
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


def _run_set(stmt: exp.Set, session: Session) -> SQLResult:
    reported: list[tuple[str, str]] = []
    for item in stmt.expressions:
        # SET TRANSACTION [ISOLATION LEVEL ...|READ ONLY|READ WRITE|DEFERRABLE]:
        # applies the characteristics to the current transaction (reported via
        # the transaction_* GUCs; single-node, so behaviour doesn't change).
        if (
            isinstance(item, exp.SetItem)
            and str(item.args.get("kind") or "").upper() == "TRANSACTION"
        ):
            if session.txn_handle is not None:
                chars = _parse_txn_characteristics(item.this.sql() if item.this else item.sql())
                for cname, cvalue in chars.items():
                    session.set_local(cname, cvalue)
            continue
        inner = item.this if isinstance(item, exp.SetItem) else item
        if not isinstance(inner, exp.EQ):
            raise errors.feature_not_supported(f"unsupported SET item: {item.sql()}")
        # A custom (extension) GUC is spelled ``namespace.name`` and parses as a
        # Column whose ``table`` part is the namespace — reconstruct the full
        # dotted name so SHOW (which reads the whole literal) can find it.
        lhs = inner.this
        if isinstance(lhs, exp.Column) and lhs.table:
            name = sql_session.canonical_guc_name(f"{lhs.table}.{lhs.name}")
        else:
            name = sql_session.canonical_guc_name(lhs.name)
        value_node = inner.expression
        if isinstance(value_node, exp.Literal):
            value = value_node.this
        elif isinstance(value_node, exp.Neg) and isinstance(value_node.this, exp.Literal):
            # ``SET extra_float_digits = -1`` — Neg's .name is the BARE inner
            # literal, which silently dropped the sign (pgtest float corpus).
            value = f"-{value_node.this.this}"
        else:
            value = value_node.name or value_node.sql()
        # SET LOCAL applies only until the end of the current transaction. Outside
        # a transaction block it has no lasting effect (Postgres warns and drops it).
        is_local = isinstance(item, exp.SetItem) and str(item.args.get("kind") or "").upper() == (
            "LOCAL"
        )
        # Canonicalize ONCE, before the local/session split — the report at the
        # bottom must carry the same spelling that was stored (pgtest
        # param_status reads SET LOCAL's DateStyle report).
        if name.lower() in ("timezone", "time zone"):
            value = sql_session.canonical_timezone_setting(str(value))
        value = sql_session.canonical_guc_value(name, str(value))
        if is_local:
            if session.txn_handle is not None:
                session.set_local(name, str(value))
            else:
                continue  # SET LOCAL outside a transaction — no lasting effect
        else:
            if name.lower() == "client_encoding":
                # Canonicalise (SHOW / ParameterStatus report the PG spelling)
                # and reject encodings the wire layer can't convert.
                canonical = sql_session.canonical_client_encoding(str(value))
                if canonical is None:
                    raise errors.SQLError(
                        "22023", f'invalid value for parameter "client_encoding": "{value}"'
                    )
                value = canonical
            if session.txn_handle is not None:
                # A plain SET inside a block unwinds on ROLLBACK (PG semantics);
                # capture the pre-SET value before overwriting.
                session.record_txn_guc(name)
            session.settings[name] = str(value)
        if name.lower() == "role":
            # ``SET [LOCAL] role = 'x'`` is the GUC spelling of SET ROLE: it
            # switches the current role, so PG reports is_superuser with it
            # (pgtest param_status uses this spelling inside a block).
            ident = _unquote_ident(str(value))
            session.role = None if ident.upper() in ("NONE", "DEFAULT") else ident
            reported.extend(_superuser_status(session))
        if name in REPORTABLE_GUCS:
            reported.append((name, str(value)))
    return SQLResult(command_tag="SET", parameter_status=reported)


# SET [SESSION | LOCAL] ROLE { name | NONE | DEFAULT }  /
# SET [SESSION | LOCAL] SESSION AUTHORIZATION { name | DEFAULT } — the target after
# stripping the optional SESSION/LOCAL scope keyword.
_SET_ROLE_RE = re.compile(r"(?is)^\s*(?:SESSION\s+|LOCAL\s+)?ROLE\s+(.+?)\s*;?\s*$")
_SET_AUTHZ_RE = re.compile(
    r"(?is)^\s*(?:SESSION\s+|LOCAL\s+)?SESSION\s+AUTHORIZATION\s+(.+?)\s*;?\s*$"
)


def _unquote_ident(text: str) -> str:
    text = text.strip()
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "\"'":
        return text[1:-1]
    return text


def _can_assume_identity(session: Session, target: str) -> bool:
    """Whether this session may SET ROLE / SET SESSION AUTHORIZATION to ``target``.
    With authorization off (trust mode / embedded API) anything goes. When active,
    the login may assume its own identity, a role it holds, or anything if it is a
    superuser (``root``) — otherwise it can't borrow another identity's grants."""
    if not session.authz_active:
        return True
    if target == session.login_user or target == session.user:
        return True
    role_names = {
        (r.get("role") if isinstance(r, dict) else getattr(r, "role", None)) for r in session.roles
    }
    return target in role_names or "root" in role_names


def _superuser_status(session: Session) -> list[tuple[str, str]]:
    """``is_superuser`` ParameterStatus for the session's CURRENT identity, but
    only when it changed — PG reports the GUC on a role switch and stays quiet
    when the role is re-set to what it already was (pgtest param_status sends
    the same SET ROLE twice and expects one report)."""
    is_super = session.role is None or session.role == (session.login_user or session.user)
    value = "on" if is_super else "off"
    if session.settings.get("is_superuser") == value:
        return []
    session.settings["is_superuser"] = value
    return [("is_superuser", value)]


def _run_authorization_command(verb: str, tail: str, session: Session) -> SQLResult | None:
    """``SET ROLE`` / ``SET SESSION AUTHORIZATION`` and their ``RESET`` forms, which
    change the session's current role / session user (#128). Returns None if the
    command isn't one of these (the caller handles the generic SET/RESET)."""
    if verb == "RESET":
        key = tail.strip().upper()
        if key == "ROLE":
            session.role = None
            session.settings.pop("role", None)
            return SQLResult(command_tag="RESET", parameter_status=_superuser_status(session))
        if key == "SESSION AUTHORIZATION":
            session.user = session.login_user or session.user
            session.role = None
            session.settings.pop("role", None)
            return SQLResult(command_tag="RESET", parameter_status=_superuser_status(session))
        return None
    if verb != "SET":
        return None
    m = _SET_AUTHZ_RE.match(tail)
    if m is not None:
        target = m.group(1).strip()
        if target.upper() == "DEFAULT":
            session.user = session.login_user or session.user
        else:
            ident = _unquote_ident(target)
            if not _can_assume_identity(session, ident):
                raise errors.SQLError(
                    "42501", f'permission denied to set session authorization to "{ident}"'
                )
            session.user = ident
        session.role = None  # a new session user resets the current role
        session.settings.pop("role", None)
        return SQLResult(command_tag="SET")
    m = _SET_ROLE_RE.match(tail)
    if m is not None:
        target = m.group(1).strip()
        if target.upper() in ("NONE", "DEFAULT"):
            session.role = None
            session.settings.pop("role", None)
        else:
            ident = _unquote_ident(target)
            if not _can_assume_identity(session, ident):
                raise errors.SQLError("42501", f'permission denied to set role "{ident}"')
            session.role = ident
            session.settings["role"] = ident
        return SQLResult(command_tag="SET", parameter_status=_superuser_status(session))
    m = _SET_TIME_ZONE_RE.match(tail)
    if m is not None:
        # ``SET TIME ZONE <value>`` takes no ``=``/``TO``, so the generic
        # name-value fallback never matched it and the statement set nothing.
        # This is the spelling JDBC uses to pin a connection's zone.
        raw = m.group(1).strip()
        if raw.upper() in ("DEFAULT", "LOCAL"):
            value = sql_session.GUC_DEFAULTS.get("TimeZone", "UTC")
        else:
            # Canonicalize like the generic SET path: a numeric offset becomes
            # PG's POSIX zone spec (``+6`` -> ``<+06>-06``), GMT/UTC prefixes
            # uppercase (pgtest param_status reads the reported value).
            value = sql_session.canonical_timezone_setting(_unquote_ident(raw))
        # ``SET LOCAL TIME ZONE`` reverts at the end of the transaction, like
        # the generic SET LOCAL path (the corpus rolls one back).
        if tail.lstrip().upper().startswith("LOCAL") and session.txn_handle is not None:
            session.set_local("TimeZone", value)
        else:
            if session.txn_handle is not None:
                session.record_txn_guc("TimeZone")
            session.settings["TimeZone"] = value
        return SQLResult(command_tag="SET", parameter_status=[("TimeZone", value)])
    return None


# ``name = value[, value…]`` / ``name TO value[, value…]`` — the Command-fallback
# SET tail (multi-part values like ``datestyle = German, YMD``). Excludes the
# SESSION CHARACTERISTICS / TRANSACTION forms, which stay no-ops.
# ``SET [SESSION|LOCAL] TIME ZONE <value>`` — no ``=``/``TO``, so it needs its
# own pattern; ``DEFAULT`` / ``LOCAL`` reset the GUC.
_SET_TIME_ZONE_RE = re.compile(r"(?is)^\s*(?:SESSION\s+|LOCAL\s+)?TIME\s+ZONE\s+(.+?)\s*;?\s*$")


_SET_MULTI_RE = re.compile(
    r"(?is)^(?!session\s+characteristics|transaction\b)"
    r'([A-Za-z_][\w.]*|"[^"]+")\s*(?:=|\bto\b)\s*(.+?)\s*;?\s*$'
)


# ``RAISE level 'fmt'[, arg…] [USING option = 'value', …];`` inside a DO body.
_RAISE_RE = re.compile(
    r"(?is)\braise\s+(?P<level>debug|log|info|notice|warning|exception)\s+"
    r"'(?P<fmt>(?:[^']|'')*)'\s*(?P<args>(?:,[^;]*?)?)\s*"
    r"(?:using\s+(?P<using>[^;]*?))?\s*;"
)


def _run_do_block(
    body: str,
    session: Session,
    storage: Any = None,
    db: str | None = None,
    catalog: Catalog | None = None,
) -> SQLResult:
    """A minimal plpgsql interpreter for ``DO`` blocks: executes each ``RAISE``
    statement in the body — notices/warnings collect on the result (the wire
    layer sends NoticeResponse messages), an EXCEPTION raises with its USING
    ERRCODE (default P0001 raise_exception). Anything else in the body is
    ignored (BEGIN/END scaffolding)."""
    from secantus.sql import scalar

    notices: list[tuple[str, str]] = []
    ctx = scalar.ScalarContext(storage=None, catalog=None, db=session.database, session=session)
    # ``EXECUTE <string expr>;`` — dynamic SQL: evaluate the expression
    # (``format('insert into "%s" …', chr(8364))``) and run the result, so
    # errors from the dynamic statement surface with their real SQLSTATE.
    if storage is not None and db is not None:
        for em in re.finditer(r"(?is)\bexecute\s+(?P<expr>[^;]+);", body):
            try:
                node = sqlglot.parse_one(f"SELECT {em.group('expr')}", read="postgres").expressions[
                    0
                ]
                dyn_sql = scalar.evaluate(node, planner._const_scope, ctx)
            except errors.SQLError:
                raise
            except Exception:  # noqa: BLE001 — unevaluable EXECUTE arg
                continue
            if dyn_sql:
                run_sql(storage, db, str(dyn_sql), session=session)
    for m in _RAISE_RE.finditer(body):
        level = m.group("level").upper()
        fmt = m.group("fmt").replace("''", "'")
        args_text = (m.group("args") or "").lstrip(",").strip()
        arg_values: list[str] = []
        if args_text:
            for part in _split_top_level_commas(args_text):
                try:
                    node = sqlglot.parse_one(f"SELECT {part}", read="postgres").expressions[0]
                    val = scalar.evaluate(node, planner._const_scope, ctx)
                except Exception:  # noqa: BLE001 — a bad arg renders as its text
                    val = part
                arg_values.append("" if val is None else str(val))
        message = fmt
        for val in arg_values:
            message = message.replace("%", val, 1)
        if level == "EXCEPTION":
            errcode = "P0001"  # raise_exception
            using = m.group("using") or ""
            code_m = re.search(r"(?i)errcode\s*=\s*'([^']+)'", using)
            if code_m is not None:
                errcode = code_m.group(1)
            raise errors.SQLError(errcode, message)
        if level in ("NOTICE", "WARNING", "INFO"):
            notices.append((level, message))
        # DEBUG/LOG stay server-side, like PG's default client_min_messages.
    return SQLResult(command_tag="DO", notices=notices)


def _split_top_level_commas(text: str) -> list[str]:
    parts, depth, buf = [], 0, []
    in_str = False
    for ch in text:
        if in_str:
            buf.append(ch)
            if ch == "'":
                in_str = False
            continue
        if ch == "'":
            in_str = True
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append("".join(buf).strip())
            buf = []
            continue
        buf.append(ch)
    if buf:
        parts.append("".join(buf).strip())
    return parts


def _run_command(
    stmt: exp.Command,
    session: Session,
    storage: Any = None,
    db: str | None = None,
    catalog: Catalog | None = None,
) -> SQLResult:
    verb = str(stmt.this).upper()
    if verb == "DO":
        raw = stmt.expression.this if isinstance(stmt.expression, exp.Literal) else ""
        return _run_do_block(str(raw), session, storage, db, catalog)
    if verb == "PUBSUB":
        # LISTEN / NOTIFY / UNLISTEN routed through parse() (the extended
        # protocol's Parse path — run_sql intercepts them before parsing).
        raw = stmt.expression.this if isinstance(stmt.expression, exp.Literal) else ""
        handled = _maybe_pubsub(str(raw), session)
        if handled is not None:
            return handled
        raise errors.syntax_error(f'syntax error at or near "{str(raw)[:40]}"')
    if verb == "CREATE_PROCEDURE":
        raw = stmt.expression.this if isinstance(stmt.expression, exp.Literal) else ""
        return _create_procedure(str(raw), db, catalog, session)
    if verb == "DROP_PROCEDURE":
        raw = stmt.expression.this if isinstance(stmt.expression, exp.Literal) else ""
        return _drop_procedure(str(raw), db, catalog)
    if verb == "CALL":
        tail = stmt.expression.this if isinstance(stmt.expression, exp.Literal) else ""
        return _call_procedure(str(tail), session, storage, db, catalog)
    arg = stmt.expression
    if isinstance(arg, exp.Literal):
        name = arg.this
    elif isinstance(arg, str):
        name = arg  # a Command fallback carries its tail as a bare string
    elif arg is not None:
        name = arg.name
    else:
        name = ""
    name = str(name).strip()
    # SET ROLE / SET SESSION AUTHORIZATION (and their RESET forms) change the
    # session's current role / session user before the generic SET/RESET below.
    # These use the full command tail (``_command_text``) since ``SET``'s tail
    # ("ROLE analyst") isn't a bare Literal.
    authz_cmd = _run_authorization_command(verb, _command_text(stmt), session)
    if authz_cmd is not None:
        return authz_cmd
    if verb == "SHOW":
        if name.upper() == "ALL":
            # SHOW ALL — every GUC as (name, setting, description). psql renders
            # this as a three-column table.
            oid = typemap.PG_OID["text"]
            rows = [(k, v, "") for k, v in sorted(session.all_settings().items())]
            return SQLResult(
                command_tag="SHOW",
                columns=[
                    ColumnDesc("name", "text", oid),
                    ColumnDesc("setting", "text", oid),
                    ColumnDesc("description", "text", oid),
                ],
                rows=rows,
                rowcount=len(rows),
            )
        # PG's special multi-word spellings resolve to their GUC (pgjdbc's
        # getTransactionIsolation issues the first form verbatim).
        folded = re.sub(r"\s+", " ", name.strip().lower())
        if folded == "transaction isolation level":
            name = "transaction_isolation"
        elif folded == "time zone":
            name = "timezone"
        value = session.get_setting(name)
        return SQLResult(
            command_tag="SHOW",
            columns=[ColumnDesc(name, "text", typemap.PG_OID["text"])],
            rows=[(value,)],
            rowcount=1,
        )
    if verb == "RESET":
        session.settings.pop(name, None)
        reported = [(name, session.get_setting(name))] if name in REPORTABLE_GUCS else []
        return SQLResult(command_tag="RESET", parameter_status=reported)
    if verb == "SET":
        # SET TRANSACTION <chars> — applies to the open transaction only.
        m_txn = re.match(r"(?is)^transaction\s+(?P<tail>.+?)\s*;?\s*$", name)
        if m_txn is not None:
            if session.txn_handle is not None:
                for cname, cvalue in _parse_txn_characteristics(m_txn.group("tail")).items():
                    session.set_local(cname, cvalue)
            return SQLResult(command_tag="SET")
        # SET SESSION CHARACTERISTICS AS TRANSACTION <chars> — sets the
        # session-default transaction characteristics (default_transaction_*).
        m_chars = re.match(
            r"(?is)^session\s+characteristics\s+as\s+transaction\s+(?P<tail>.+?)\s*;?\s*$", name
        )
        if m_chars is not None:
            for cname, cvalue in _parse_txn_characteristics(m_chars.group("tail")).items():
                session.settings[f"default_{cname}"] = cvalue
            return SQLResult(command_tag="SET")
        # ``SET name = v1, v2`` (DateStyle's two-part value) parses as a raw
        # Command, not exp.Set — store and report it like the structured path.
        m_set = _SET_MULTI_RE.match(name)
        if m_set is not None:
            guc = sql_session.canonical_guc_name(m_set.group(1))
            value = ", ".join(part.strip().strip("'\"") for part in m_set.group(2).split(","))
            if guc == "TimeZone":
                value = sql_session.canonical_timezone_setting(value)
            if guc == "client_encoding":
                # Canonicalise like the structured SET path (utf-8/utf_8 → UTF8);
                # ParameterStatus must echo the PG spelling.
                canonical = sql_session.canonical_client_encoding(value)
                if canonical is None:
                    raise errors.SQLError(
                        "22023", f'invalid value for parameter "client_encoding": "{value}"'
                    )
                value = canonical
            session.settings[guc] = value
            reported = [(guc, value)] if guc in REPORTABLE_GUCS else []
            return SQLResult(command_tag="SET", parameter_status=reported)
        # SET SESSION CHARACTERISTICS AS TRANSACTION ... falls back to a Command;
        # accepted as a no-op (single-node — no isolation/read-only semantics).
        return SQLResult(command_tag="SET")
    raise errors.feature_not_supported(f"command {verb} is not supported")
