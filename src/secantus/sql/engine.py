"""``run_sql`` — the embedded SQL entry point.

Parses a SQL string, plans each statement, and executes it against a
``Storage`` instance, returning one ``SQLResult`` per statement. This is both
the embedded API and what the PostgreSQL-wire server drives. A per-connection
``Session`` carries the database, user, and GUC settings so session functions
and ``SHOW`` / ``SET`` resolve against real state.
"""

from __future__ import annotations

import copy
import datetime as _dt
import re
from dataclasses import dataclass
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
    typemap,
    virtual,
)
from secantus.sql import explain as explain_mod
from secantus.sql.catalog import Catalog, Column, TableDef
from secantus.sql.result import ColumnDesc, SQLResult
from secantus.sql.session import (
    REPORTABLE_GUCS,
    PreparedXact,
    PreparedXactRegistry,
    Session,
    _Cursor,
    _Savepoint,
)


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
    # Two-phase commit (#139) is handled before sqlglot: it cannot parse
    # ``COMMIT PREPARED`` / ``ROLLBACK PREPARED`` at all, and ``PREPARE
    # TRANSACTION`` collides with the SQL-level ``PREPARE name AS`` (#121).
    two_phase = _maybe_two_phase(sql, storage, db, catalog, session)
    if two_phase is not None:
        return [two_phase]
    results: list[SQLResult] = []
    for stmt in planner.parse(sql):
        results.append(_normalize_result(_dispatch(stmt, storage, db, catalog, session)))
    return results


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
    if isinstance(stmt, exp.Transaction):
        return _begin_txn(storage, session)
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

    if session.txn_handle is not None:
        try:
            with storage.use_user_transaction(session.txn_handle):
                _capture_savepoint_snapshots(stmt, storage, db, catalog, session)
                return _run_statement(stmt, storage, db, catalog, session)
        except Exception:
            session.txn_failed = True
            raise
    return _run_statement(stmt, storage, db, catalog, session)


def _begin_txn(storage: Any, session: Session) -> SQLResult:
    # A nested BEGIN is a no-op in Postgres (it warns and stays in the block).
    if session.txn_handle is None:
        session.txn_handle = storage.begin_user_transaction()
        session.txn_failed = False
        session.savepoints = []
        session.reset_deferred()
    return SQLResult(command_tag="BEGIN")


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
            raise
    _end_txn_state(session)
    if handle is None:
        return SQLResult(command_tag="COMMIT")  # no open block — Postgres warns, returns COMMIT
    if failed:
        # COMMIT of an aborted block actually rolls back (and tags ROLLBACK).
        storage.abort_user_transaction(handle)
        return SQLResult(command_tag="ROLLBACK")
    storage.commit_user_transaction(handle)
    if session.notify_hub is not None:
        for channel, payload in buffered_notifies:
            session.notify_hub.notify(channel, payload, session.backend_pid)
    return SQLResult(command_tag="COMMIT")


def _end_txn_state(session: Session) -> None:
    """Clear all per-transaction session state at the end of a block."""
    session.txn_handle = None
    session.txn_failed = False
    session.savepoints = []
    session.pending_notifies = []  # NOTIFYs in the block are flushed (commit) or dropped (rollback)
    session.reset_deferred()
    session.restore_local_gucs()  # SET LOCAL reverts at end of transaction
    session.release_xact_advisory_locks()  # pg_advisory_xact_lock* release at txn end
    _close_non_hold_cursors(session)  # WITHOUT HOLD cursors close at end of txn


def _rollback_txn(storage: Any, session: Session) -> SQLResult:
    handle = session.txn_handle
    _end_txn_state(session)
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
    session.savepoints.append(_Savepoint(name=name))
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
    return SQLResult(command_tag="ROLLBACK")


def _capture_savepoint_snapshots(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> None:
    """Before a write runs, snapshot its target collection into every open
    savepoint that hasn't captured it yet — that pins each savepoint's view of the
    collection to its establishment state (nothing wrote to it in between)."""
    if not session.savepoints:
        return
    coll = _write_target_collection(stmt, catalog, db, storage)
    if coll is None:
        return
    snap: list | None = None
    for fr in session.savepoints:
        if coll in fr.snapshots:
            continue
        if snap is None:
            snap = [copy.deepcopy(d) for d in storage.find_matching(db, coll, {})]
        fr.snapshots[coll] = snap


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
    name = table_node.name
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
    stmts = planner.parse(m.group("query"))
    if len(stmts) != 1:
        raise errors.syntax_error("DECLARE CURSOR expects a single query")
    result = _run_query(stmts[0], storage, db, catalog, session)
    rows = list(result.rows)
    # Cap the materialized row set a single cursor retains (SecantusDB cursors
    # are eager, unlike mongod's lazy ones). Bounds the memory one connection can
    # pin; the number is generous so real queries aren't affected. (#194)
    if len(rows) > MAX_CURSOR_ROWS:
        raise errors.program_limit_exceeded(
            f"cursor result too large: {len(rows)} rows exceeds the {MAX_CURSOR_ROWS} limit"
        )
    session.cursors[name] = _Cursor(name=name, columns=result.columns, rows=rows, pos=-1, hold=hold)
    return SQLResult(command_tag="DECLARE CURSOR")


_FETCH_DIRECTIONS = frozenset(
    {"NEXT", "PRIOR", "FIRST", "LAST", "ABSOLUTE", "RELATIVE", "FORWARD", "BACKWARD"}
)


def _parse_fetch(tail: str) -> tuple[str, int | None, str]:
    """Parse a FETCH/MOVE tail into ``(kind, count, cursor_name)``. ``kind`` is one
    of forward / backward / absolute / relative; ``count`` is the row count (None =
    ALL). The cursor name is the final token; an optional ``FROM`` / ``IN`` before
    it is dropped."""
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
        return "forward", as_count(spec), name  # FETCH n / FETCH ALL
    rest = spec[1:]
    if head == "NEXT":
        return "forward", 1, name
    if head == "PRIOR":
        return "backward", 1, name
    if head == "FIRST":
        return "absolute", 1, name
    if head == "LAST":
        return "absolute", -1, name
    if head == "FORWARD":
        return "forward", as_count(rest), name
    if head == "BACKWARD":
        return "backward", as_count(rest), name
    # ABSOLUTE / RELATIVE require a count.
    if not rest:
        raise errors.syntax_error(f"{head} requires a count")
    return head.lower(), int(rest[0]), name


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


def _fetch_cursor(stmt: exp.Command, session: Session, *, move: bool = False) -> SQLResult:
    """``FETCH`` returns the moved-over rows; ``MOVE`` performs the same
    positioning but returns only the count (no result set)."""
    kind, count, name = _parse_fetch(_command_tail(stmt))
    cur = session.cursors.get(name)
    if cur is None:
        raise errors.SQLError("34000", f'cursor "{name}" does not exist')
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
    target = _require_table(catalog, db, stmt.this.name, storage)
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
    from secantus.sql.executor import ColumnDesc

    columns = [
        ColumnDesc(name, col.type_tag, typemap.PG_OID.get(col.type_tag, 25))
        for name, col, _ in returning
    ]

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
    target = _require_table(catalog, db, stmt.this.name, storage)
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


def _run_update_from(
    stmt: exp.Update, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """``UPDATE t SET … FROM u [, v …] WHERE …`` — update each target row that joins
    a source row satisfying the WHERE; the SET right-hand sides may reference the
    source (``SET col = u.col``)."""
    target_node = stmt.this
    target = _require_table(catalog, db, target_node.name, storage)
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
                    f'duplicate key value violates unique constraint "{target.name}_pkey"',
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
    return _normalize_result(_dispatch(stmt, storage, db, catalog, session))


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
    if isinstance(stmt, exp.Command) and str(stmt.this).upper() == "SHOW":
        oid = typemap.PG_OID["text"]
        if _show_name(stmt).upper() == "ALL":
            return [
                ColumnDesc("name", "text", oid),
                ColumnDesc("setting", "text", oid),
                ColumnDesc("description", "text", oid),
            ]
        return [ColumnDesc(_show_name(stmt), "text", oid)]
    if _own_with(stmt) is not None:
        # Resolving a CTE query's columns would require materializing the CTEs
        # (execution), which Describe must not do — defer to Execute's
        # RowDescription by reporting NoData here.
        return None
    if isinstance(stmt, exp.SetOperation):
        # A set operation's result shape is its first arm's (descend chained ops).
        arm = stmt
        while isinstance(arm, exp.SetOperation):
            arm = arm.left
        return _describe_statement(storage, db, arm, session, catalog)
    if not isinstance(stmt, exp.Select):
        return None
    table_node = stmt.find(exp.Table)
    if table_node is None:
        plan = planner.plan_constant_select(stmt, session)
        return [ColumnDesc(n, t, typemap.PG_OID.get(t, 25)) for n, t, _ in plan.columns]
    if planner.select_needs_pipeline(stmt):
        pplan = planner.plan_pipeline_select(stmt, db, catalog, storage)
        return [ColumnDesc(n, t, typemap.PG_OID.get(t, 25)) for n, t in pplan.out_columns]
    schema = table_node.args.get("db")
    schema_name = schema.name if schema is not None else None
    vtable = virtual.lookup(schema_name, table_node.name)
    if vtable is not None:
        table = vtable.table_def()
    else:
        table = catalog.get(db, table_node.name) or reflect.reflect(storage, db, table_node.name)
    if table is None:
        return None  # undefined table — let Execute raise the real error
    select_plan = planner.plan_select(stmt, table)
    if select_plan.count_star:
        return [ColumnDesc(select_plan.count_alias, "int8", typemap.PG_OID["int8"])]
    return [
        ColumnDesc(name, col.type_tag, typemap.PG_OID.get(col.type_tag, 25))
        for name, col in select_plan.out_columns
    ]


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
    fmt: str  # "text" | "csv"
    delimiter: str
    null: str
    header: bool
    # For ``COPY (SELECT …) TO STDOUT``: the pre-rendered copy-stream cells of the
    # query result (query-form COPY is dump-only; ``table`` is None).
    query_rows: list[list] | None = None


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
    to_stdout = not bool(stmt.args.get("kind"))  # kind True = FROM, False = TO
    fmt, delimiter, null, header = _copy_options(stmt)
    if delimiter is None:
        delimiter = "," if fmt == "csv" else "\t"
    if null is None:
        null = "" if fmt == "csv" else "\\N"

    this = stmt.this
    if isinstance(this, (exp.Subquery, exp.Select)):
        if not to_stdout:
            raise errors.syntax_error("COPY (query) must be COPY … TO, not FROM")
        select = this.this if isinstance(this, exp.Subquery) else this
        columns, query_rows = _copy_query_rows(select, storage, db, catalog, session)
        return CopyPlan(None, columns, True, fmt, delimiter, null, header, query_rows=query_rows)

    if isinstance(this, exp.Schema):
        tname = this.this.name
        columns = [c.name for c in this.expressions]
    else:
        tname = this.name
        columns = None
    table = catalog.get(db, tname) or reflect.reflect(storage, db, tname)
    if table is None:
        raise errors.undefined_table(tname)
    if columns is None:
        cols = list(table.columns)
        if not to_stdout:  # a generated column can't be copied in
            cols = [c for c in cols if c.generated is None and c.identity != "always"]
        columns = [c.name for c in cols]
    return CopyPlan(table, columns, to_stdout, fmt, delimiter, null, header)


def _copy_query_rows(
    select: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session | None
) -> tuple[list[str], list[list]]:
    """Run a ``COPY (SELECT …) TO`` query and render its result as copy-stream
    cells (string / None), returning ``(column_names, rows)``."""
    result = _run_query(select, storage, db, catalog, session or Session(database=db))
    columns = [c.name for c in result.columns]
    tags = [c.type_tag for c in result.columns]
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
    return columns, rows


def _copy_options(stmt: exp.Copy) -> tuple[str, str | None, str | None, bool]:
    """Parse ``FORMAT`` / ``CSV`` / ``DELIMITER`` / ``NULL`` / ``HEADER`` from a
    COPY statement's parameter list."""
    fmt, delimiter, null, header = "text", None, None, False
    for p in stmt.args.get("params") or []:
        key = str(getattr(p.this, "name", p.this)).upper()
        val = p.args.get("expression")
        val_text = (
            val.this if isinstance(val, exp.Literal) else (val.name if val is not None else "")
        )
        if key == "FORMAT":
            fmt = str(val_text).lower()
        elif key == "CSV":
            fmt = "csv"
            # The legacy ``WITH CSV HEADER`` bundles HEADER as the CSV param's
            # expression rather than a separate parameter.
            if str(val_text).upper() == "HEADER":
                header = True
        elif key == "DELIMITER":
            delimiter = str(val_text)
        elif key == "NULL":
            null = str(val_text)
        elif key == "HEADER":
            header = str(val_text).lower() not in ("false", "off", "0")
    return fmt, delimiter, null, header


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
            if col is not None and col.type_tag == "bool":
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
            value = get_path(doc, field)
            if value is None:
                cells.append(None)
            else:
                rendered = typemap.to_pg_text(value, tag)
                cells.append(rendered.decode() if rendered is not None else None)
        out.append(cells)
    return out


def _run_statement(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    if isinstance(stmt, exp.Create):
        kind = (stmt.args.get("kind") or "TABLE").upper()
        if kind == "TABLE":
            return executor.execute_create_table(
                planner.plan_create_table(stmt), catalog, storage, db
            )
        if kind == "INDEX":
            index = stmt.this
            tname = index.args["table"].name
            table = catalog.get(db, tname) or reflect.reflect(storage, db, tname)
            if table is None:
                raise errors.undefined_table(tname)
            return executor.execute_create_index(
                planner.plan_create_index(stmt, table), catalog, storage, db
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
            return _create_function(stmt, db, catalog)
        raise errors.feature_not_supported(f"CREATE {kind} is not supported")

    if isinstance(stmt, exp.Drop):
        kind = (stmt.args.get("kind") or "TABLE").upper()
        if kind == "TABLE":
            return executor.execute_drop_table(planner.plan_drop_table(stmt), catalog, storage, db)
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

    check_pred = None
    if isinstance(stmt, (exp.Insert, exp.Update, exp.Delete)):
        # DML through an (automatically-updatable) view rewrites onto its base
        # table before planning (#146). ``check_pred`` is the view's WITH CHECK
        # OPTION predicate, enforced against each written row.
        stmt, check_pred = _rewrite_write_through_view(stmt, catalog, db)

    if isinstance(stmt, exp.Insert):
        return _run_insert(stmt, storage, db, catalog, session, check_option=check_pred)

    if isinstance(stmt, exp.Update):
        if stmt.args.get("from_") is not None:
            return _run_update_from(stmt, storage, db, catalog, session)
        table = _require_table(catalog, db, stmt.find(exp.Table).name, storage)
        plan = planner.plan_update(stmt, table)
        plan.check_option = check_pred
        return executor.execute_update(plan, storage, db, catalog, session)

    if isinstance(stmt, exp.Delete):
        if stmt.args.get("using"):
            return _run_delete_using(stmt, storage, db, catalog, session)
        table = _require_table(catalog, db, stmt.find(exp.Table).name, storage)
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
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("TYPE"):
            return _alter_type_command(stmt, db, catalog)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("DOMAIN"):
            return _alter_domain_command(stmt, storage, db, catalog)
        if verb == "CREATE" and _command_text(stmt).lstrip().upper().startswith("DOMAIN"):
            return _create_domain_command(stmt, db, catalog)
        if verb == "DROP" and _command_text(stmt).lstrip().upper().startswith("DOMAIN"):
            return _drop_domain_command(stmt, db, catalog)
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
        return _run_command(stmt, session)

    if isinstance(stmt, exp.Grant):
        return _run_grant(stmt, storage, db, catalog, revoke=False)

    if isinstance(stmt, exp.Revoke):
        return _run_grant(stmt, storage, db, catalog, revoke=True)

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

    raise errors.feature_not_supported(f"unsupported statement: {type(stmt).__name__}")


_NOOP_WORDS = {"DISCARD"}


def _noop_command_word(stmt: exp.Expression) -> str | None:
    """Return the no-op command word (DISCARD) or None."""
    head = stmt.this if isinstance(stmt, exp.Alias) else stmt
    name = head.name if isinstance(head, exp.Column) else None
    if name is not None and name.upper() in _NOOP_WORDS:
        return name.upper()
    return None


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
    if target.upper() == "ALL":
        session.prepared.clear()
    else:
        session.prepared.pop(_unquote_ident(target), None)
    return SQLResult(command_tag="DEALLOCATE")


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


def _run_select(
    stmt: exp.Select, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    _validate_locks(stmt)  # FOR UPDATE / SHARE: single-node no-op, but OF-targets validated.
    # A base-less set-returning function as the row source: ``FROM generate_series(…)``
    # / ``FROM unnest(…)`` / … or a bare ``SELECT generate_series(…)``.
    srf_source = srf.from_source(stmt) or srf.fromless_projection(stmt)
    if srf_source is not None:
        return _run_srf_select(srf_source, stmt, storage, db, catalog, session)

    table_node = stmt.find(exp.Table)
    if table_node is None:
        return executor.execute_constant_select(
            planner.plan_constant_select(stmt, session, storage, catalog, db)
        )

    # A WITH NO DATA materialized view is not scannable until its first REFRESH.
    if (
        not table_node.args.get("db")
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
        plan = planner.plan_pipeline_select(stmt, db, catalog, storage)
        sctx = scalar.ScalarContext(storage=backend, catalog=catalog, db=db, session=session)
        if isinstance(plan, planner.EvaluatedSelectPlan):
            return executor.execute_evaluated_select(plan, backend, db, sctx)
        return executor.execute_pipeline_select(plan, backend, db, sctx)

    schema = table_node.args.get("db")
    schema_name = schema.name if schema is not None else None
    vtable = virtual.lookup(schema_name, table_node.name)
    if vtable is not None:
        rows = vtable.builder(db, session, storage, catalog)
        plan = planner.plan_select(stmt, vtable.table_def())
        return executor.execute_select(plan, virtual.MemoryBackend(rows), db)

    # A declared table, else a reflected (schema-on-read) view of an existing
    # Mongo collection — the dual-protocol read path.
    table = catalog.get(db, table_node.name) or reflect.reflect(storage, db, table_node.name)
    if table is None:
        raise errors.undefined_table(table_node.name)
    # A WHERE with EXISTS or a correlated subquery can't lower to a pushdown
    # filter — evaluate it per row (the inner query reads through the same
    # storage view, with outer-row references resolved by the scalar evaluator).
    if planner.where_needs_per_row(stmt, table, catalog, db):
        plan = planner.plan_correlated_select(stmt, table)
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
    rows, tdef = srf.build(source, sctx)
    query = stmt
    if stmt.args.get("from_") is None:
        # ``SELECT generate_series(…)`` — retarget the projection at the value
        # column so the standard column-projection path handles it.
        query = stmt.copy()
        query.set("from", exp.From(this=exp.Table(this=exp.to_identifier(tdef.name))))
        query.set("expressions", [exp.column(tdef.columns[0].name)])
    return executor.execute_select(
        planner.plan_select(query, tdef), virtual.MemoryBackend(rows), db
    )


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
    name = target.this.name if isinstance(target, exp.Schema) else target.name
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
        plan = planner.plan_insert(stmt, table)
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
    """The grantee role names of a ``GRANT``/``REVOKE`` (``PUBLIC`` kept as-is)."""
    out: list[str] = []
    for gp in stmt.args.get("principals") or []:
        ident = gp.this if isinstance(gp, exp.GrantPrincipal) else gp
        out.append(str(getattr(ident, "name", ident)))
    return out


def _run_grant(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, *, revoke: bool
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
        name = t.name
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
            if val is not None:
                return int(val.this if isinstance(val, exp.Literal) else val)
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
    name = stmt.this.name
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
    name = stmt.this.name
    if not catalog.drop_sequence(db, name) and not stmt.args.get("exists"):
        raise errors.SQLError("42P01", f'sequence "{name}" does not exist')
    return SQLResult(command_tag="DROP SEQUENCE")


def _create_type(stmt: exp.Create, db: str, catalog: Catalog) -> SQLResult:
    """``CREATE TYPE name AS ENUM ('a', 'b', …)`` records the enum's label list;
    ``CREATE TYPE name AS (field type, …)`` records a composite type's ordered
    fields. Range / base types are not supported."""
    name = stmt.this.name
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


def _drop_type(stmt: exp.Drop, db: str, catalog: Catalog) -> SQLResult:
    name = stmt.this.name
    dropped = catalog.drop_enum(db, name) or catalog.drop_composite(db, name)
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


def _function_param_types(udf: exp.Expression) -> list[str | None]:
    """Parameter type tags of a ``CREATE FUNCTION`` signature (positional), for
    ``pg_proc`` / ``information_schema.parameters`` reflection. Unknown → None."""
    types: list[str | None] = []
    for p in udf.expressions or []:
        dt = p.args.get("kind") if isinstance(p, exp.ColumnDef) else p
        types.append(typemap.type_tag_for_sql(dt) if isinstance(dt, exp.DataType) else None)
    return types


def _create_function(stmt: exp.Create, db: str, catalog: Catalog) -> SQLResult:
    """``CREATE [OR REPLACE] FUNCTION name(params) RETURNS t AS $$ body $$
    LANGUAGE sql`` — store the parsed body for the scalar evaluator to invoke."""
    udf = stmt.this
    name = udf.this.name
    params = _function_params(udf)
    nargs = len(params)

    language = "sql"
    return_tag = None
    is_table = False
    for prop in stmt.args.get("properties").expressions if stmt.args.get("properties") else []:
        if isinstance(prop, exp.LanguageProperty):
            language = str(prop.this.name if hasattr(prop.this, "name") else prop.this).lower()
        elif isinstance(prop, exp.ReturnsProperty):
            is_table = bool(prop.args.get("is_table"))
            if isinstance(prop.this, exp.DataType):
                return_tag = typemap.type_tag_for_sql(prop.this)

    if language not in ("sql",):
        raise errors.feature_not_supported(
            f"CREATE FUNCTION LANGUAGE {language} is not supported (only LANGUAGE sql)"
        )

    body = _function_body_text(stmt.expression).strip()
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
        },
    )
    return SQLResult(command_tag="CREATE FUNCTION")


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


def _alter_sequence_command(stmt: exp.Command, db: str, catalog: Catalog) -> SQLResult:
    """``ALTER SEQUENCE [IF EXISTS] name { RESTART [WITH n] | INCREMENT BY n |
    MINVALUE n | MAXVALUE n | START WITH n | [NO] CYCLE }…``. Arrives as a
    Command (sqlglot doesn't model the grammar)."""
    m = _ALTER_SEQUENCE_RE.match(_command_text(stmt))
    if m is None:
        raise errors.feature_not_supported(f"unsupported ALTER SEQUENCE: {stmt.sql()}")
    name = m.group(1).strip('"')
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
        if not isinstance(src, exp.Table) or src.args.get("db"):
            continue
        if src.name in cte_names:
            continue
        vdef = catalog.get_view(db, src.name)
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
    is_write = isinstance(stmt, (exp.Insert, exp.Update, exp.Delete))
    if not isinstance(stmt, (exp.Select, exp.SetOperation)) and not is_write:
        raise errors.feature_not_supported(
            "WITH is supported only with SELECT / set-operation / INSERT / UPDATE / DELETE"
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


def _run_set(stmt: exp.Set, session: Session) -> SQLResult:
    reported: list[tuple[str, str]] = []
    for item in stmt.expressions:
        # SET TRANSACTION [ISOLATION LEVEL ...|READ ONLY|READ WRITE|DEFERRABLE]:
        # accepted as a no-op — SecantusDB is single-node, so isolation/read-only
        # characteristics don't change behaviour.
        if (
            isinstance(item, exp.SetItem)
            and str(item.args.get("kind") or "").upper() == "TRANSACTION"
        ):
            continue
        inner = item.this if isinstance(item, exp.SetItem) else item
        if not isinstance(inner, exp.EQ):
            raise errors.feature_not_supported(f"unsupported SET item: {item.sql()}")
        name = inner.this.name
        value_node = inner.expression
        if isinstance(value_node, exp.Literal):
            value = value_node.this
        else:
            value = value_node.name or value_node.sql()
        # SET LOCAL applies only until the end of the current transaction. Outside
        # a transaction block it has no lasting effect (Postgres warns and drops it).
        is_local = isinstance(item, exp.SetItem) and str(item.args.get("kind") or "").upper() == (
            "LOCAL"
        )
        if is_local:
            if session.txn_handle is not None:
                session.set_local(name, str(value))
            else:
                continue  # SET LOCAL outside a transaction — no lasting effect
        else:
            session.settings[name] = str(value)
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


def _run_authorization_command(verb: str, tail: str, session: Session) -> SQLResult | None:
    """``SET ROLE`` / ``SET SESSION AUTHORIZATION`` and their ``RESET`` forms, which
    change the session's current role / session user (#128). Returns None if the
    command isn't one of these (the caller handles the generic SET/RESET)."""
    if verb == "RESET":
        key = tail.strip().upper()
        if key == "ROLE":
            session.role = None
            session.settings.pop("role", None)
            return SQLResult(command_tag="RESET")
        if key == "SESSION AUTHORIZATION":
            session.user = session.login_user or session.user
            session.role = None
            session.settings.pop("role", None)
            return SQLResult(command_tag="RESET")
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
        return SQLResult(command_tag="SET")
    return None


def _run_command(stmt: exp.Command, session: Session) -> SQLResult:
    verb = str(stmt.this).upper()
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
        # SET SESSION CHARACTERISTICS AS TRANSACTION ... falls back to a Command;
        # accepted as a no-op (single-node — no isolation/read-only semantics).
        return SQLResult(command_tag="SET")
    raise errors.feature_not_supported(f"command {verb} is not supported")
