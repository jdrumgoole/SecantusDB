"""``run_sql`` — the embedded SQL entry point.

Parses a SQL string, plans each statement, and executes it against a
``Storage`` instance, returning one ``SQLResult`` per statement. This is both
the embedded API and what the PostgreSQL-wire server drives. A per-connection
``Session`` carries the database, user, and GUC settings so session functions
and ``SHOW`` / ``SET`` resolve against real state.
"""

from __future__ import annotations

from typing import Any

from sqlglot import exp

from secantus.sql import errors, executor, planner, reflect, scalar, typemap, virtual
from secantus.sql.catalog import Catalog, Column, TableDef
from secantus.sql.result import ColumnDesc, SQLResult
from secantus.sql.session import REPORTABLE_GUCS, Session


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
        return _commit_txn(storage, session)
    if isinstance(stmt, exp.Rollback):
        if stmt.args.get("savepoint") is not None:
            # ROLLBACK TO SAVEPOINT recovers the block from an error and keeps it
            # open. We don't track per-savepoint state, so un-poison and continue.
            session.txn_failed = False
            return SQLResult(command_tag="ROLLBACK")
        return _rollback_txn(storage, session)

    if session.txn_failed:
        raise errors.SQLError(
            "25P02",
            "current transaction is aborted, commands ignored until end of transaction block",
        )

    if session.txn_handle is not None:
        try:
            with storage.use_user_transaction(session.txn_handle):
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
    return SQLResult(command_tag="BEGIN")


def _commit_txn(storage: Any, session: Session) -> SQLResult:
    handle, failed = session.txn_handle, session.txn_failed
    session.txn_handle = None
    session.txn_failed = False
    if handle is None:
        return SQLResult(command_tag="COMMIT")  # no open block — Postgres warns, returns COMMIT
    if failed:
        # COMMIT of an aborted block actually rolls back (and tags ROLLBACK).
        storage.abort_user_transaction(handle)
        return SQLResult(command_tag="ROLLBACK")
    storage.commit_user_transaction(handle)
    return SQLResult(command_tag="COMMIT")


def _rollback_txn(storage: Any, session: Session) -> SQLResult:
    handle = session.txn_handle
    session.txn_handle = None
    session.txn_failed = False
    if handle is not None:
        storage.abort_user_transaction(handle)
    return SQLResult(command_tag="ROLLBACK")


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
        raise errors.feature_not_supported(f"CREATE {kind} is not supported")

    if isinstance(stmt, exp.Drop):
        kind = (stmt.args.get("kind") or "TABLE").upper()
        if kind == "TABLE":
            return executor.execute_drop_table(planner.plan_drop_table(stmt), catalog, storage, db)
        if kind == "INDEX":
            return executor.execute_drop_index(planner.plan_drop_index(stmt), catalog, storage, db)
        raise errors.feature_not_supported(f"DROP {kind} is not supported")

    if _own_with(stmt) is not None:
        return _run_with(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.SetOperation):
        return _run_set_operation(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Select):
        return _run_select(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Insert):
        table = _require_table(catalog, db, stmt.find(exp.Table).name, storage)
        return executor.execute_insert(planner.plan_insert(stmt, table), storage, db)

    if isinstance(stmt, exp.Update):
        table = _require_table(catalog, db, stmt.find(exp.Table).name, storage)
        return executor.execute_update(planner.plan_update(stmt, table), storage, db)

    if isinstance(stmt, exp.Delete):
        table = _require_table(catalog, db, stmt.find(exp.Table).name, storage)
        return executor.execute_delete(planner.plan_delete(stmt, table), storage, db)

    if isinstance(stmt, exp.Set):
        return _run_set(stmt, session)

    if isinstance(stmt, exp.Command):
        return _run_command(stmt, session)

    # DEALLOCATE / DISCARD / SAVEPOINT / RELEASE parse as a bare Alias in
    # sqlglot's pg dialect (e.g. ``SAVEPOINT "x"`` → ``SAVEPOINT AS "x"``); libpq
    # clients (psycopg) and SQLAlchemy emit them to manage prepared statements and
    # nested transaction savepoints. Accept as a no-op — our prepared statements
    # live for the connection, and ROLLBACK TO SAVEPOINT (handled above) is what
    # actually recovers an aborted block.
    noop = _noop_command_word(stmt)
    if noop is not None:
        return SQLResult(command_tag=noop)

    raise errors.feature_not_supported(f"unsupported statement: {type(stmt).__name__}")


_NOOP_WORDS = {"DEALLOCATE", "DISCARD", "SAVEPOINT", "RELEASE"}


def _noop_command_word(stmt: exp.Expression) -> str | None:
    """Return the no-op command word (DEALLOCATE/DISCARD/SAVEPOINT/RELEASE) or None."""
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
        return executor.execute_constant_select(planner.plan_constant_select(stmt, session))

    # JOIN / GROUP BY / aggregates compile to an aggregation pipeline. Route it
    # through a CatalogBackend so the pipeline can read pg_catalog /
    # information_schema relations (the joins interactive psql's \d emits) as
    # well as real collections.
    if planner.select_needs_pipeline(stmt):
        backend = virtual.CatalogBackend(storage, catalog, session, db)
        plan = planner.plan_pipeline_select(stmt, db, catalog, storage)
        if isinstance(plan, planner.EvaluatedSelectPlan):
            sctx = scalar.ScalarContext(storage=backend, catalog=catalog, db=db, session=session)
            return executor.execute_evaluated_select(plan, backend, db, sctx)
        return executor.execute_pipeline_select(plan, backend, db)

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


def _run_with(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    """Run a ``WITH name AS (...) [, ...] <query>`` statement (non-recursive CTEs).

    Each CTE is materialized to rows and registered as an ephemeral collection on
    a ``CatalogBackend``; a catalog overlay maps the CTE names to TableDefs built
    from each inner query's result shape. The WITH is then stripped and the main
    query runs against that backend + overlay, so CTE names resolve like tables
    in every path (single-table, pipeline/join, set operations). CTEs are
    materialized in order, so a later CTE may reference an earlier one."""
    with_node = _own_with(stmt)
    if with_node.args.get("recursive"):
        raise errors.feature_not_supported("WITH RECURSIVE is not supported")
    if not isinstance(stmt, (exp.Select, exp.SetOperation)):
        raise errors.feature_not_supported(
            "WITH is supported only with SELECT / set-operation queries"
        )

    backend = virtual.CatalogBackend(storage, catalog, session, db)
    cte_defs: dict[str, TableDef] = {}
    cte_catalog = _CTECatalog(catalog, cte_defs)
    for cte in with_node.expressions:
        name = cte.alias
        if not name:
            raise errors.feature_not_supported("a CTE must be named")
        result = _run_query(cte.this, backend, db, cte_catalog, session)
        col_names = [c.name for c in result.columns]
        backend.register_ephemeral(
            name, [dict(zip(col_names, row, strict=True)) for row in result.rows]
        )
        # reflected=True so any column resolves; the explicit columns carry the
        # inner query's names + types (and make `SELECT *` / an empty CTE work).
        cte_defs[name] = TableDef(
            name=name,
            collection=name,
            columns=[
                Column(c.name, c.type_tag, c.name, pk=False, nullable=True) for c in result.columns
            ],
            reflected=True,
        )

    with_node.pop()  # detach the WITH so the main query plans as a plain statement
    return _run_query(stmt, backend, db, cte_catalog, session)


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
        terms = [
            (_setop_order_index(o.this, columns), -1 if o.args.get("desc") else 1)
            for o in order.expressions
        ]
        # Stable multi-key sort: apply each key from least to most significant.
        for idx, direction in reversed(terms):
            rows = sorted(
                rows,
                key=lambda r, idx=idx: (r[idx] is None, r[idx]),
                reverse=(direction == -1),
            )
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
