"""Execute a plan against ``Storage`` and shape the result rows.

Each function takes a plan (already lowered to Mongo structures by the planner),
performs the corresponding ``Storage`` call, and returns a ``SQLResult`` with a
Postgres ``CommandComplete`` tag. This is the only layer that touches storage;
the planner stays pure translation.
"""

from __future__ import annotations

from typing import Any

from secantus.paths import get_path
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


def execute_create_index(
    plan: planner.CreateIndexPlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    existing = [ix.get("name") for ix in storage.list_indexes(db, plan.collection)]
    if plan.name in existing:
        if plan.if_not_exists:
            return SQLResult(command_tag="CREATE INDEX")
        raise errors.SQLError("42P07", f'relation "{plan.name}" already exists')
    options = {"unique": True} if plan.unique else None
    storage.create_index(db, plan.collection, plan.name, plan.key_spec, options)
    return SQLResult(command_tag="CREATE INDEX")


def execute_drop_index(
    plan: planner.DropIndexPlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    # Postgres DROP INDEX names the index, not its table — find the owning
    # collection by scanning the catalog's tables for the index name.
    for tname in catalog.list_tables(db):
        table = catalog.get(db, tname)
        if table is None:
            continue
        if any(ix.get("name") == plan.name for ix in storage.list_indexes(db, table.collection)):
            storage.drop_index(db, table.collection, plan.name)
            return SQLResult(command_tag="DROP INDEX")
    if plan.if_exists:
        return SQLResult(command_tag="DROP INDEX")
    raise errors.SQLError("42704", f'index "{plan.name}" does not exist')


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


def execute_constant_select(plan: planner.ConstantSelectPlan) -> SQLResult:
    columns = [ColumnDesc(name, tag, typemap.PG_OID.get(tag, 25)) for name, tag, _ in plan.columns]
    row = tuple(value for _, _, value in plan.columns)
    return SQLResult(command_tag="SELECT 1", columns=columns, rows=[row], rowcount=1)


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
        # get_path walks dotted field paths (jsonb navigation); a plain field
        # name resolves to a top-level lookup.
        rows.append(
            tuple(
                typemap.to_py(get_path(doc, col.field), col.type_tag) for _, col in plan.out_columns
            )
        )
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


def _scalar_ctx(storage: Any, db: str, sctx: Any) -> Any:
    """The ScalarContext for evaluating derived sub-plans — reuse the caller's
    when present, else build one over the (catalog-backed) storage."""
    if sctx is not None:
        return sctx
    from secantus.sql import scalar
    from secantus.sql.catalog import Catalog

    return scalar.ScalarContext(storage=storage, catalog=Catalog(storage), db=db, session=None)


def _materialize_derived(plan: Any, storage: Any, db: str, sctx: Any = None) -> None:
    """Materialize a plan's derived-table subqueries into ephemeral collections.

    Each ``(SELECT ...) AS alias`` join source is run to rows (its own derived
    tables first) and registered under its alias so the main pipeline's
    ``$lookup`` can read it."""
    for dt in getattr(plan, "derived", []):
        rows = _run_subplan_to_docs(dt.plan, storage, db, sctx)
        storage.register_ephemeral(dt.name, rows)


def _run_subplan_to_docs(
    plan: Any, storage: Any, db: str, sctx: Any = None
) -> list[dict[str, Any]]:
    from secantus.aggregate import PipelineContext, apply_pipeline

    _materialize_derived(plan, storage, db, sctx)
    if isinstance(plan, planner.PipelineSelectPlan):
        docs = storage.find_matching(db, plan.base_collection, plan.base_filter)
        ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
        return apply_pipeline(docs, plan.pipeline, ctx)
    if isinstance(plan, planner.EvaluatedSelectPlan):
        rows = _evaluated_value_rows(plan, storage, db, _scalar_ctx(storage, db, sctx))
        names = [n for n, _ in plan.out_columns]
        return [dict(zip(names, row, strict=True)) for row in rows]
    raise errors.feature_not_supported("unsupported derived-table plan")


def _evaluated_value_rows(
    plan: planner.EvaluatedSelectPlan, storage: Any, db: str, sctx: Any
) -> list[tuple[Any, ...]]:
    """Run an evaluated plan's pipeline, evaluate each output expression per row
    (expanding set-returning functions), then apply ORDER BY / DISTINCT / LIMIT."""
    from secantus.aggregate import PipelineContext, apply_pipeline
    from secantus.sql import scalar

    _materialize_derived(plan, storage, db, sctx)
    docs = storage.find_matching(db, plan.base_collection, plan.base_filter)
    ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
    docs = apply_pipeline(docs, plan.pipeline, ctx)

    def make_scope(doc: dict[str, Any]):
        def scope(node: Any) -> Any:
            path, _ = plan.resolve(node)
            return get_path(doc, path)

        return scope

    scored: list[tuple[tuple[Any, ...], tuple[Any, ...]]] = []
    for doc in docs:
        scope = make_scope(doc)
        keys = tuple(scalar.evaluate(oe, scope, sctx) for oe, _ in plan.order)
        for vt in _expand_srf(plan, scope, sctx):
            scored.append((keys, vt))

    for i in reversed(range(len(plan.order))):
        direction = plan.order[i][1]
        scored.sort(key=lambda r, i=i: (r[0][i] is None, r[0][i]), reverse=(direction == -1))

    rows = [vt for _, vt in scored]
    if plan.distinct:
        seen: set = set()
        deduped: list[tuple[Any, ...]] = []
        for row in rows:
            key = tuple(repr(v) for v in row)
            if key not in seen:
                seen.add(key)
                deduped.append(row)
        rows = deduped
    if plan.skip:
        rows = rows[plan.skip :]
    if plan.limit:
        rows = rows[: plan.limit]
    return rows


def _expand_srf(plan: planner.EvaluatedSelectPlan, scope: Any, sctx: Any) -> list[tuple[Any, ...]]:
    """Evaluate one source row's output columns, expanding set-returning
    functions (unnest / generate_subscripts) into one row per array element.
    Multiple SRFs over the same array zip in parallel; the longest wins, shorter
    arrays pad with NULL (Postgres semantics)."""
    from secantus.sql import scalar

    srfs: dict[int, tuple[str, list[Any]]] = {}
    for idx, expr in enumerate(plan.out_exprs):
        srf = planner._srf_of(expr)
        if srf is None:
            continue
        kind, arr_expr = srf
        val = scalar.evaluate(arr_expr, scope, sctx)
        if kind == "jsonb_object_keys":
            items = list(val.keys()) if isinstance(val, dict) else []
        elif isinstance(val, (list, tuple)):
            items = list(val)
        else:
            items = [] if val is None else [val]
        srfs[idx] = (kind, items)

    if not srfs:
        return [tuple(scalar.evaluate(e, scope, sctx) for e in plan.out_exprs)]

    scalars = {
        idx: scalar.evaluate(e, scope, sctx)
        for idx, e in enumerate(plan.out_exprs)
        if idx not in srfs
    }
    length = max((len(arr) for _, arr in srfs.values()), default=0)
    rows: list[tuple[Any, ...]] = []
    for k in range(length):
        row: list[Any] = []
        for idx in range(len(plan.out_exprs)):
            if idx in srfs:
                kind, arr = srfs[idx]
                if kind == "generate_subscripts":  # 1-based ordinal
                    row.append(k + 1 if k < len(arr) else None)
                else:  # unnest / jsonb_array_elements / jsonb_object_keys → element/key
                    row.append(arr[k] if k < len(arr) else None)
            else:
                row.append(scalars[idx])
        rows.append(tuple(row))
    return rows


def execute_pipeline_select(plan: planner.PipelineSelectPlan, storage: Any, db: str) -> SQLResult:
    """Run a JOIN / GROUP BY / aggregate SELECT through the aggregation engine."""
    from secantus.aggregate import PipelineContext, apply_pipeline

    _materialize_derived(plan, storage, db)
    docs = storage.find_matching(db, plan.base_collection, plan.base_filter)
    ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
    result = apply_pipeline(docs, plan.pipeline, ctx)
    columns = [ColumnDesc(name, tag, typemap.PG_OID.get(tag, 25)) for name, tag in plan.out_columns]
    rows = [
        tuple(typemap.to_py(doc.get(name), tag) for name, tag in plan.out_columns) for doc in result
    ]
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


def execute_evaluated_select(
    plan: planner.EvaluatedSelectPlan, storage: Any, db: str, sctx: Any
) -> SQLResult:
    """Run a SELECT whose list / ORDER BY needs per-row evaluation (scalar /
    set-returning functions, CASE, correlated subqueries)."""
    rows = _evaluated_value_rows(plan, storage, db, sctx)
    columns = [ColumnDesc(name, tag, typemap.PG_OID.get(tag, 25)) for name, tag in plan.out_columns]
    out_rows = [
        tuple(typemap.to_py(v, tag) for v, (_, tag) in zip(row, plan.out_columns, strict=True))
        for row in rows
    ]
    return SQLResult(
        command_tag=f"SELECT {len(out_rows)}",
        columns=columns,
        rows=out_rows,
        rowcount=len(out_rows),
    )


def execute_update(plan: planner.UpdatePlan, storage: Any, db: str) -> SQLResult:
    res = storage.update_matching(db, plan.table.collection, plan.filter, plan.update, multi=True)
    matched = int(res["matched"])
    return SQLResult(command_tag=f"UPDATE {matched}", rowcount=matched)


def execute_delete(plan: planner.DeletePlan, storage: Any, db: str) -> SQLResult:
    n = storage.delete_matching(db, plan.table.collection, plan.filter)
    return SQLResult(command_tag=f"DELETE {n}", rowcount=n)
