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

from secantus.paths import get_path, has_path
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


def _order_key_fn(
    order: list[tuple[str, int, bool]], enum_orders: dict[str, list[str]] | None = None
) -> Any:
    """Build the ``key_of(doc)`` used by ``_pg_sort`` for a list of ORDER BY field
    paths. An enum-typed order field maps its label value to the label's ordinal in
    the enum's declared order (``enum_orders[field]``) so sorting follows the
    declared order, not lexical text order — a NULL stays NULL for placement."""
    ordinals: dict[str, dict[str, int]] = {}
    if enum_orders:
        ordinals = {
            f: {lbl: i for i, lbl in enumerate(labels)} for f, labels in enum_orders.items()
        }

    def key_of(doc: Any) -> tuple:
        out = []
        for field_path, _, _ in order:
            value = get_path(doc, field_path)
            omap = ordinals.get(field_path)
            if omap is not None and value is not None:
                value = omap.get(value, len(omap))  # unknown label sorts last
            out.append(value)
        return tuple(out)

    return key_of


def _resolve_user_type_column(col: Any, catalog: Catalog, db: str) -> Any:
    """Resolve a column whose declared type is a user-defined name. The planner
    tags any unknown type as ``enum_type`` (it can't reach storage); here we
    disambiguate: a declared enum keeps that tag, a declared domain is rewritten
    to ``domain_type`` with the domain's base type tag (inheriting the domain's
    DEFAULT when the column has none), and anything else is 42704."""
    import dataclasses

    if col.enum_type is None:
        return col
    name = col.enum_type
    if catalog.enum_exists(db, name):
        return col
    domain = catalog.get_domain(db, name)
    if domain is None:
        raise errors.SQLError("42704", f'type "{name}" does not exist')
    inherit_default = not col.has_default and bool(domain.get("has_default"))
    return dataclasses.replace(
        col,
        enum_type=None,
        domain_type=name,
        type_tag=domain["base_tag"],
        has_default=col.has_default or inherit_default,
        default=domain.get("default") if inherit_default else col.default,
    )


def execute_create_table(
    plan: planner.CreateTablePlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    if catalog.exists(db, plan.table.name):
        if plan.if_not_exists:
            return SQLResult(command_tag="CREATE TABLE")
        raise errors.duplicate_table(plan.table.name)
    # A user-defined column type (the planner records it as ``enum_type``, since
    # it can't reach storage to tell enum from domain) must resolve to a declared
    # enum *or* domain — else 42704. A domain column adopts the domain's base tag
    # and inherits its DEFAULT when the column declares none.
    plan.table.columns = [_resolve_user_type_column(col, catalog, db) for col in plan.table.columns]
    catalog.put(db, plan.table)
    storage.create_collection(db, plan.table.collection)
    # Auto-create the sequence behind each SERIAL column (owned by the table).
    for seq in plan.sequences:
        catalog.create_sequence(
            db,
            seq["name"],
            start=seq.get("start", 1),
            increment=seq.get("increment", 1),
            owned_by=f"{plan.table.name}.{seq['column']}",
        )
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
    # Drop any sequences the table owned (SERIAL columns).
    for col in table.columns:
        if col.sequence is not None:
            seq = catalog.get_sequence(db, col.sequence)
            if seq is not None and seq.get("owned_by") == f"{table.name}.{col.name}":
                catalog.drop_sequence(db, col.sequence)
    return SQLResult(command_tag="DROP TABLE")


def execute_alter_table(
    plan: planner.AlterTablePlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    """Apply ``ALTER TABLE`` actions to the catalog (and the backing collection
    where the data must follow — a dropped column's field is ``$unset``, a renamed
    non-PK column's field is ``$rename``d)."""
    table = catalog.get(db, plan.name)
    if table is None:
        if plan.if_exists:
            return SQLResult(command_tag="ALTER TABLE")
        raise errors.undefined_table(plan.name)
    old_name = table.name
    for action in plan.actions:
        _apply_alter_action(action, table, storage, db)
    catalog.replace(db, table, old_name=old_name)
    return SQLResult(command_tag="ALTER TABLE")


def execute_comment(stmt: Any, catalog: Catalog, storage: Any, db: str) -> SQLResult:
    """``COMMENT ON TABLE t IS '…'`` / ``COMMENT ON COLUMN t.c IS '…'`` — store the
    comment in the catalog so it reflects via ``pg_description`` (SQLAlchemy's
    ``get_columns`` / ``get_table_comment``). ``IS NULL`` removes it."""
    import dataclasses

    from sqlglot import exp

    kind = str(stmt.args.get("kind") or "").upper()
    expr = stmt.args.get("expression")
    text = expr.this if isinstance(expr, exp.Literal) else None
    if text == planner.UNCOMMENT_SENTINEL:
        text = None
    if kind == "TABLE":
        table = catalog.get(db, stmt.this.name)
        if table is None:
            raise errors.undefined_table(stmt.this.name)
        table.comment = text
        catalog.replace(db, table)
        return SQLResult(command_tag="COMMENT")
    if kind == "COLUMN":
        col_node = stmt.this  # exp.Column: table.col
        tname, cname = col_node.table, col_node.name
        table = catalog.get(db, tname)
        if table is None:
            raise errors.undefined_table(tname)
        if table.column(cname) is None:
            raise errors.undefined_column(cname)
        table.columns = [
            dataclasses.replace(c, comment=text) if c.name == cname else c for c in table.columns
        ]
        catalog.replace(db, table)
        return SQLResult(command_tag="COMMENT")
    raise errors.feature_not_supported(f"COMMENT ON {kind} is not supported")


def execute_create_view(stmt: Any, catalog: Catalog, storage: Any, db: str) -> SQLResult:
    """``CREATE [OR REPLACE] VIEW v AS SELECT …`` — store the SELECT definition.
    Querying the view expands it as a subquery (see ``engine._expand_views``)."""
    name = stmt.this.name
    replace = bool(stmt.args.get("replace"))
    if catalog.exists(db, name):
        raise errors.SQLError("42P07", f'relation "{name}" already exists')
    if not replace and catalog.get_view(db, name) is not None:
        raise errors.SQLError("42P07", f'relation "{name}" already exists')
    catalog.put_view(db, name, stmt.expression.sql(dialect="postgres"))
    return SQLResult(command_tag="CREATE VIEW")


def execute_drop_view(stmt: Any, catalog: Catalog, storage: Any, db: str) -> SQLResult:
    name = stmt.this.name
    if not catalog.drop_view(db, name) and not stmt.args.get("exists"):
        raise errors.SQLError("42P01", f'view "{name}" does not exist')
    return SQLResult(command_tag="DROP VIEW")


def _apply_alter_action(action: Any, table: Any, storage: Any, db: str) -> None:
    from sqlglot import exp

    from secantus.sql.catalog import Column

    coll = table.collection
    if isinstance(action, exp.ColumnDef):  # ADD COLUMN [IF NOT EXISTS] name type
        name = action.name
        if table.column(name) is not None:
            if action.args.get("exists"):
                return
            raise errors.SQLError(
                "42701", f'column "{name}" of relation "{table.name}" already exists'
            )
        tag = typemap.type_tag_for_sql(action.args["kind"])
        if tag is None:
            raise errors.feature_not_supported(
                f"unsupported column type: {action.args['kind'].sql()}"
            )
        cons = [type(c.kind).__name__ for c in (action.args.get("constraints") or [])]
        table.columns.append(
            Column(
                name=name,
                type_tag=tag,
                field=name,
                pk=False,
                nullable="NotNullColumnConstraint" not in cons,
            )
        )
        return
    if (
        isinstance(action, exp.Drop)
        and (action.args.get("kind") or "COLUMN").upper() == "CONSTRAINT"
    ):
        # DROP CONSTRAINT [IF EXISTS] name — remove a declared FK / CHECK / UNIQUE.
        name = action.this.name
        buckets = (table.foreign_keys, table.check_constraints, table.unique_constraints)
        if not any(any(c.name == name for c in b) for b in buckets):
            if action.args.get("exists"):
                return
            raise errors.SQLError(
                "42704", f'constraint "{name}" of relation "{table.name}" does not exist'
            )
        table.foreign_keys = [c for c in table.foreign_keys if c.name != name]
        table.check_constraints = [c for c in table.check_constraints if c.name != name]
        table.unique_constraints = [c for c in table.unique_constraints if c.name != name]
        return
    if isinstance(action, exp.Drop):  # DROP COLUMN [IF EXISTS] name
        name = action.this.name
        col = table.column(name)
        if col is None:
            if action.args.get("exists"):
                return
            raise errors.undefined_column(name)
        if col.pk:
            raise errors.feature_not_supported("dropping the PRIMARY KEY column is not supported")
        table.columns = [c for c in table.columns if c.name != name]
        storage.update_matching(db, coll, {}, {"$unset": {col.field: ""}}, multi=True)
        return
    if isinstance(action, exp.RenameColumn):  # RENAME COLUMN a TO b
        old = action.this.name
        new = action.args["to"].name
        col = table.column(old)
        if col is None:
            if action.args.get("exists"):
                return
            raise errors.undefined_column(old)
        if table.column(new) is not None:
            raise errors.SQLError(
                "42701", f'column "{new}" of relation "{table.name}" already exists'
            )
        new_field = col.field if col.pk else new
        table.columns = [
            Column(new, c.type_tag, new_field, c.pk, c.nullable) if c.name == old else c
            for c in table.columns
        ]
        if not col.pk and col.field != new_field:
            storage.update_matching(db, coll, {}, {"$rename": {col.field: new_field}}, multi=True)
        return
    if isinstance(action, exp.AlterRename):  # RENAME TO newname
        new_name = action.this.name
        if table.collection == table.name:
            # A declared table maps 1:1 to a same-named collection — move the
            # collection too, so the old name stops resolving (a leftover
            # collection would otherwise reflect as a phantom table).
            ok, err = storage.rename_collection(db, table.collection, db, new_name)
            if not ok:
                raise errors.SQLError("42P07", err or f'relation "{new_name}" already exists')
            table.collection = new_name
        table.name = new_name
        return
    if isinstance(action, exp.AlterColumn):
        # ALTER COLUMN c { TYPE t | SET/DROP DEFAULT | SET/DROP NOT NULL }.
        import dataclasses

        from secantus.sql import planner

        name = action.this.name
        col = table.column(name)
        if col is None:
            raise errors.undefined_column(name)
        if action.args.get("dtype") is not None:  # TYPE t — retype in the catalog
            tag = typemap.type_tag_for_sql(action.args["dtype"])
            if tag is None:
                raise errors.feature_not_supported(
                    f"unsupported column type: {action.args['dtype'].sql()}"
                )
            new_col = dataclasses.replace(col, type_tag=tag)
        elif action.args.get("default") is not None:  # SET DEFAULT <literal>
            has_def, value = planner._literal_default(action.args["default"], col.type_tag)
            if not has_def:
                raise errors.feature_not_supported(
                    f"only a literal DEFAULT is supported: {action.args['default'].sql()}"
                )
            new_col = dataclasses.replace(col, has_default=True, default=value)
        elif action.args.get("allow_null") is not None:  # SET/DROP NOT NULL
            new_col = dataclasses.replace(col, nullable=bool(action.args["allow_null"]))
        elif action.args.get("drop"):  # DROP DEFAULT
            new_col = dataclasses.replace(col, has_default=False, default=None)
        else:
            raise errors.feature_not_supported(f"unsupported ALTER COLUMN action: {action.sql()}")
        table.columns = [new_col if c.name == name else c for c in table.columns]
        return
    if isinstance(action, exp.AddConstraint):
        # ADD [CONSTRAINT name] { FOREIGN KEY (…) REFERENCES … | CHECK (…) |
        # UNIQUE (…) } — declared, reflected, never enforced (same as a CREATE
        # TABLE constraint). Unnamed ``ADD UNIQUE (…)`` is accepted; unnamed ``ADD
        # CHECK (…)`` isn't parseable by sqlglot, so a CHECK needs CONSTRAINT name.
        from secantus.sql import planner

        for con in action.args.get("expressions") or []:
            con_name = None
            node = con
            if isinstance(con, exp.Constraint):
                con_name = con.this.name if con.this else None
                inner = con.args.get("expressions") or []
                node = inner[0] if inner else None
            if isinstance(node, exp.ForeignKey):
                cols = tuple(c.name for c in node.args.get("expressions") or [])
                ref = node.args.get("reference")
                if ref is None:
                    raise errors.feature_not_supported(
                        f"unsupported ADD FOREIGN KEY: {action.sql()}"
                    )
                table.foreign_keys.append(planner._make_fk(table.name, cols, ref, con_name))
            elif isinstance(node, exp.CheckColumnConstraint):
                table.check_constraints.append(
                    planner.make_check_constraint(node, table.name, con_name)
                )
            elif isinstance(node, exp.UniqueColumnConstraint):
                table.unique_constraints.append(
                    planner.make_unique_constraint(node, table.name, con_name)
                )
            else:
                raise errors.feature_not_supported(f"unsupported ADD CONSTRAINT: {action.sql()}")
        return
    raise errors.feature_not_supported(f"unsupported ALTER TABLE action: {action.sql()}")


def execute_create_index(
    plan: planner.CreateIndexPlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    existing = [ix.get("name") for ix in storage.list_indexes(db, plan.collection)]
    if plan.name in existing:
        if plan.if_not_exists:
            return SQLResult(command_tag="CREATE INDEX")
        raise errors.SQLError("42P07", f'relation "{plan.name}" already exists')
    options: dict[str, Any] = {}
    if plan.unique:
        options["unique"] = True
    if plan.partial_filter:
        options["partialFilterExpression"] = plan.partial_filter
    storage.create_index(db, plan.collection, plan.name, plan.key_spec, options or None)
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
    _assign_sequences(plan.docs, plan.table, db, catalog, session)
    if plan.returning is not None:
        # Pin an ``_id`` on every doc up front so the in-hand list is the
        # authoritative inserted set to project from (storage may deep-copy on
        # insert, so we can't rely on it back-filling ``_id`` into plan.docs).
        for doc in plan.docs:
            doc.setdefault("_id", bson.ObjectId())
    enforce_insert_rows(plan.docs, plan.table, storage, db, catalog, session)
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


@functools.lru_cache(maxsize=256)
def _parse_check_expr(text: str) -> Any:
    import sqlglot

    return sqlglot.parse_one(text, read="postgres")


def _validate_write_row(doc: dict[str, Any], table: Any, ctx: Any) -> None:
    """Enforce a declared table's NOT NULL and CHECK constraints against a row
    (the post-image for an UPDATE). Raises on violation; no-op for reflected
    (schema-on-read) tables, which carry no declared constraints.

    NOT NULL — a non-nullable, non-PK column that is null / missing violates
    ``23502`` (the PK is skipped: storage auto-assigns ``_id``). CHECK — a
    predicate that evaluates to FALSE violates ``23514``; NULL passes (Postgres
    treats an unknown CHECK result as satisfied)."""
    from secantus.sql import scalar

    if getattr(table, "reflected", False):
        return
    for col in table.columns:
        if col.pk or col.nullable:
            continue
        if get_path(doc, col.field) is None:
            raise errors.SQLError(
                "23502",
                f'null value in column "{col.name}" of relation "{table.name}" '
                "violates not-null constraint",
            )
    if not table.check_constraints:
        return

    def scope(node: Any) -> Any:
        col = table.column(node.name)
        return get_path(doc, col.field if col is not None else node.name)

    for ck in table.check_constraints:
        value = scalar.evaluate(_parse_check_expr(ck.expression), scope, ctx)
        if value is not None and not scalar._truthy(value):
            raise errors.SQLError(
                "23514",
                f'new row for relation "{table.name}" violates check constraint "{ck.name}"',
            )


def _validate_rows(
    docs: list[dict[str, Any]], table: Any, storage: Any, db: str, session: Any, catalog: Any = None
) -> None:
    from secantus.sql import scalar

    if getattr(table, "reflected", False) or not (table.check_constraints or _has_not_null(table)):
        return
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    for doc in docs:
        _validate_write_row(doc, table, ctx)


def _has_not_null(table: Any) -> bool:
    return any(not c.pk and not c.nullable for c in table.columns)


# -- combined enforcement entry points (shared by INSERT / UPDATE / ON CONFLICT /
#    MERGE) so every write path enforces the same NOT NULL / CHECK / UNIQUE / FK.


def _assign_sequences(
    docs: list[dict[str, Any]], table: Any, db: str, catalog: Any, session: Any
) -> None:
    """Fill sequence-backed columns (SERIAL / ``DEFAULT nextval``) omitted from an
    INSERT: draw each doc's next value from the sequence and record it as the
    session's ``currval`` / ``lastval``. A supplied value is left untouched."""
    if catalog is None or getattr(table, "reflected", False):
        return
    seq_cols = [c for c in table.columns if c.sequence is not None]
    if not seq_cols:
        return
    for doc in docs:
        for col in seq_cols:
            if col.field in doc and doc[col.field] is not None:
                continue
            if not catalog.sequence_exists(db, col.sequence):
                continue  # dropped sequence — leave unset (NOT NULL will catch it)
            value = catalog.sequence_nextval(db, col.sequence)
            doc[col.field] = value
            if session is not None:
                session.record_sequence_value(col.sequence, value)


def _validate_enum_columns(docs: list[dict[str, Any]], table: Any, catalog: Any, db: str) -> None:
    """Every enum-typed column value must be one of the enum's labels (22P02). A
    NULL value is exempt (the NOT NULL check handles required columns)."""
    if catalog is None or getattr(table, "reflected", False):
        return
    enum_cols = [c for c in table.columns if c.enum_type is not None]
    if not enum_cols:
        return
    label_cache: dict[str, set] = {}
    for col in enum_cols:
        if col.enum_type not in label_cache:
            enum = catalog.get_enum(db, col.enum_type)
            label_cache[col.enum_type] = set(enum["labels"]) if enum else set()
    for doc in docs:
        for col in enum_cols:
            value = get_path(doc, col.field)
            if value is None:
                continue
            if value not in label_cache[col.enum_type]:
                raise errors.SQLError(
                    "22P02",
                    f'invalid input value for enum {col.enum_type}: "{value}"',
                )


def _validate_domain_columns(
    docs: list[dict[str, Any]], table: Any, storage: Any, db: str, session: Any, catalog: Any
) -> None:
    """Every domain-typed column value must satisfy the domain's NOT NULL and
    CHECK constraints. NOT NULL → ``23502`` (``domain <name> does not allow null
    values``, matching Postgres' not_null_violation); a CHECK evaluating to FALSE
    → ``23514`` (NULL passes, matching Postgres' three-valued CHECK semantics). The
    CHECK expression references the value via the ``VALUE`` keyword."""
    from secantus.sql import scalar

    if catalog is None or getattr(table, "reflected", False):
        return
    domain_cols = [c for c in table.columns if c.domain_type is not None]
    if not domain_cols:
        return
    cache: dict[str, dict[str, Any] | None] = {}
    for col in domain_cols:
        if col.domain_type not in cache:
            cache[col.domain_type] = catalog.get_domain(db, col.domain_type)
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    for doc in docs:
        for col in domain_cols:
            domain = cache[col.domain_type]
            if domain is None:
                continue
            value = get_path(doc, col.field)
            if value is None:
                if domain.get("not_null"):
                    raise errors.SQLError(
                        "23502", f"domain {col.domain_type} does not allow null values"
                    )
                continue
            for check in domain.get("checks") or []:

                def scope(node: Any, _v: Any = value) -> Any:
                    return _v if node.name.lower() == "value" else None

                result = scalar.evaluate(_parse_check_expr(check["expression"]), scope, ctx)
                if result is not None and not scalar._truthy(result):
                    raise errors.SQLError(
                        "23514",
                        f"value for domain {col.domain_type} violates check "
                        f'constraint "{check["name"]}"',
                    )


def _apply_generated_columns(docs: list[dict[str, Any]], table: Any, ctx: Any) -> None:
    """Compute each ``GENERATED ALWAYS AS (expr) STORED`` column from the row's
    other columns and store the result (runs before validation so NOT NULL / CHECK
    / UNIQUE see the computed value). No-op for reflected tables."""
    from secantus.sql import scalar

    if getattr(table, "reflected", False):
        return
    gen_cols = [c for c in table.columns if c.generated is not None]
    if not gen_cols:
        return
    for doc in docs:

        def scope(node: Any, _doc: Any = doc) -> Any:
            col = table.column(node.name)
            return get_path(_doc, col.field if col is not None else node.name)

        for col in gen_cols:
            value = scalar.evaluate(_parse_check_expr(col.generated), scope, ctx)
            doc[col.field] = typemap.coerce(value, col.type_tag)


def enforce_insert_rows(
    docs: list[dict[str, Any]], table: Any, storage: Any, db: str, catalog: Any, session: Any
) -> None:
    """Enforce every declared constraint against rows about to be inserted."""
    from secantus.sql import scalar

    _apply_generated_columns(
        docs, table, scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    )
    _validate_rows(docs, table, storage, db, session, catalog)
    _validate_enum_columns(docs, table, catalog, db)
    _validate_domain_columns(docs, table, storage, db, session, catalog)
    _validate_unique_rows(docs, table, storage, db, session=session)
    _validate_fk_child_rows(docs, table, storage, db, catalog, session)


def enforce_update_images(
    post_images: list[dict[str, Any]],
    matched_ids: Any,
    table: Any,
    storage: Any,
    db: str,
    catalog: Any,
    session: Any,
) -> None:
    """Enforce constraints against an UPDATE's post-images (UNIQUE probes exclude
    the rows being rewritten). Parent-side FK actions are the caller's job."""
    from secantus.sql import scalar

    if getattr(table, "reflected", False):
        return
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    _apply_generated_columns(post_images, table, ctx)
    for post in post_images:
        _validate_write_row(post, table, ctx)
    _validate_enum_columns(post_images, table, catalog, db)
    _validate_domain_columns(post_images, table, storage, db, session, catalog)
    _validate_unique_rows(
        post_images,
        table,
        storage,
        db,
        exclude_ids=frozenset(_hashable_id(i) for i in matched_ids),
        session=session,
    )
    _validate_fk_child_rows(post_images, table, storage, db, catalog, session)


def enforce_parent_delete(
    victims: list[dict[str, Any]], table: Any, storage: Any, db: str, catalog: Any
) -> None:
    """Apply referential actions before deleting parent rows (RESTRICT / CASCADE /
    SET NULL). Safe to call for any table — a no-op when nothing references it."""
    if catalog is None or getattr(table, "reflected", False):
        return
    _enforce_fk_on_parent_delete(victims, table, storage, db, catalog)


def _hashable_id(value: Any) -> Any:
    """A hashable key for an ``_id`` value. A composite PK's ``_id`` is a
    subdocument (dict) — unhashable — so canonicalize it to a sorted tuple of
    items; a scalar ``_id`` passes through."""
    if isinstance(value, dict):
        return tuple(sorted((k, _hashable_id(v)) for k, v in value.items()))
    if isinstance(value, list):
        return tuple(_hashable_id(v) for v in value)
    return value


def _uq_violation(uq: Any) -> errors.SQLError:
    return errors.unique_violation(f'duplicate key value violates unique constraint "{uq.name}"')


def _maybe_defer(session: Any, kind: str, table_name: str, constraint: Any) -> bool:
    """If ``constraint`` is DEFERRABLE and currently deferred inside an open
    transaction, record a pending re-check on the session and return True (the
    caller skips the immediate raise — the constraint is re-validated at COMMIT or
    ``SET CONSTRAINTS … IMMEDIATE``). Returns False otherwise (raise now)."""
    if session is None or getattr(session, "txn_handle", None) is None:
        return False
    if not getattr(constraint, "deferrable", False):
        return False
    if not session.constraint_is_deferred(constraint.name, constraint.initially_deferred):
        return False
    record = (kind, table_name, constraint.name)
    if record not in session.pending_deferred:
        session.pending_deferred.append(record)
    return True


def _validate_unique_rows(
    docs: list[dict[str, Any]],
    table: Any,
    storage: Any,
    db: str,
    *,
    exclude_ids: frozenset = frozenset(),
    session: Any = None,
) -> None:
    """Enforce declared UNIQUE constraints against a batch of rows (23505). A row
    with any NULL in a constraint's columns is exempt (NULLs are distinct in a
    UNIQUE constraint). Duplicates *within* the batch collide, and each row is
    probed against stored rows (``exclude_ids`` skips the rows an UPDATE is
    rewriting, so a row keeping its own value doesn't conflict with itself). A
    deferred constraint records a pending re-check instead of raising."""
    if getattr(table, "reflected", False) or not table.unique_constraints:
        return
    seen: dict[str, set] = {uq.name: set() for uq in table.unique_constraints}
    for doc in docs:
        for uq in table.unique_constraints:
            fields = [table.field_for(c) for c in uq.columns]
            key = tuple(get_path(doc, f) for f in fields)
            if any(v is None for v in key):
                continue
            violated = key in seen[uq.name]
            if not violated:
                probe = dict(zip(fields, key, strict=True))
                for existing in storage.find_matching(db, table.collection, probe):
                    if _hashable_id(existing.get("_id")) not in exclude_ids:
                        violated = True
                        break
            if violated:
                if _maybe_defer(session, "unique", table.name, uq):
                    continue
                raise _uq_violation(uq)
            seen[uq.name].add(key)


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
        _assign_sequences([doc], plan.table, db, catalog, session)
        if plan.returning is not None:
            doc.setdefault("_id", bson.ObjectId())
        existing = _find_conflict(storage, db, coll, oc, doc)
        if existing is None:
            # Full enforcement — including any UNIQUE / CHECK / FK other than the
            # arbiter target (which _find_conflict already cleared).
            enforce_insert_rows([doc], plan.table, storage, db, catalog, session)
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
    # A composite-PK conflict field is a dotted path (``_id.a``) into the ``_id``
    # subdocument, so probe with has_path / get_path, not flat dict access.
    if not oc.conflict_fields or any(not has_path(doc, f) for f in oc.conflict_fields):
        return None
    found = storage.find_matching(
        db, coll, {f: get_path(doc, f) for f in oc.conflict_fields}, limit=1
    )
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
        return get_path(source, field)  # field may be a dotted composite-PK path

    if oc.where is not None and not scalar._truthy(scalar.evaluate(oc.where, scope, sctx)):
        return None
    set_doc: dict[str, Any] = {}
    for field, type_tag, expr in oc.set_exprs:
        set_doc[field] = typemap.coerce(scalar.evaluate(expr, scope, sctx), type_tag)
    updated = copy.deepcopy(existing)
    updated.update(set_doc)
    # Enforce every constraint on the DO UPDATE post-image (UNIQUE excludes the
    # row itself; NOT NULL / CHECK / FK-child all apply).
    enforce_update_images(
        [updated], [existing["_id"]], table, storage, db, sctx.catalog, sctx.session
    )
    storage.update_matching(db, table.collection, {"_id": existing["_id"]}, {"$set": set_doc})
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
        key_of = _order_key_fn(plan.order, plan.enum_orders)
        _pg_sort(docs, key_of, [(direction, nf) for _, direction, nf in plan.order])
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

    # An enum ORDER BY term sorts by the label's declared ordinal, not lexically.
    enum_ordinals = {
        i: {lbl: k for k, lbl in enumerate(labels)}
        for i, labels in getattr(plan, "enum_orders", {}).items()
    }

    def _order_key(oe: Any, i: int, scope: Any) -> Any:
        v = scalar.evaluate(oe, scope, sctx)
        omap = enum_ordinals.get(i)
        return omap.get(v, len(omap)) if omap is not None and v is not None else v

    scored: list[tuple[tuple[Any, ...], tuple[Any, ...], tuple[Any, ...]]] = []
    for doc in docs:
        scope = make_scope(doc)
        keys = tuple(_order_key(oe, i, scope) for i, (oe, _, _) in enumerate(plan.order))
        # DISTINCT ON key (row-level, evaluated before any SRF expansion).
        don_key = (
            tuple(repr(scalar.evaluate(e, scope, sctx)) for e in plan.distinct_on)
            if plan.distinct_on
            else ()
        )
        for vt in _expand_srf(plan, scope, sctx):
            scored.append((keys, don_key, vt))

    _pg_sort(scored, lambda r: r[0], [(direction, nf) for _, direction, nf in plan.order])

    if plan.distinct_on:
        # Keep the first row (in the sorted order above) per DISTINCT ON key.
        seen_on: set = set()
        rows = []
        for _keys, don_key, vt in scored:
            if don_key not in seen_on:
                seen_on.add(don_key)
                rows.append(vt)
    else:
        rows = [vt for _, _, vt in scored]
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


def _ordered_set_value(kind: str, fraction: float | None, values: Any) -> Any:
    """Compute an ordered-set aggregate from the pushed ORDER BY values (NULLs
    dropped, then sorted ascending):

    - ``percentile_cont(f)``: continuous percentile with linear interpolation
      between the two nearest ranks (``f`` in [0, 1]).
    - ``percentile_disc(f)``: the first value whose cumulative fraction ≥ ``f``.
    - ``mode``: the most frequent value; on a tie, the smallest.

    Returns NULL when the (non-NULL) set is empty."""
    import math

    vals = sorted(v for v in (values or []) if v is not None)
    if not vals:
        return None
    n = len(vals)
    if kind == "mode":
        best_val, best_count = vals[0], 0
        i = 0
        while i < n:
            j = i
            while j < n and vals[j] == vals[i]:
                j += 1
            if j - i > best_count:
                best_val, best_count = vals[i], j - i
            i = j
        return best_val
    assert fraction is not None
    if kind == "percentile_disc":
        # Smallest index whose 1-based position / n ≥ fraction.
        idx = 0 if fraction == 0 else min(math.ceil(fraction * n) - 1, n - 1)
        return vals[max(idx, 0)]
    # percentile_cont: linear interpolation between the neighbouring ranks.
    rank = fraction * (n - 1)
    lo = math.floor(rank)
    hi = math.ceil(rank)
    if lo == hi:
        return float(vals[lo])
    frac = rank - lo
    return float(vals[lo]) + (float(vals[hi]) - float(vals[lo])) * frac


def execute_pipeline_select(
    plan: planner.PipelineSelectPlan, storage: Any, db: str, sctx: Any = None
) -> SQLResult:
    """Run a JOIN / GROUP BY / aggregate SELECT through the aggregation engine."""
    from secantus.aggregate import PipelineContext, apply_pipeline

    _materialize_derived(plan, storage, db, sctx)
    docs, remaining = _pipeline_input_docs(plan, storage, db, sctx)
    ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
    result = apply_pipeline(docs, remaining, ctx)
    if plan.post_aggregates:
        for doc in result:
            for field_name, kind, fraction in plan.post_aggregates:
                doc[field_name] = _ordered_set_value(kind, fraction, doc.get(field_name))
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


def execute_update(
    plan: planner.UpdatePlan, storage: Any, db: str, catalog: Any = None, session: Any = None
) -> SQLResult:
    coll = plan.table.collection
    _validate_update_post_images(plan, storage, db, session, catalog)
    gen_cols = (
        [c for c in plan.table.columns if c.generated is not None]
        if not getattr(plan.table, "reflected", False)
        else []
    )
    # A generated column's value depends on each row's post-image, so the bulk
    # ``$set`` can't carry it; capture the target ids first, then recompute and
    # persist per row after the update lands.
    if plan.returning is not None or gen_cols:
        ids = [d["_id"] for d in storage.find_matching(db, coll, plan.filter)]
    res = storage.update_matching(db, coll, plan.filter, plan.update, multi=True)
    matched = int(res["matched"])
    if gen_cols and ids:
        from secantus.sql import scalar

        ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
        for doc in storage.find_matching(db, coll, {"_id": {"$in": ids}}):
            _apply_generated_columns([doc], plan.table, ctx)
            gset = {c.field: doc[c.field] for c in gen_cols}
            storage.update_matching(db, coll, {"_id": doc["_id"]}, {"$set": gset}, multi=False)
    if plan.returning is not None:
        post = storage.find_matching(db, coll, {"_id": {"$in": ids}}) if ids else []
        return _returning_result(
            post, plan.returning, f"UPDATE {matched}", matched, plan.table, storage, db
        )
    return SQLResult(command_tag=f"UPDATE {matched}", rowcount=matched)


def _validate_update_post_images(
    plan: planner.UpdatePlan, storage: Any, db: str, session: Any, catalog: Any
) -> None:
    """Enforce NOT NULL / CHECK / UNIQUE on an UPDATE's post-image: apply the
    update in memory to each matched row and validate the result before touching
    storage, so a violating UPDATE leaves the table unchanged (Postgres
    statement-atomic). UNIQUE probes exclude every row the statement is rewriting
    (so unchanged rows and value swaps across the matched set don't self-conflict)."""
    from secantus.sql import scalar
    from secantus.update import apply_update

    table = plan.table
    needs = (
        table.check_constraints
        or table.unique_constraints
        or table.foreign_keys
        or _has_not_null(table)
        or any(c.enum_type for c in table.columns)
        or any(c.domain_type for c in table.columns)
        or any(c.generated for c in table.columns)
        or (catalog is not None and _referencing_fks(catalog, db, table.name))
    )
    if getattr(table, "reflected", False) or not needs:
        return
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    matched = storage.find_matching(db, table.collection, plan.filter)
    post_images = [apply_update(doc, plan.update) for doc in matched]
    _apply_generated_columns(post_images, table, ctx)
    for post in post_images:
        _validate_write_row(post, table, ctx)
    _validate_enum_columns(post_images, table, catalog, db)
    _validate_domain_columns(post_images, table, storage, db, session, catalog)
    _validate_unique_rows(
        post_images,
        table,
        storage,
        db,
        exclude_ids=frozenset(d["_id"] for d in matched),
        session=session,
    )
    _validate_fk_child_rows(post_images, table, storage, db, catalog, session)
    _enforce_fk_on_parent_update(matched, post_images, table, storage, db, catalog)


# --------------------------------------------------------------------------- #
# Foreign-key enforcement
# --------------------------------------------------------------------------- #


def _fk_ref_columns(fk: Any, parent: Any) -> list[str]:
    """The parent columns a FK targets — its explicit ``ref_columns`` or, when the
    reference had no column list (``REFERENCES t``), the parent's PRIMARY KEY."""
    if fk.ref_columns:
        return list(fk.ref_columns)
    return [c.name for c in parent.pk_columns] if parent is not None else []


def _parent_probe(fk: Any, parent: Any, values: list[Any]) -> dict[str, Any]:
    ref_cols = _fk_ref_columns(fk, parent)
    return {parent.field_for(rc): v for rc, v in zip(ref_cols, values, strict=False)}


def _validate_fk_child_rows(
    docs: list[dict[str, Any]], table: Any, storage: Any, db: str, catalog: Any, session: Any = None
) -> None:
    """Referential integrity on the child side: every INSERT/UPDATE row whose FK
    columns are all non-NULL must have a matching parent row (23503). MATCH SIMPLE
    — a NULL in any FK column exempts the row. A deferred FK records a pending
    re-check instead of raising."""
    if catalog is None or getattr(table, "reflected", False) or not table.foreign_keys:
        return
    for doc in docs:
        for fk in table.foreign_keys:
            fields = [table.field_for(c) for c in fk.columns]
            values = [get_path(doc, f) for f in fields]
            if any(v is None for v in values):
                continue
            parent = catalog.get(db, fk.ref_table)
            if parent is None:
                continue  # parent table not declared — can't check
            probe = _parent_probe(fk, parent, values)
            if not storage.find_matching(db, parent.collection, probe, limit=1):
                if _maybe_defer(session, "fk", table.name, fk):
                    continue
                raise errors.foreign_key_violation(
                    f'insert or update on table "{table.name}" violates foreign key '
                    f'constraint "{fk.name}"'
                )


def flush_deferred(
    session: Any, storage: Any, db: str, catalog: Any, names: set | None = None
) -> None:
    """Re-validate every constraint whose deferred violation was recorded during
    the transaction, against the current (uncommitted) state. Raises on the first
    that still fails — the caller aborts the transaction. Clears the flushed
    records; ``names`` (when given) restricts the flush to those constraint names,
    leaving the rest pending (for ``SET CONSTRAINTS <name> IMMEDIATE``)."""
    if names is None:
        pending = session.pending_deferred
        session.pending_deferred = []
    else:
        pending = [r for r in session.pending_deferred if r[2] in names]
        session.pending_deferred = [r for r in session.pending_deferred if r[2] not in names]
    for kind, table_name, cname in pending:
        table = catalog.get(db, table_name) if catalog is not None else None
        if table is None:
            continue  # table dropped inside the txn — nothing to re-check
        if kind == "unique":
            _recheck_unique(table, cname, storage, db)
        elif kind == "fk":
            _recheck_fk(table, cname, storage, db, catalog)


def _recheck_unique(table: Any, cname: str, storage: Any, db: str) -> None:
    uq = next((u for u in table.unique_constraints if u.name == cname), None)
    if uq is None:
        return
    fields = [table.field_for(c) for c in uq.columns]
    seen: set = set()
    for doc in storage.find_matching(db, table.collection, {}):
        key = tuple(get_path(doc, f) for f in fields)
        if any(v is None for v in key):
            continue
        if key in seen:
            raise _uq_violation(uq)
        seen.add(key)


def _recheck_fk(table: Any, cname: str, storage: Any, db: str, catalog: Any) -> None:
    fk = next((f for f in table.foreign_keys if f.name == cname), None)
    if fk is None:
        return
    parent = catalog.get(db, fk.ref_table)
    if parent is None:
        return
    fields = [table.field_for(c) for c in fk.columns]
    for doc in storage.find_matching(db, table.collection, {}):
        values = [get_path(doc, f) for f in fields]
        if any(v is None for v in values):
            continue
        probe = _parent_probe(fk, parent, values)
        if not storage.find_matching(db, parent.collection, probe, limit=1):
            raise errors.foreign_key_violation(
                f'insert or update on table "{table.name}" violates foreign key '
                f'constraint "{fk.name}"'
            )


def _referencing_fks(catalog: Any, db: str, parent_name: str) -> list[tuple[Any, Any]]:
    """Every ``(child_table, fk)`` whose FK targets ``parent_name`` — the reverse
    of a table's declared foreign keys, scanned across the catalog."""
    out: list[tuple[Any, Any]] = []
    for tname in catalog.list_tables(db):
        child = catalog.get(db, tname)
        if child is None:
            continue
        for fk in child.foreign_keys:
            if fk.ref_table == parent_name:
                out.append((child, fk))
    return out


def _child_match_filter(child: Any, fk: Any, parent: Any, parent_row: dict[str, Any]) -> Any:
    """A storage filter selecting the child rows that reference ``parent_row``, or
    None when the parent's referenced columns are NULL (nothing references NULL)."""
    ref_cols = _fk_ref_columns(fk, parent)
    filt: dict[str, Any] = {}
    for child_col, ref_col in zip(fk.columns, ref_cols, strict=False):
        val = get_path(parent_row, parent.field_for(ref_col))
        if val is None:
            return None
        filt[child.field_for(child_col)] = val
    return filt


def _enforce_fk_on_parent_delete(
    victims: list[dict[str, Any]], parent: Any, storage: Any, db: str, catalog: Any, _depth: int = 0
) -> None:
    """Apply referential actions when parent rows are deleted: RESTRICT / NO ACTION
    reject if any child references them (23503); CASCADE deletes the children
    (recursively); SET NULL / SET DEFAULT clears the child FK columns."""
    if catalog is None or _depth > 20:
        return
    for child, fk in _referencing_fks(catalog, db, parent.name):
        for parent_row in victims:
            filt = _child_match_filter(child, fk, parent, parent_row)
            if filt is None:
                continue
            children = storage.find_matching(db, child.collection, filt)
            if not children:
                continue
            action = (fk.on_delete or "NO ACTION").upper()
            if action in ("NO ACTION", "RESTRICT"):
                raise errors.foreign_key_violation(
                    f'update or delete on table "{parent.name}" violates foreign key '
                    f'constraint "{fk.name}" on table "{child.name}"'
                )
            if action == "CASCADE":
                _enforce_fk_on_parent_delete(children, child, storage, db, catalog, _depth + 1)
                storage.delete_matching(db, child.collection, filt)
            else:  # SET NULL / SET DEFAULT — clear the child's FK columns
                clear = {child.field_for(c): _fk_clear_value(child, c, action) for c in fk.columns}
                storage.update_matching(db, child.collection, filt, {"$set": clear}, multi=True)


def _fk_clear_value(child: Any, col_name: str, action: str) -> Any:
    if action == "SET DEFAULT":
        col = child.column(col_name)
        if col is not None and col.has_default:
            return col.default
    return None


def _enforce_fk_on_parent_update(
    matched: list[dict[str, Any]],
    post_images: list[dict[str, Any]],
    parent: Any,
    storage: Any,
    db: str,
    catalog: Any,
) -> None:
    """Apply referential actions when an UPDATE changes a parent's referenced
    columns: RESTRICT / NO ACTION reject (23503); CASCADE rewrites the children's
    FK to the new value; SET NULL / SET DEFAULT clears them. Usually a no-op —
    references target the PK (``_id``), which isn't updatable."""
    if catalog is None:
        return
    refs = _referencing_fks(catalog, db, parent.name)
    if not refs:
        return
    for child, fk in refs:
        ref_cols = _fk_ref_columns(fk, parent)
        for pre, post in zip(matched, post_images, strict=False):
            old = [get_path(pre, parent.field_for(rc)) for rc in ref_cols]
            new = [get_path(post, parent.field_for(rc)) for rc in ref_cols]
            if old == new or any(v is None for v in old):
                continue
            filt = {child.field_for(c): v for c, v in zip(fk.columns, old, strict=False)}
            if not storage.find_matching(db, child.collection, filt, limit=1):
                continue
            action = (fk.on_update or "NO ACTION").upper()
            if action in ("NO ACTION", "RESTRICT"):
                raise errors.foreign_key_violation(
                    f'update or delete on table "{parent.name}" violates foreign key '
                    f'constraint "{fk.name}" on table "{child.name}"'
                )
            if action == "CASCADE":
                new_set = {child.field_for(c): v for c, v in zip(fk.columns, new, strict=False)}
                storage.update_matching(db, child.collection, filt, {"$set": new_set}, multi=True)
            else:
                clear = {child.field_for(c): _fk_clear_value(child, c, action) for c in fk.columns}
                storage.update_matching(db, child.collection, filt, {"$set": clear}, multi=True)


def execute_delete(
    plan: planner.DeletePlan, storage: Any, db: str, catalog: Any = None, session: Any = None
) -> SQLResult:
    coll = plan.table.collection
    # RETURNING yields the deleted rows, so snapshot them before the delete. FK
    # enforcement also needs the victims, so read them whenever either applies.
    enforce_fk = (
        catalog is not None
        and not getattr(plan.table, "reflected", False)
        and bool(_referencing_fks(catalog, db, plan.table.name))
    )
    victims = (
        storage.find_matching(db, coll, plan.filter)
        if (plan.returning is not None or enforce_fk)
        else []
    )
    if enforce_fk:
        _enforce_fk_on_parent_delete(victims, plan.table, storage, db, catalog)
    n = storage.delete_matching(db, coll, plan.filter)
    if plan.returning is not None:
        return _returning_result(victims, plan.returning, f"DELETE {n}", n, plan.table, storage, db)
    return SQLResult(command_tag=f"DELETE {n}", rowcount=n)
