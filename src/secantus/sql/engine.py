"""``run_sql`` — the embedded SQL entry point.

Parses a SQL string, plans each statement, and executes it against a
``Storage`` instance, returning one ``SQLResult`` per statement. This is both
the embedded API and what the PostgreSQL-wire server drives. A per-connection
``Session`` carries the database, user, and GUC settings so session functions
and ``SHOW`` / ``SET`` resolve against real state.
"""

from __future__ import annotations

import copy
import re
from dataclasses import dataclass
from typing import Any

import sqlglot
from sqlglot import exp

from secantus.sql import errors, executor, planner, reflect, scalar, typemap, virtual
from secantus.sql.catalog import Catalog, Column, TableDef
from secantus.sql.result import ColumnDesc, SQLResult
from secantus.sql.session import REPORTABLE_GUCS, Session, _Cursor, _Savepoint


def run_sql(storage: Any, db: str, sql: str, *, session: Session | None = None) -> list[SQLResult]:
    """Execute ``sql`` against ``db`` on ``storage``; one result per statement.

    ``storage`` is any object exposing the ``Storage`` data API. ``session`` is
    the per-connection state (created fresh if omitted, e.g. for the embedded
    API); the wire server passes a long-lived one so ``SET`` persists.
    """
    if session is None:
        session = Session(database=db)
    catalog = Catalog(storage)
    results: list[SQLResult] = []
    for stmt in planner.parse(sql):
        results.append(_dispatch(stmt, storage, db, catalog, session))
    return results


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
    return SQLResult(command_tag="COMMIT")


def _end_txn_state(session: Session) -> None:
    """Clear all per-transaction session state at the end of a block."""
    session.txn_handle = None
    session.txn_failed = False
    session.savepoints = []
    session.reset_deferred()
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
    hold = re.search(r"\bWITH\s+HOLD\b", opts, re.IGNORECASE) is not None
    stmts = planner.parse(m.group("query"))
    if len(stmts) != 1:
        raise errors.syntax_error("DECLARE CURSOR expects a single query")
    result = _run_query(stmts[0], storage, db, catalog, session)
    session.cursors[name] = _Cursor(
        name=name, columns=result.columns, rows=list(result.rows), pos=-1, hold=hold
    )
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
    affected_docs: list[dict[str, Any]] = []  # post/pre-images for RETURNING
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

    def record(count: int, doc: dict[str, Any] | None) -> None:
        nonlocal affected
        affected += count
        if returning is not None and doc is not None:
            affected_docs.append(doc)

    for srow in source_rows:
        matched = [
            td
            for td in target_docs
            if id(td) not in done and scalar._truthy(scalar.evaluate(on, scope_for(td, srow), sctx))
        ]
        for td in matched:
            source_matched.add(id(td))
        if matched:
            when = _merge_pick_when(whens, True, False, scope_for(matched[0], srow), sctx)
            if when is None:
                continue
            for td in matched:
                record(
                    *_merge_apply_matched(when, target, td, storage, db, scope_for(td, srow), sctx)
                )
                done.add(id(td))
        else:
            when = _merge_pick_when(whens, False, False, scope_for(None, srow), sctx)
            if when is not None:
                record(
                    *_merge_apply_not_matched(
                        when, target, storage, db, scope_for(None, srow), sctx
                    )
                )

    # WHEN NOT MATCHED BY SOURCE — target rows no source row matched.
    if any(not w.args.get("matched") and w.args.get("source") for w in whens):
        for td in target_docs:
            if id(td) in source_matched or id(td) in done:
                continue
            when = _merge_pick_when(whens, False, True, scope_for(td, None), sctx)
            if when is not None:
                record(
                    *_merge_apply_matched(when, target, td, storage, db, scope_for(td, None), sctx)
                )
                done.add(id(td))

    if returning is not None:
        return executor._returning_result(
            affected_docs, returning, f"MERGE {affected}", affected, target, storage, db
        )
    return SQLResult(command_tag=f"MERGE {affected}", rowcount=affected)


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
        set_doc: dict[str, Any] = {}
        for eq in then.expressions:
            col = eq.this.name
            set_doc[target.field_for(col)] = typemap.coerce(
                scalar.evaluate(eq.expression, scope, sctx), target.type_for(col)
            )
        if set_doc:
            post = {**td, **set_doc}
            executor.enforce_update_images(
                [post], [td["_id"]], target, storage, db, sctx.catalog, sctx.session
            )
            storage.update_matching(db, target.collection, {"_id": td["_id"]}, {"$set": set_doc})
        return 1, {**td, **set_doc}
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
    doc: dict[str, Any] = {}
    for col, vexpr in zip(cols, values, strict=True):
        doc[target.field_for(col)] = typemap.coerce(
            scalar.evaluate(vexpr, scope, sctx), target.type_for(col)
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
    return _dispatch(stmt, storage, db, catalog, session)


def describe_statement(
    storage: Any, db: str, stmt: exp.Expression, session: Session, catalog: Catalog
) -> list[ColumnDesc] | None:
    """Resolve a statement's result columns WITHOUT executing it.

    Used by the extended protocol's Describe: planning is side-effect-free, so
    this never reads or writes storage (important — Describe must not run an
    INSERT/UPDATE/DELETE). Returns the column descriptors for a row-returning
    statement (SELECT / SHOW), or None for everything else (→ NoData).
    """
    if isinstance(stmt, exp.Command) and str(stmt.this).upper() == "SHOW":
        return [ColumnDesc(_show_name(stmt), "text", typemap.PG_OID["text"])]
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
        return describe_statement(storage, db, arm, session, catalog)
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
    columns: list[str]  # target column SQL names
    to_stdout: bool  # True = COPY TO STDOUT, False = COPY FROM STDIN
    fmt: str  # "text" | "csv"
    delimiter: str
    null: str
    header: bool


def copy_plan(stmt: exp.Copy, storage: Any, db: str, catalog: Catalog) -> CopyPlan:
    """Resolve a ``COPY`` statement to a :class:`CopyPlan`. Only ``STDIN`` /
    ``STDOUT`` are supported (no server-side file access)."""
    files = stmt.args.get("files") or []
    target = files[0].name.upper() if files else ""
    if target not in ("STDIN", "STDOUT"):
        raise errors.feature_not_supported("COPY only supports STDIN / STDOUT")
    to_stdout = not bool(stmt.args.get("kind"))  # kind True = FROM, False = TO
    this = stmt.this
    if isinstance(this, exp.Schema):
        tname = this.this.name
        columns: list[str] | None = [c.name for c in this.expressions]
    else:
        tname = this.name
        columns = None
    table = catalog.get(db, tname) or reflect.reflect(storage, db, tname)
    if table is None:
        raise errors.undefined_table(tname)
    fmt, delimiter, null, header = _copy_options(stmt)
    if columns is None:
        cols = list(table.columns)
        if not to_stdout:  # a generated column can't be copied in
            cols = [c for c in cols if c.generated is None and c.identity != "always"]
        columns = [c.name for c in cols]
    if delimiter is None:
        delimiter = "," if fmt == "csv" else "\t"
    if null is None:
        null = "" if fmt == "csv" else "\\N"
    return CopyPlan(table, columns, to_stdout, fmt, delimiter, null, header)


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
        raise errors.feature_not_supported(f"DROP {kind} is not supported")

    if isinstance(stmt, exp.Alter):
        return executor.execute_alter_table(planner.plan_alter_table(stmt), catalog, storage, db)

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

    if isinstance(stmt, exp.SetOperation):
        return _run_set_operation(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Select):
        return _run_select(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Insert):
        return _run_insert(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Update):
        table = _require_table(catalog, db, stmt.find(exp.Table).name, storage)
        return executor.execute_update(
            planner.plan_update(stmt, table), storage, db, catalog, session
        )

    if isinstance(stmt, exp.Delete):
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
        if verb == "REFRESH":
            return _refresh_matview(stmt, storage, db, catalog, session)
        if verb == "CREATE" and _command_text(stmt).lstrip().upper().startswith("MATERIALIZED"):
            return _create_matview_command(stmt, storage, db, catalog, session)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("MATERIALIZED"):
            return _alter_matview_command(stmt, storage, db, catalog, session)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("SEQUENCE"):
            return _alter_sequence_command(stmt, db, catalog)
        if verb == "ALTER" and _command_text(stmt).lstrip().upper().startswith("TYPE"):
            return _alter_type_command(stmt, db, catalog)
        if verb == "SET" and _command_text(stmt).lstrip().upper().startswith("CONSTRAINTS"):
            return _set_constraints_command(stmt, storage, db, catalog, session)
        if verb in ("CREATE", "DROP", "ALTER") and _command_text(stmt).lstrip().upper().startswith(
            ("ROLE ", "USER ", "GROUP ")
        ):
            return _run_role_command(verb, stmt, db, catalog)
        if verb in ("GRANT", "REVOKE"):
            # Role-membership / privilege grants aren't enforced — accept no-op.
            return SQLResult(command_tag=verb)
        return _run_command(stmt, session)

    if isinstance(stmt, exp.Grant):
        # Privileges aren't enforced (single-node dev surface) — accept as a no-op.
        return SQLResult(command_tag="GRANT")

    if isinstance(stmt, exp.Revoke):
        return SQLResult(command_tag="REVOKE")

    # CLOSE cursor / CLOSE ALL parses as a bare Alias (``CLOSE AS name``).
    close = _close_cursor_target(stmt)
    if close is not None:
        return _close_cursor(close, session)

    # DEALLOCATE / DISCARD parse as a bare Alias in sqlglot's pg dialect (e.g.
    # ``DEALLOCATE "x"`` → ``DEALLOCATE AS "x"``); libpq clients (psycopg) and
    # SQLAlchemy emit them to manage prepared statements. Accept as a no-op — our
    # prepared statements live for the connection. (SAVEPOINT / RELEASE are real
    # commands, handled in ``_dispatch``.)
    noop = _noop_command_word(stmt)
    if noop is not None:
        return SQLResult(command_tag=noop)

    raise errors.feature_not_supported(f"unsupported statement: {type(stmt).__name__}")


_NOOP_WORDS = {"DEALLOCATE", "DISCARD"}


def _noop_command_word(stmt: exp.Expression) -> str | None:
    """Return the no-op command word (DEALLOCATE / DISCARD) or None."""
    head = stmt.this if isinstance(stmt, exp.Alias) else stmt
    name = head.name if isinstance(head, exp.Column) else None
    if name is not None and name.upper() in _NOOP_WORDS:
        return name.upper()
    return None


def _run_select(
    stmt: exp.Select, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
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
    if planner.where_needs_per_row(stmt):
        plan = planner.plan_correlated_select(stmt, table)
        return executor.execute_correlated_select(plan, storage, db, catalog, session)
    # A non-correlated WHERE subquery (`x IN (SELECT ...)`, `x = (SELECT ...)`) is
    # pre-evaluated by the planner, which runs the inner SELECT through the engine.
    subctx = planner.SubqueryCtx(storage=storage, db=db, catalog=catalog, session=session)
    return executor.execute_select(planner.plan_select(stmt, table, subctx), storage, db)


def _run_insert(
    stmt: exp.Insert, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Dispatch an INSERT: ``VALUES`` plans directly; ``INSERT … SELECT`` runs the
    source query first (it may join / aggregate / be a set operation), then maps
    its result rows positionally onto the target columns."""
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
        return executor.execute_insert(plan, storage, db, catalog, session)
    return executor.execute_insert(planner.plan_insert(stmt, table), storage, db, catalog, session)


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
    text = arg.this if isinstance(arg, exp.Literal) else str(arg)
    m = _MATVIEW_NAME_RE.match(str(text))
    if m is None:
        raise errors.feature_not_supported(f"unsupported REFRESH: {stmt.sql()}")
    name = m.group(1).strip().strip('"')
    definition = catalog.get_matview(db, name)
    if definition is None:
        raise errors.SQLError("42P01", f'materialized view "{name}" does not exist')
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
    """``CREATE TYPE name AS ENUM ('a', 'b', …)`` — record the enum's label list.
    Only the ENUM form is supported (composite / range / base types are not)."""
    name = stmt.this.name
    dt = stmt.args.get("expression")
    if not (isinstance(dt, exp.DataType) and dt.this and dt.this.name == "ENUM"):
        raise errors.feature_not_supported(
            "only CREATE TYPE … AS ENUM is supported (composite/range types are not)"
        )
    if catalog.enum_exists(db, name):
        raise errors.SQLError("42710", f'type "{name}" already exists')
    labels = [e.this if isinstance(e, exp.Literal) else str(e) for e in dt.expressions]
    catalog.create_enum(db, name, labels)
    return SQLResult(command_tag="CREATE TYPE")


def _drop_type(stmt: exp.Drop, db: str, catalog: Catalog) -> SQLResult:
    name = stmt.this.name
    if not catalog.drop_enum(db, name) and not stmt.args.get("exists"):
        raise errors.SQLError("42704", f'type "{name}" does not exist')
    return SQLResult(command_tag="DROP TYPE")


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
        else:
            result = _run_query(cte.this, backend, db, cte_catalog, session)
        _register_cte(backend, cte_defs, name, result.columns, result.rows, col_aliases)

    with_node.pop()  # detach the WITH so the body plans as a plain statement
    if is_write:
        # A write body reads CTEs through the backend (INSERT … SELECT FROM cte)
        # or a WHERE subquery over one. Publish the CTE-aware context so an UPDATE
        # / DELETE WHERE subquery resolves the CTE, and dispatch the write against
        # the backend (its writes forward to real storage) + overlay catalog.
        token = planner._pipeline_subctx.set(
            planner.SubqueryCtx(storage=backend, db=db, catalog=cte_catalog, session=session)
        )
        try:
            return _run_statement(stmt, backend, db, cte_catalog, session)
        finally:
            planner._pipeline_subctx.reset(token)
    return _run_query(stmt, backend, db, cte_catalog, session)


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
        session.settings[name] = str(value)
        if name in REPORTABLE_GUCS:
            reported.append((name, str(value)))
    return SQLResult(command_tag="SET", parameter_status=reported)


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
    if verb == "SHOW":
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
