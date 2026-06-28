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

from secantus.sql import errors, executor, planner, reflect, typemap, virtual
from secantus.sql.catalog import Catalog
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
    if not isinstance(stmt, exp.Select):
        return None
    table_node = stmt.find(exp.Table)
    if table_node is None:
        plan = planner.plan_constant_select(stmt, session)
        return [ColumnDesc(n, t, typemap.PG_OID.get(t, 25)) for n, t, _ in plan.columns]
    if planner.select_needs_pipeline(stmt):
        pplan = planner.plan_pipeline_select(stmt, db, catalog)
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


def _require_table(catalog: Catalog, db: str, name: str) -> Any:
    table = catalog.get(db, name)
    if table is None:
        raise errors.undefined_table(name)
    return table


def _run_statement(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    if isinstance(stmt, exp.Create):
        if (stmt.args.get("kind") or "TABLE").upper() != "TABLE":
            raise errors.feature_not_supported(f"CREATE {stmt.args.get('kind')} is not supported")
        return executor.execute_create_table(planner.plan_create_table(stmt), catalog, storage, db)

    if isinstance(stmt, exp.Drop):
        if (stmt.args.get("kind") or "TABLE").upper() != "TABLE":
            raise errors.feature_not_supported(f"DROP {stmt.args.get('kind')} is not supported")
        return executor.execute_drop_table(planner.plan_drop_table(stmt), catalog, storage, db)

    if isinstance(stmt, exp.Select):
        return _run_select(stmt, storage, db, catalog, session)

    if isinstance(stmt, exp.Insert):
        table = _require_table(catalog, db, stmt.find(exp.Table).name)
        return executor.execute_insert(planner.plan_insert(stmt, table), storage, db)

    if isinstance(stmt, exp.Update):
        table = _require_table(catalog, db, stmt.find(exp.Table).name)
        return executor.execute_update(planner.plan_update(stmt, table), storage, db)

    if isinstance(stmt, exp.Delete):
        table = _require_table(catalog, db, stmt.find(exp.Table).name)
        return executor.execute_delete(planner.plan_delete(stmt, table), storage, db)

    if isinstance(stmt, exp.Set):
        return _run_set(stmt, session)

    if isinstance(stmt, exp.Command):
        return _run_command(stmt, session)

    raise errors.feature_not_supported(f"unsupported statement: {type(stmt).__name__}")


def _run_select(
    stmt: exp.Select, storage: Any, db: str, catalog: Catalog, session: Session
) -> SQLResult:
    table_node = stmt.find(exp.Table)
    if table_node is None:
        return executor.execute_constant_select(planner.plan_constant_select(stmt, session))

    schema = table_node.args.get("db")
    schema_name = schema.name if schema is not None else None
    vtable = virtual.lookup(schema_name, table_node.name)
    if vtable is not None:
        rows = vtable.builder(db, session, storage, catalog)
        plan = planner.plan_select(stmt, vtable.table_def())
        return executor.execute_select(plan, virtual.MemoryBackend(rows), db)

    # JOIN / GROUP BY / aggregates compile to an aggregation pipeline.
    if planner.select_needs_pipeline(stmt):
        return executor.execute_pipeline_select(
            planner.plan_pipeline_select(stmt, db, catalog), storage, db
        )

    if schema_name in ("information_schema", "pg_catalog"):
        # A catalog query we don't model yet (typically a \d-style join) —
        # faithful "not supported" rather than a wrong answer.
        raise errors.feature_not_supported(
            f"catalog relation {schema_name}.{table_node.name} is not supported yet"
        )

    # A declared table, else a reflected (schema-on-read) view of an existing
    # Mongo collection — the dual-protocol read path.
    table = catalog.get(db, table_node.name) or reflect.reflect(storage, db, table_node.name)
    if table is None:
        raise errors.undefined_table(table_node.name)
    return executor.execute_select(planner.plan_select(stmt, table), storage, db)


def _run_set(stmt: exp.Set, session: Session) -> SQLResult:
    reported: list[tuple[str, str]] = []
    for item in stmt.expressions:
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
    name = arg.this if isinstance(arg, exp.Literal) else (arg.name if arg is not None else "")
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
    raise errors.feature_not_supported(f"command {verb} is not supported")
