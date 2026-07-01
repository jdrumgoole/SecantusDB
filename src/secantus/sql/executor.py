"""Execute a plan against ``Storage`` and shape the result rows.

Each function takes a plan (already lowered to Mongo structures by the planner),
performs the corresponding ``Storage`` call, and returns a ``SQLResult`` with a
Postgres ``CommandComplete`` tag. This is the only layer that touches storage;
the planner stays pure translation.
"""

from __future__ import annotations

import functools
from typing import Any

import bson

from secantus.paths import get_path
from secantus.sql import errors, planner, typemap
from secantus.sql.catalog import Catalog
from secantus.sql.result import ColumnDesc, SQLResult


def _pg_sort(items: list[Any], key_of: Any, specs: list[tuple[int, bool]]) -> None:
    """Stable in-place sort with Postgres ORDER BY semantics.

    ``key_of(item)`` returns the tuple of ordering-key values; ``specs`` is the
    parallel list of ``(direction, nulls_first)`` (direction 1 asc / -1 desc).
    NULLs sort to the front or back per ``nulls_first`` independent of direction —
    Postgres orders NULL as though it were the largest value, so this can't be
    delegated to Mongo's sort (which treats NULL/missing as the smallest)."""

    def cmp(a: Any, b: Any) -> int:
        ka, kb = key_of(a), key_of(b)
        for i, (direction, nulls_first) in enumerate(specs):
            x, y = ka[i], kb[i]
            if x is None and y is None:
                continue
            if x is None:
                return -1 if nulls_first else 1
            if y is None:
                return 1 if nulls_first else -1
            if x == y:
                continue
            base = -1 if x < y else 1
            return -base if direction == -1 else base
        return 0

    items.sort(key=functools.cmp_to_key(cmp))


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


def _returning_result(
    docs: list[dict[str, Any]],
    returning: list[tuple[str, Any, Any]],
    command_tag: str,
    rowcount: int,
    table: Any = None,
    storage: Any = None,
    db: str | None = None,
) -> SQLResult:
    """Shape a write statement's ``RETURNING`` rows the same way a SELECT does —
    so the wire layer emits a RowDescription + DataRows ahead of CommandComplete.

    Each returning item is ``(name, Column, expr)``; a plain item (``expr`` None)
    reads straight from the doc, a computed one is evaluated per row against a
    scope over the returned doc."""
    columns = [
        ColumnDesc(name, col.type_tag, typemap.PG_OID.get(col.type_tag, 25))
        for name, col, _ in returning
    ]
    ctx = None
    if any(expr is not None for _, _, expr in returning):
        from secantus.sql import scalar

        ctx = scalar.ScalarContext(storage=storage, catalog=None, db=db, session=None)

    def cell(doc: dict[str, Any], col: Any, expr: Any) -> Any:
        if expr is None:
            return typemap.to_py(get_path(doc, col.field), col.type_tag)
        from secantus.sql import scalar

        def scope(node: Any) -> Any:
            return get_path(doc, table.field_for(node.name))

        return typemap.to_py(scalar.evaluate(expr, scope, ctx), col.type_tag)

    rows = [tuple(cell(doc, col, expr) for _, col, expr in returning) for doc in docs]
    return SQLResult(command_tag=command_tag, columns=columns, rows=rows, rowcount=rowcount)


def execute_insert(
    plan: planner.InsertPlan,
    storage: Any,
    db: str,
    catalog: Catalog | None = None,
    session: Any = None,
) -> SQLResult:
    if plan.on_conflict is not None:
        return _execute_insert_on_conflict(plan, storage, db, catalog, session)
    if plan.returning is not None:
        # Pin an ``_id`` on every doc up front so the in-hand list is the
        # authoritative inserted set to project from (storage may deep-copy on
        # insert, so we can't rely on it back-filling ``_id`` into plan.docs).
        for doc in plan.docs:
            doc.setdefault("_id", bson.ObjectId())
    inserted, write_errors = storage.insert(db, plan.table.collection, plan.docs)
    if write_errors:
        _raise_write_error(write_errors[0], plan.table)
    if plan.returning is not None:
        return _returning_result(
            plan.docs[:inserted],
            plan.returning,
            f"INSERT 0 {inserted}",
            inserted,
            plan.table,
            storage,
            db,
        )
    return SQLResult(command_tag=f"INSERT 0 {inserted}", rowcount=inserted)


def _raise_write_error(err: dict[str, Any], table: planner.TableDef) -> None:
    if err.get("code") == 11000:
        raise errors.unique_violation(
            f'duplicate key value violates unique constraint on "{table.name}"'
        )
    raise errors.SQLError("XX000", err.get("errmsg", "insert failed"))


def _execute_insert_on_conflict(
    plan: planner.InsertPlan, storage: Any, db: str, catalog: Catalog | None, session: Any
) -> SQLResult:
    """Execute ``INSERT … ON CONFLICT``. Each proposed row is probed against the
    conflict target: a clean row inserts; a conflicting row is skipped
    (``DO NOTHING``) or updated in place (``DO UPDATE``). The command tag counts
    rows inserted *or* updated (matching Postgres); skipped rows don't count.
    ``RETURNING`` projects the inserted and updated rows, not the skipped ones."""
    from secantus.sql import scalar

    oc = plan.on_conflict
    coll = plan.table.collection
    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    affected = 0
    result_docs: list[dict[str, Any]] = []
    for doc in plan.docs:
        if plan.returning is not None:
            doc.setdefault("_id", bson.ObjectId())
        existing = _find_conflict(storage, db, coll, oc, doc)
        if existing is None:
            inserted, write_errors = storage.insert(db, coll, [doc])
            if write_errors:
                # A bare ``DO NOTHING`` (no conflict target) absorbs any unique
                # collision — including one on an index other than the probed
                # target; with a target, a collision elsewhere is still an error.
                if not oc.conflict_fields and write_errors[0].get("code") == 11000:
                    continue
                _raise_write_error(write_errors[0], plan.table)
            affected += inserted
            if inserted:
                result_docs.append(doc)
            continue
        if oc.action == "nothing":
            continue
        updated = _apply_conflict_update(plan.table, storage, db, oc, existing, doc, sctx)
        if updated is not None:
            affected += 1
            result_docs.append(updated)
    tag = f"INSERT 0 {affected}"
    if plan.returning is not None:
        return _returning_result(
            result_docs, plan.returning, tag, affected, plan.table, storage, db
        )
    return SQLResult(command_tag=tag, rowcount=affected)


def _find_conflict(
    storage: Any, db: str, coll: str, oc: planner.OnConflict, doc: dict[str, Any]
) -> dict[str, Any] | None:
    """The existing row that ``doc`` would conflict with on the conflict target,
    or None. A row with any target field unset can't collide (a fresh PK is
    unique), so it short-circuits to an insert."""
    if not oc.conflict_fields or any(f not in doc for f in oc.conflict_fields):
        return None
    found = storage.find_matching(db, coll, {f: doc[f] for f in oc.conflict_fields}, limit=1)
    return found[0] if found else None


def _apply_conflict_update(
    table: planner.TableDef,
    storage: Any,
    db: str,
    oc: planner.OnConflict,
    existing: dict[str, Any],
    excluded: dict[str, Any],
    sctx: Any,
) -> dict[str, Any] | None:
    """Apply a ``DO UPDATE`` to the conflicting ``existing`` row. ``EXCLUDED``
    references resolve to the proposed ``excluded`` row; bare / target-qualified
    columns to the existing row. Returns the post-update doc, or None when a
    ``WHERE`` predicate gates the update out (the row is left untouched)."""
    import copy

    from secantus.sql import scalar

    def scope(node: Any) -> Any:
        field = table.field_for(node.name)
        tbl = (node.table or "").lower()
        source = excluded if tbl == "excluded" else existing
        return source.get(field)

    if oc.where is not None and not scalar._truthy(scalar.evaluate(oc.where, scope, sctx)):
        return None
    set_doc: dict[str, Any] = {}
    for field, type_tag, expr in oc.set_exprs:
        set_doc[field] = typemap.coerce(scalar.evaluate(expr, scope, sctx), type_tag)
    storage.update_matching(db, table.collection, {"_id": existing["_id"]}, {"$set": set_doc})
    updated = copy.deepcopy(existing)
    updated.update(set_doc)
    return updated


def execute_constant_select(plan: planner.ConstantSelectPlan) -> SQLResult:
    columns = [ColumnDesc(name, tag, typemap.PG_OID.get(tag, 25)) for name, tag, _ in plan.columns]
    rows = [tuple(value for _, _, value in plan.columns)] if plan.emit else []
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


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

    if plan.order:
        # NULL placement follows Postgres, not Mongo sort order, so order in
        # Python; that also pulls OFFSET/LIMIT off the storage fetch.
        docs = storage.find_matching(db, plan.table.collection, plan.filter)
        _pg_sort(
            docs,
            lambda d: tuple(get_path(d, f) for f, _, _ in plan.order),
            [(direction, nf) for _, direction, nf in plan.order],
        )
        if plan.skip:
            docs = docs[plan.skip :]
        if plan.limit:
            docs = docs[: plan.limit]
    else:
        docs = storage.find_matching(
            db, plan.table.collection, plan.filter, skip=plan.skip, limit=plan.limit
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


def execute_correlated_select(
    plan: planner.CorrelatedSelectPlan, storage: Any, db: str, catalog: Catalog, session: Any
) -> SQLResult:
    """Execute a single-table SELECT whose WHERE references the outer row.

    The WHERE can't push down to a Mongo filter, so we fetch every candidate row
    (in ORDER BY order) and evaluate the predicate per row with the scalar
    evaluator — a correlated subquery reads inner-table rows through the same
    storage view, with outer-row references falling through to ``scope``."""
    from secantus.sql import scalar

    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    table = plan.table
    docs = storage.find_matching(db, table.collection, {})

    def make_scope(doc: dict[str, Any]):
        # Only outer-table columns reach this scope; a correlated subquery's own
        # inner columns are resolved inside scalar._inner_row_scopes.
        def scope(node: Any) -> Any:
            return get_path(doc, table.field_for(node.name))

        return scope

    def keep(doc: dict[str, Any]) -> bool:
        result = scalar.evaluate(plan.where, make_scope(doc), sctx)
        return bool(result) if result is not None else False

    matched = [doc for doc in docs if keep(doc)]

    if plan.count_star:
        return SQLResult(
            command_tag="SELECT 1",
            columns=[ColumnDesc(plan.count_alias, "int8", typemap.PG_OID["int8"])],
            rows=[(len(matched),)],
            rowcount=1,
        )

    # Order the survivors (Postgres NULL placement), then slice OFFSET/LIMIT.
    if plan.order:
        _pg_sort(
            matched,
            lambda d: tuple(get_path(d, f) for f, _, _ in plan.order),
            [(direction, nf) for _, direction, nf in plan.order],
        )
    if plan.skip:
        matched = matched[plan.skip :]
    if plan.limit:
        matched = matched[: plan.limit]

    columns = [
        ColumnDesc(name, col.type_tag, typemap.PG_OID.get(col.type_tag, 25))
        for name, col in plan.out_columns
    ]
    rows = [
        tuple(typemap.to_py(get_path(doc, col.field), col.type_tag) for _, col in plan.out_columns)
        for doc in matched
    ]
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
        docs, remaining = _pipeline_input_docs(plan, storage, db, sctx)
        ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
        return apply_pipeline(docs, remaining, ctx)
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
    from secantus.sql import scalar, window

    _materialize_derived(plan, storage, db, sctx)
    docs = storage.find_matching(db, plan.base_collection, plan.base_filter)
    ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
    docs = apply_pipeline(docs, plan.pipeline, ctx)
    # A correlated / EXISTS WHERE that couldn't push into the pipeline is applied
    # per joined row here (before windows / projection see the survivors); the
    # scope resolves outer columns via the join resolver, and the subquery reads
    # its inner rows through the same storage view.
    if plan.where is not None:

        def keep(doc: dict[str, Any]) -> bool:
            def scope(node: Any) -> Any:
                return get_path(doc, plan.resolve(node)[0])

            r = scalar.evaluate(plan.where, scope, sctx)
            return bool(r) if r is not None else False

        docs = [d for d in docs if keep(d)]
    # Window functions depend on the whole partition, so they're computed over all
    # rows up front and stored on each doc; the scope resolves an exp.Window node
    # (keyed by id) to that precomputed field.
    win_field = window.compute_windows(plan.out_exprs, docs, plan.resolve, sctx)

    def make_scope(doc: dict[str, Any]):
        def scope(node: Any) -> Any:
            field = win_field.get(id(node))
            if field is not None:
                return get_path(doc, field)
            path, _ = plan.resolve(node)
            return get_path(doc, path)

        return scope

    scored: list[tuple[tuple[Any, ...], tuple[Any, ...]]] = []
    for doc in docs:
        scope = make_scope(doc)
        keys = tuple(scalar.evaluate(oe, scope, sctx) for oe, _, _ in plan.order)
        for vt in _expand_srf(plan, scope, sctx):
            scored.append((keys, vt))

    _pg_sort(scored, lambda r: r[0], [(direction, nf) for _, direction, nf in plan.order])

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


def _pipeline_input_docs(
    plan: planner.PipelineSelectPlan, storage: Any, db: str, sctx: Any
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Fetch the docs to feed the aggregation and return ``(docs, remaining_stages)``.

    A correlated / EXISTS residual WHERE can't lower to a ``$match``, so it's
    applied in Python: the leading ``residual_split`` stages (a JOIN prefix, or
    none for a single-table GROUP BY) run first, the rows are filtered, and the
    remaining stages (the ``$group`` etc.) run over the survivors."""
    from secantus.aggregate import PipelineContext, apply_pipeline
    from secantus.sql import scalar

    docs = storage.find_matching(db, plan.base_collection, plan.base_filter)
    if plan.residual_where is None:
        return docs, plan.pipeline
    if plan.residual_split:
        ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
        docs = apply_pipeline(docs, plan.pipeline[: plan.residual_split], ctx)
    remaining = plan.pipeline[plan.residual_split :]
    sc = sctx or _scalar_ctx(storage, db, None)
    resolve = plan.residual_resolve

    def keep(doc: dict[str, Any]) -> bool:
        def scope(node: Any) -> Any:
            return get_path(doc, resolve(node)[0])

        r = scalar.evaluate(plan.residual_where, scope, sc)
        return bool(r) if r is not None else False

    return [d for d in docs if keep(d)], remaining


def execute_pipeline_select(
    plan: planner.PipelineSelectPlan, storage: Any, db: str, sctx: Any = None
) -> SQLResult:
    """Run a JOIN / GROUP BY / aggregate SELECT through the aggregation engine."""
    from secantus.aggregate import PipelineContext, apply_pipeline

    _materialize_derived(plan, storage, db, sctx)
    docs, remaining = _pipeline_input_docs(plan, storage, db, sctx)
    ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
    result = apply_pipeline(docs, remaining, ctx)
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
    coll = plan.table.collection
    if plan.returning is not None:
        # RETURNING yields the post-image, so capture the matched ``_id``s first,
        # apply the update, then re-read those rows.
        ids = [d["_id"] for d in storage.find_matching(db, coll, plan.filter)]
    res = storage.update_matching(db, coll, plan.filter, plan.update, multi=True)
    matched = int(res["matched"])
    if plan.returning is not None:
        post = storage.find_matching(db, coll, {"_id": {"$in": ids}}) if ids else []
        return _returning_result(
            post, plan.returning, f"UPDATE {matched}", matched, plan.table, storage, db
        )
    return SQLResult(command_tag=f"UPDATE {matched}", rowcount=matched)


def execute_delete(plan: planner.DeletePlan, storage: Any, db: str) -> SQLResult:
    coll = plan.table.collection
    # RETURNING yields the deleted rows, so snapshot them before the delete.
    victims = storage.find_matching(db, coll, plan.filter) if plan.returning is not None else []
    n = storage.delete_matching(db, coll, plan.filter)
    if plan.returning is not None:
        return _returning_result(victims, plan.returning, f"DELETE {n}", n, plan.table, storage, db)
    return SQLResult(command_tag=f"DELETE {n}", rowcount=n)
