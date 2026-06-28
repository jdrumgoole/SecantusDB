"""``run_sql`` — the embedded SQL entry point.

Parses a SQL string, plans each statement, and executes it against a
``Storage`` instance, returning one ``SQLResult`` per statement. This is the P0
deliverable: a no-wire, in-process SQL engine over the document store. The
PostgreSQL-wire server (a later phase) will drive these same planner/executor
functions; ``run_sql`` is both the test seam and the proof that the
SQL-to-Mongo-engines translation holds.
"""

from __future__ import annotations

from typing import Any

from sqlglot import exp

from secantus.sql import errors, executor, planner
from secantus.sql.catalog import Catalog
from secantus.sql.result import SQLResult


def run_sql(storage: Any, db: str, sql: str) -> list[SQLResult]:
    """Execute ``sql`` against ``db`` on ``storage``; one result per statement.

    ``storage`` is any object exposing the ``Storage`` data API
    (``insert`` / ``find_matching`` / ``update_matching`` / ``delete_matching`` /
    ``create_collection`` / ``drop_collection``) — the real WiredTiger-backed
    ``Storage`` in production, or an in-memory double in tests.
    """
    catalog = Catalog(storage)
    results: list[SQLResult] = []
    for stmt in planner.parse(sql):
        results.append(_run_statement(stmt, storage, db, catalog))
    return results


def _require_table(catalog: Catalog, db: str, name: str) -> Any:
    table = catalog.get(db, name)
    if table is None:
        raise errors.undefined_table(name)
    return table


def _run_statement(
    stmt: exp.Expression, storage: Any, db: str, catalog: Catalog
) -> SQLResult:
    if isinstance(stmt, exp.Create):
        if (stmt.args.get("kind") or "TABLE").upper() != "TABLE":
            raise errors.feature_not_supported(f"CREATE {stmt.args.get('kind')} is not supported")
        return executor.execute_create_table(planner.plan_create_table(stmt), catalog, storage, db)

    if isinstance(stmt, exp.Drop):
        if (stmt.args.get("kind") or "TABLE").upper() != "TABLE":
            raise errors.feature_not_supported(f"DROP {stmt.args.get('kind')} is not supported")
        return executor.execute_drop_table(planner.plan_drop_table(stmt), catalog, storage, db)

    if isinstance(stmt, exp.Insert):
        table = _require_table(catalog, db, stmt.find(exp.Table).name)
        return executor.execute_insert(planner.plan_insert(stmt, table), storage, db)

    if isinstance(stmt, exp.Select):
        table = _require_table(catalog, db, stmt.find(exp.Table).name)
        return executor.execute_select(planner.plan_select(stmt, table), storage, db)

    if isinstance(stmt, exp.Update):
        table = _require_table(catalog, db, stmt.find(exp.Table).name)
        return executor.execute_update(planner.plan_update(stmt, table), storage, db)

    if isinstance(stmt, exp.Delete):
        table = _require_table(catalog, db, stmt.find(exp.Table).name)
        return executor.execute_delete(planner.plan_delete(stmt, table), storage, db)

    raise errors.feature_not_supported(f"unsupported statement: {type(stmt).__name__}")
