"""Execute a plan against ``Storage`` and shape the result rows.

Each function takes a plan (already lowered to Mongo structures by the planner),
performs the corresponding ``Storage`` call, and returns a ``SQLResult`` with a
Postgres ``CommandComplete`` tag. This is the only layer that touches storage;
the planner stays pure translation.
"""

from __future__ import annotations

from typing import Any

from secantus.sql import errors, planner, typemap
from secantus.sql.catalog import Catalog
from secantus.sql.result import ColumnDesc, SQLResult


def execute_create_table(
    plan: planner.CreateTablePlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    if catalog.exists(db, plan.table.name):
        if plan.if_not_exists:
            return SQLResult(command_tag="CREATE TABLE")
        raise errors.duplicate_table(plan.table.name)
    catalog.put(db, plan.table)
    storage.create_collection(db, plan.table.collection)
    return SQLResult(command_tag="CREATE TABLE")


def execute_drop_table(
    plan: planner.DropTablePlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    table = catalog.get(db, plan.name)
    if table is None:
        if plan.if_exists:
            return SQLResult(command_tag="DROP TABLE")
        raise errors.undefined_table(plan.name)
    catalog.drop(db, plan.name)
    storage.drop_collection(db, table.collection)
    return SQLResult(command_tag="DROP TABLE")


def execute_insert(plan: planner.InsertPlan, storage: Any, db: str) -> SQLResult:
    inserted, write_errors = storage.insert(db, plan.table.collection, plan.docs)
    if write_errors:
        first = write_errors[0]
        if first.get("code") == 11000:
            raise errors.unique_violation(
                f'duplicate key value violates unique constraint on "{plan.table.name}"'
            )
        raise errors.SQLError("XX000", first.get("errmsg", "insert failed"))
    return SQLResult(command_tag=f"INSERT 0 {inserted}", rowcount=inserted)


def execute_select(plan: planner.SelectPlan, storage: Any, db: str) -> SQLResult:
    if plan.count_star:
        # COUNT(*) ignores LIMIT/OFFSET — count everything the filter matches.
        n = len(storage.find_matching(db, plan.table.collection, plan.filter))
        return SQLResult(
            command_tag="SELECT 1",
            columns=[ColumnDesc(plan.count_alias, "int8", typemap.PG_OID["int8"])],
            rows=[(n,)],
            rowcount=1,
        )

    docs = storage.find_matching(
        db,
        plan.table.collection,
        plan.filter,
        skip=plan.skip,
        limit=plan.limit,
        sort=plan.sort,
    )
    columns = [
        ColumnDesc(name, col.type_tag, typemap.PG_OID.get(col.type_tag, 25))
        for name, col in plan.out_columns
    ]
    rows: list[tuple[Any, ...]] = []
    for doc in docs:
        rows.append(
            tuple(typemap.to_py(doc.get(col.field), col.type_tag) for _, col in plan.out_columns)
        )
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


def execute_update(plan: planner.UpdatePlan, storage: Any, db: str) -> SQLResult:
    res = storage.update_matching(
        db, plan.table.collection, plan.filter, plan.update, multi=True
    )
    matched = int(res["matched"])
    return SQLResult(command_tag=f"UPDATE {matched}", rowcount=matched)


def execute_delete(plan: planner.DeletePlan, storage: Any, db: str) -> SQLResult:
    n = storage.delete_matching(db, plan.table.collection, plan.filter)
    return SQLResult(command_tag=f"DELETE {n}", rowcount=n)
