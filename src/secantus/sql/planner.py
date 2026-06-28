"""Translate a parsed SQL statement into operations over the Mongo engines.

This is the heart of the spike: it walks a ``sqlglot`` AST and lowers it to the
exact structures ``Storage`` already consumes — a ``query``-style filter dict, a
sort spec, an ``update``-style ``$set`` document, or a list of documents to
insert. The executor then just hands those to ``Storage``. Because WHERE becomes
a real Mongo filter, SQL inherits the storage layer's index acceleration and
matching semantics with no separate execution engine.

Only the P0 subset is handled; anything outside it raises a
``feature_not_supported`` SQLError rather than silently diverging.
"""

from __future__ import annotations

import logging
import re
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

import sqlglot
from sqlglot import exp

from secantus.sql import errors, typemap
from secantus.sql.catalog import Column, TableDef

# sqlglot logs a WARNING when it falls back to parsing ``SHOW`` / ``RESET`` as a
# generic ``Command`` node — which is exactly how we consume them. Quiet it so
# the server log isn't spammed for statements we handle on purpose.
logging.getLogger("sqlglot").setLevel(logging.ERROR)

# ---------------------------------------------------------------------------
# Plan objects — ready-to-execute structures over Storage.
# ---------------------------------------------------------------------------


@dataclass
class CreateTablePlan:
    table: TableDef
    if_not_exists: bool


@dataclass
class DropTablePlan:
    name: str
    if_exists: bool


@dataclass
class InsertPlan:
    table: TableDef
    docs: list[dict[str, Any]]


@dataclass
class ConstantSelectPlan:
    # A FROM-less ``SELECT <literals>`` — one row, no storage access. The
    # headline P1 case (``SELECT 1``) and the seed for ``SELECT version()`` etc.
    columns: list[tuple[str, str, Any]]  # (out_name, type_tag, python_value)


@dataclass
class SelectPlan:
    table: TableDef
    filter: dict[str, Any]
    sort: dict[str, int] | None
    limit: int
    skip: int
    out_columns: list[tuple[str, Column]] = field(default_factory=list)
    count_star: bool = False
    count_alias: str = "count"


@dataclass
class UpdatePlan:
    table: TableDef
    filter: dict[str, Any]
    update: dict[str, Any]


@dataclass
class DeletePlan:
    table: TableDef
    filter: dict[str, Any]


Plan = CreateTablePlan | DropTablePlan | InsertPlan | SelectPlan | UpdatePlan | DeletePlan


# ---------------------------------------------------------------------------
# Literal / column extraction
# ---------------------------------------------------------------------------


def _literal(node: exp.Expression) -> Any:
    """Extract a Python value from a literal-ish AST node."""
    if isinstance(node, exp.Paren):
        return _literal(node.this)
    if isinstance(node, exp.Neg):
        return -_literal(node.this)
    if isinstance(node, exp.Null):
        return None
    if isinstance(node, exp.Boolean):
        return bool(node.this)
    if isinstance(node, exp.Literal):
        if node.is_string:
            return node.this
        text = node.this
        return float(text) if ("." in text or "e" in text.lower()) else int(text)
    raise errors.feature_not_supported(f"unsupported value expression: {node.sql()}")


def _column_name(node: exp.Expression) -> str:
    if isinstance(node, exp.Column):
        return node.name
    if isinstance(node, exp.Identifier):
        return node.name
    raise errors.feature_not_supported(f"expected a column, got: {node.sql()}")


# ---------------------------------------------------------------------------
# WHERE -> Mongo filter
# ---------------------------------------------------------------------------

_CMP_OPS: dict[type, tuple[str, str]] = {
    # exp class -> (operator, operator-when-column-is-on-the-right)
    exp.GT: ("$gt", "$lt"),
    exp.GTE: ("$gte", "$lte"),
    exp.LT: ("$lt", "$gt"),
    exp.LTE: ("$lte", "$gte"),
}


def _like_to_regex(pattern: str) -> str:
    """Translate a SQL LIKE pattern to an anchored regex.

    ``%`` -> ``.*`` and ``_`` -> ``.``; every other character is escaped so it
    matches literally.
    """
    out = ["^"]
    for ch in pattern:
        if ch == "%":
            out.append(".*")
        elif ch == "_":
            out.append(".")
        else:
            out.append(re.escape(ch))
    out.append("$")
    return "".join(out)


# A resolver maps a column AST node to (mongo_field_path, type_tag). The
# single-table path ignores any table qualifier; the join path uses it to route
# ``alias.column`` to the right side of the lookup.
Resolve = Callable[[exp.Expression], tuple[str, str]]


def table_resolver(table: TableDef) -> Resolve:
    def resolve(node: exp.Expression) -> tuple[str, str]:
        col = _column_name(node)
        return table.field_for(col), table.type_for(col)

    return resolve


# jsonb navigation: ->, ->>, #>, #>> parse to these nodes. ``Scalar`` variants
# (->> / #>>) return text; the others return jsonb.
_JSONB_CLASSES = (exp.JSONExtract, exp.JSONExtractScalar, exp.JSONBExtract, exp.JSONBExtractScalar)
_JSONB_SCALAR = (exp.JSONExtractScalar, exp.JSONBExtractScalar)


def _is_field_node(node: exp.Expression) -> bool:
    return isinstance(node, (exp.Column, *_JSONB_CLASSES))


def _json_keys(expr: exp.Expression) -> list[str]:
    """Extract the path keys from a ->/#> right-hand side."""
    if isinstance(expr, exp.JSONPath):
        return [p.this for p in expr.expressions if isinstance(p, exp.JSONPathKey)]
    if isinstance(expr, exp.Literal):
        # ``#> '{a,b}'`` — a Postgres text[] path literal.
        return [k for k in str(expr.this).strip("{}").split(",") if k]
    raise errors.feature_not_supported(f"unsupported jsonb path: {expr.sql()}")


def _field(node: exp.Expression, resolve: Resolve) -> tuple[str, str]:
    """Resolve a column or jsonb-path node to (dotted_field_path, type_tag)."""
    if isinstance(node, exp.Column):
        return resolve(node)
    if isinstance(node, _JSONB_CLASSES):
        base_path, _ = _field(node.this, resolve)
        keys = _json_keys(node.expression)
        path = base_path + ("." + ".".join(keys) if keys else "")
        return path, ("text" if isinstance(node, _JSONB_SCALAR) else "json")
    raise errors.feature_not_supported(f"expected a column or jsonb path: {node.sql()}")


def _comparison(node: exp.Expression) -> tuple[exp.Expression, exp.Expression]:
    """Return (field_node, other_node), normalising the field-on-the-right case."""
    left, right = node.this, node.expression
    if _is_field_node(left):
        return left, right
    if _is_field_node(right):
        return right, left
    raise errors.feature_not_supported(f"comparison needs a column: {node.sql()}")


def _expr_to_filter(node: exp.Expression, resolve: Resolve) -> dict[str, Any]:
    if isinstance(node, exp.Paren):
        return _expr_to_filter(node.this, resolve)

    if isinstance(node, exp.And):
        parts = [_expr_to_filter(node.this, resolve), _expr_to_filter(node.expression, resolve)]
        return _merge_and(parts)

    if isinstance(node, exp.Or):
        return {
            "$or": [_expr_to_filter(node.this, resolve), _expr_to_filter(node.expression, resolve)]
        }

    if isinstance(node, exp.Not):
        inner = node.this
        # IS NOT NULL parses as Not(Is(col, Null)).
        if isinstance(inner, exp.Is) and isinstance(inner.expression, exp.Null):
            field, _ = _field(inner.this, resolve)
            return {field: {"$ne": None}}
        return {"$nor": [_expr_to_filter(inner, resolve)]}

    if isinstance(node, exp.Is):
        field, _ = _field(node.this, resolve)
        if isinstance(node.expression, exp.Null):
            return {field: None}
        raise errors.feature_not_supported(f"unsupported IS predicate: {node.sql()}")

    if isinstance(node, exp.EQ):
        col_node, other = _comparison(node)
        field, tag = _field(col_node, resolve)
        return {field: typemap.coerce(_literal(other), tag)}

    if isinstance(node, exp.NEQ):
        col_node, other = _comparison(node)
        field, tag = _field(col_node, resolve)
        return {field: {"$ne": typemap.coerce(_literal(other), tag)}}

    for cls, (op, flipped) in _CMP_OPS.items():
        if isinstance(node, cls):
            col_node, other = _comparison(node)
            field, tag = _field(col_node, resolve)
            use = op if _is_field_node(node.this) else flipped
            return {field: {use: typemap.coerce(_literal(other), tag)}}

    if isinstance(node, exp.In):
        if node.args.get("query") is not None:
            raise errors.feature_not_supported("IN (subquery) is not supported")
        field, tag = _field(node.this, resolve)
        values = [typemap.coerce(_literal(e), tag) for e in node.expressions]
        return {field: {"$in": values}}

    if isinstance(node, exp.Between):
        field, tag = _field(node.this, resolve)
        low = typemap.coerce(_literal(node.args["low"]), tag)
        high = typemap.coerce(_literal(node.args["high"]), tag)
        return {field: {"$gte": low, "$lte": high}}

    if isinstance(node, (exp.Like, exp.ILike)):
        field, _ = _field(node.this, resolve)
        pattern = _literal(node.expression)
        spec: dict[str, Any] = {"$regex": _like_to_regex(str(pattern))}
        if isinstance(node, exp.ILike):
            spec["$options"] = "i"
        return {field: spec}

    raise errors.feature_not_supported(f"unsupported WHERE clause: {node.sql()}")


def _merge_and(parts: list[dict[str, Any]]) -> dict[str, Any]:
    """Merge conjuncts into one dict when their field keys are disjoint, else $and.

    Merging keeps simple ``a = 1 AND b = 2`` queries as ``{a: 1, b: 2}`` so the
    storage layer's compound-index planning can see them; colliding keys fall
    back to an explicit ``$and`` which the query engine also handles.
    """
    merged: dict[str, Any] = {}
    for part in parts:
        if any(k in merged or k.startswith("$") for k in part):
            return {"$and": parts}
        merged.update(part)
    return merged


def _where_filter(stmt: exp.Expression, table: TableDef) -> dict[str, Any]:
    where = stmt.args.get("where")
    return _expr_to_filter(where.this, table_resolver(table)) if where is not None else {}


# ---------------------------------------------------------------------------
# Statement planners
# ---------------------------------------------------------------------------


def plan_create_table(stmt: exp.Create) -> CreateTablePlan:
    schema = stmt.this
    if not isinstance(schema, exp.Schema):
        raise errors.feature_not_supported("CREATE TABLE requires a column list")
    table_name = schema.this.name
    columns: list[Column] = []
    pk_seen = False
    for coldef in schema.expressions:
        if isinstance(coldef, exp.PrimaryKey):
            # Table-level PRIMARY KEY (col) — mark the named column.
            names = [_column_name(c) for c in coldef.expressions]
            if len(names) != 1:
                raise errors.feature_not_supported("composite primary keys are not supported")
            columns = [_with_pk(c, names[0]) for c in columns]
            pk_seen = True
            continue
        if not isinstance(coldef, exp.ColumnDef):
            raise errors.feature_not_supported(f"unsupported table element: {coldef.sql()}")
        tag = typemap.type_tag_for_sql(coldef.args["kind"])
        if tag is None:
            raise errors.feature_not_supported(
                f"unsupported column type for {coldef.name}: {coldef.args['kind'].sql()}"
            )
        constraints = [type(c.kind).__name__ for c in (coldef.args.get("constraints") or [])]
        is_pk = "PrimaryKeyColumnConstraint" in constraints
        nullable = not is_pk and "NotNullColumnConstraint" not in constraints
        if is_pk:
            pk_seen = True
        columns.append(
            Column(
                name=coldef.name,
                type_tag=tag,
                field="_id" if is_pk else coldef.name,
                pk=is_pk,
                nullable=nullable,
            )
        )
    if not pk_seen:
        # No PK: the _id is auto-assigned by storage and not surfaced as a
        # column. Fine for the spike.
        pass
    table = TableDef(name=table_name, collection=table_name, columns=columns)
    return CreateTablePlan(table=table, if_not_exists=bool(stmt.args.get("exists")))


def _with_pk(col: Column, pk_name: str) -> Column:
    if col.name != pk_name:
        return col
    return Column(name=col.name, type_tag=col.type_tag, field="_id", pk=True, nullable=False)


def plan_drop_table(stmt: exp.Drop) -> DropTablePlan:
    return DropTablePlan(name=stmt.this.name, if_exists=bool(stmt.args.get("exists")))


def plan_insert(stmt: exp.Insert, table: TableDef) -> InsertPlan:
    schema = stmt.this
    if isinstance(schema, exp.Schema):
        col_names = [_column_name(c) for c in schema.expressions]
    else:
        col_names = [c.name for c in table.columns]
    values = stmt.expression
    if not isinstance(values, exp.Values):
        raise errors.feature_not_supported("INSERT requires a VALUES clause")
    docs: list[dict[str, Any]] = []
    for tup in values.expressions:
        cells = tup.expressions
        if len(cells) != len(col_names):
            raise errors.syntax_error(
                f"INSERT has {len(cells)} values but {len(col_names)} columns"
            )
        doc: dict[str, Any] = {}
        provided = set()
        for name, cell in zip(col_names, cells, strict=True):
            col = table.column(name)
            if col is None:
                raise errors.undefined_column(name)
            raw = _literal(cell)
            if raw is None and not col.nullable:
                raise errors.not_null_violation(name)
            doc[col.field] = typemap.coerce(raw, col.type_tag)
            provided.add(name)
        # NOT NULL columns omitted entirely are a violation (no DEFAULT support).
        for col in table.columns:
            if col.name not in provided and not col.nullable:
                raise errors.not_null_violation(col.name)
        docs.append(doc)
    return InsertPlan(table=table, docs=docs)


def _order_sort(stmt: exp.Expression, table: TableDef) -> dict[str, int] | None:
    order = stmt.args.get("order")
    if order is None:
        return None
    sort: dict[str, int] = {}
    for ordered in order.expressions:
        col = _column_name(ordered.this)
        sort[table.field_for(col)] = -1 if ordered.args.get("desc") else 1
    return sort


def _limit_skip(stmt: exp.Expression) -> tuple[int, int]:
    limit_node = stmt.args.get("limit")
    offset_node = stmt.args.get("offset")
    limit = int(_literal(limit_node.expression)) if limit_node is not None else 0
    skip = int(_literal(offset_node.expression)) if offset_node is not None else 0
    return limit, skip


def _infer_value_tag(value: Any) -> str:
    if value is None:
        return "text"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, int):
        return "int4" if -(2**31) <= value < 2**31 else "int8"
    if isinstance(value, float):
        return "float8"
    return "text"


_LITERAL_NODES = (exp.Literal, exp.Boolean, exp.Null, exp.Neg, exp.Paren)


def plan_constant_select(stmt: exp.Select, session: Any) -> ConstantSelectPlan:
    """Plan a FROM-less ``SELECT <literal | function>, ...`` into one row.

    Literals are read directly; session/info functions (``version()``,
    ``current_database()``, ``current_setting(...)``, ...) resolve against the
    connection ``session``.
    """
    from secantus.sql import functions

    if stmt.args.get("where") or stmt.args.get("group") or stmt.args.get("joins"):
        raise errors.feature_not_supported("FROM-less SELECT supports only constant projections")
    columns: list[tuple[str, str, Any]] = []
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, _LITERAL_NODES):
            value = _literal(target)
            columns.append((alias or "?column?", _infer_value_tag(value), value))
        elif functions.is_scalar_function(target):
            fname, value, tag = functions.evaluate_scalar(target, session)
            columns.append((alias or fname, tag, value))
        else:
            raise errors.feature_not_supported(f"unsupported FROM-less projection: {target.sql()}")
    return ConstantSelectPlan(columns=columns)


def plan_select(stmt: exp.Select, table: TableDef) -> SelectPlan:
    if stmt.args.get("joins"):
        raise errors.feature_not_supported("JOIN is not supported yet")
    if stmt.args.get("group") or stmt.args.get("having"):
        raise errors.feature_not_supported("GROUP BY / HAVING is not supported yet")
    if stmt.args.get("distinct"):
        raise errors.feature_not_supported("SELECT DISTINCT is not supported yet")

    filt = _where_filter(stmt, table)
    sort = _order_sort(stmt, table)
    limit, skip = _limit_skip(stmt)

    exprs = stmt.expressions
    # COUNT(*) as the sole projection (no GROUP BY) -> whole-table count.
    if len(exprs) == 1 and isinstance(exprs[0], (exp.Count, exp.Alias)):
        inner = exprs[0].this if isinstance(exprs[0], exp.Alias) else exprs[0]
        if isinstance(inner, exp.Count) and isinstance(inner.this, exp.Star):
            alias = exprs[0].alias if isinstance(exprs[0], exp.Alias) else "count"
            return SelectPlan(
                table=table,
                filter=filt,
                sort=sort,
                limit=limit,
                skip=skip,
                count_star=True,
                count_alias=alias or "count",
            )

    out_columns: list[tuple[str, Column]] = []
    for e in exprs:
        if isinstance(e, exp.Star):
            for col in table.columns:
                out_columns.append((col.name, col))
            continue
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, _JSONB_CLASSES):
            # jsonb navigation (doc->>'k', doc->'a'->>'b', doc #> '{a,b}') reads a
            # dotted path; surface it as a synthetic column.
            path, tag = _field(target, table_resolver(table))
            out_name = alias or "?column?"
            out_columns.append((out_name, Column(out_name, tag, path, pk=False, nullable=True)))
            continue
        cname = _column_name(target)
        col = table.column(cname)
        if col is None:
            if table.reflected:
                # Schema-on-read: any selected field is valid on a reflected table.
                col = Column(cname, "any", cname, pk=False, nullable=True)
            else:
                raise errors.undefined_column(cname)
        out_columns.append((alias or cname, col))
    return SelectPlan(
        table=table, filter=filt, sort=sort, limit=limit, skip=skip, out_columns=out_columns
    )


def plan_update(stmt: exp.Update, table: TableDef) -> UpdatePlan:
    set_doc: dict[str, Any] = {}
    for assign in stmt.expressions:
        if not isinstance(assign, exp.EQ):
            raise errors.feature_not_supported(f"unsupported SET item: {assign.sql()}")
        col_name = _column_name(assign.this)
        col = table.column(col_name)
        if col is None:
            raise errors.undefined_column(col_name)
        if col.pk:
            raise errors.feature_not_supported("updating the primary key is not supported")
        raw = _literal(assign.expression)
        if raw is None and not col.nullable:
            raise errors.not_null_violation(col_name)
        set_doc[col.field] = typemap.coerce(raw, col.type_tag)
    return UpdatePlan(table=table, filter=_where_filter(stmt, table), update={"$set": set_doc})


def plan_delete(stmt: exp.Delete, table: TableDef) -> DeletePlan:
    return DeletePlan(table=table, filter=_where_filter(stmt, table))


def _value_to_node(value: Any) -> exp.Expression:
    if value is None:
        return exp.Null()
    if isinstance(value, bool):
        return exp.Boolean(this=value)
    if isinstance(value, (int, float)):
        return exp.Literal.number(repr(value))
    return exp.Literal.string(str(value))


def substitute_parameters(stmt: exp.Expression, values: list[Any]) -> exp.Expression:
    """Replace ``$1`` / ``$2`` ... placeholders with bound literal nodes.

    Bound values arrive as Python scalars (text params decode to ``str``); the
    column-type coercion in the planner then converts them to the right BSON
    type, so a text ``"5"`` bound into an ``int8`` column lands as ``Int64(5)``.
    """
    stmt = stmt.copy()
    for param in list(stmt.find_all(exp.Parameter)):
        try:
            idx = int(param.name) - 1
        except (TypeError, ValueError) as exc:
            raise errors.syntax_error(f"invalid bind parameter ${param.name}") from exc
        if idx < 0 or idx >= len(values):
            raise errors.syntax_error(f"bind parameter ${param.name} has no value")
        param.replace(_value_to_node(values[idx]))
    return stmt


def parameter_count(stmt: exp.Expression) -> int:
    """Highest ``$N`` index referenced by ``stmt`` (0 if none)."""
    indices = []
    for param in stmt.find_all(exp.Parameter):
        try:
            indices.append(int(param.name))
        except (TypeError, ValueError):
            continue
    return max(indices, default=0)


# ---------------------------------------------------------------------------
# Pipeline path: JOIN / GROUP BY / aggregates -> an aggregation pipeline
# ---------------------------------------------------------------------------


@dataclass
class PipelineSelectPlan:
    base_collection: str
    base_filter: dict[str, Any]
    pipeline: list[dict[str, Any]]
    out_columns: list[tuple[str, str]]  # (output_name, type_tag)


_AGG_CLASSES: dict[type, str] = {
    exp.Count: "count",
    exp.Sum: "sum",
    exp.Avg: "avg",
    exp.Min: "min",
    exp.Max: "max",
}

_HAVING_CMP: dict[type, tuple[str, str]] = {
    exp.GT: ("$gt", "$lt"),
    exp.GTE: ("$gte", "$lte"),
    exp.LT: ("$lt", "$gt"),
    exp.LTE: ("$lte", "$gte"),
}


def _aggregate_of(node: exp.Expression) -> tuple[str, str | None] | None:
    """If ``node`` (or its alias target) is an aggregate, return (func, column)."""
    inner = node.this if isinstance(node, exp.Alias) else node
    for cls, name in _AGG_CLASSES.items():
        if isinstance(inner, cls):
            arg = inner.this
            col = _column_name(arg) if isinstance(arg, exp.Column) else None
            return name, col
    return None


def select_needs_pipeline(stmt: exp.Select) -> bool:
    """Whether a SELECT must be compiled to an aggregation pipeline."""
    if stmt.args.get("joins") or stmt.args.get("group") or stmt.args.get("having"):
        return True
    aggs = [e for e in stmt.expressions if _aggregate_of(e) is not None]
    if not aggs:
        return False
    # A lone COUNT(*) (no GROUP BY) is served by the simpler find path.
    if len(stmt.expressions) == 1:
        only = _aggregate_of(stmt.expressions[0])
        if only is not None and only == ("count", None):
            return False
    return True


def plan_pipeline_select(stmt: exp.Select, db: str, catalog: Any) -> PipelineSelectPlan:
    if stmt.args.get("distinct"):
        raise errors.feature_not_supported("SELECT DISTINCT is not supported yet")
    if stmt.args.get("joins"):
        return _plan_join_select(stmt, db, catalog)
    table_node = stmt.find(exp.Table)
    if table_node is None:
        raise errors.feature_not_supported("aggregate without FROM is not supported")
    table = catalog.get(db, table_node.name)
    if table is None:
        raise errors.undefined_table(table_node.name)
    return _plan_group_select(stmt, table)


def _accumulator(func: str, col: str | None, table: TableDef) -> tuple[dict[str, Any], str]:
    if func == "count":
        if col is None:
            return {"$sum": 1}, "int8"
        field = table.field_for(col)
        # COUNT(col) counts non-null values.
        return {"$sum": {"$cond": [{"$ne": [f"${field}", None]}, 1, 0]}}, "int8"
    field = table.field_for(col)
    tag = table.type_for(col)
    if func == "sum":
        return {"$sum": f"${field}"}, (
            tag if tag in ("int4", "int8", "numeric", "float8") else "float8"
        )
    if func == "avg":
        return {"$avg": f"${field}"}, "float8"
    if func == "min":
        return {"$min": f"${field}"}, tag
    if func == "max":
        return {"$max": f"${field}"}, tag
    raise errors.feature_not_supported(f"aggregate {func} is not supported")


class _NameAllocator:
    def __init__(self) -> None:
        self._used: set[str] = set()

    def fresh(self, name: str) -> str:
        base, i = name, 1
        while name in self._used:
            i += 1
            name = f"{base}_{i}"
        self._used.add(name)
        return name


def _plan_group_select(stmt: exp.Select, table: TableDef) -> PipelineSelectPlan:
    base_filter = _where_filter(stmt, table)
    group_node = stmt.args.get("group")
    group_cols = [_column_name(c) for c in group_node.expressions] if group_node else []
    for c in group_cols:
        table.field_for(c)  # validate
    group_id = {c: f"${table.field_for(c)}" for c in group_cols} or None

    accumulators: dict[str, Any] = {}
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    agg_fields: dict[tuple[str, str | None], str] = {}

    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        agg = _aggregate_of(e)
        if agg is not None:
            func, col = agg
            acc, tag = _accumulator(func, col, table)
            fname = names.fresh(alias or func)
            accumulators[fname] = acc
            agg_fields[(func, col)] = fname
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        else:
            inner = e.this if isinstance(e, exp.Alias) else e
            if isinstance(inner, exp.Star):
                raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
            col = _column_name(inner)
            if col not in group_cols:
                raise errors.SQLError(
                    "42803",
                    f'column "{col}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            out_name = names.fresh(alias or col)
            project[out_name] = f"$_id.{col}"
            out_columns.append((out_name, table.type_for(col)))

    # Resolve HAVING first — it may register hidden accumulators that must be
    # present in the $group stage built below.
    having = stmt.args.get("having")
    having_match = (
        _having_to_match(having.this, table, accumulators, agg_fields, group_cols)
        if having is not None
        else None
    )
    pipeline: list[dict[str, Any]] = [{"$group": {"_id": group_id, **accumulators}}]
    if having_match is not None:
        pipeline.append({"$match": having_match})
    pipeline.append({"$project": project})
    _append_sort_limit(pipeline, stmt, {n for n, _ in out_columns})
    return PipelineSelectPlan(table.collection, base_filter, pipeline, out_columns)


def _having_to_match(
    node: exp.Expression,
    table: TableDef,
    accumulators: dict[str, Any],
    agg_fields: dict[tuple[str, str | None], str],
    group_cols: list[str],
) -> dict[str, Any]:
    if isinstance(node, exp.Paren):
        return _having_to_match(node.this, table, accumulators, agg_fields, group_cols)
    if isinstance(node, exp.And):
        left = _having_to_match(node.this, table, accumulators, agg_fields, group_cols)
        right = _having_to_match(node.expression, table, accumulators, agg_fields, group_cols)
        return _merge_and([left, right])
    if isinstance(node, exp.Or):
        return {
            "$or": [
                _having_to_match(node.this, table, accumulators, agg_fields, group_cols),
                _having_to_match(node.expression, table, accumulators, agg_fields, group_cols),
            ]
        }

    def field_tag(term: exp.Expression) -> tuple[str, str]:
        if isinstance(term, exp.Column):
            col = _column_name(term)
            if col not in group_cols:
                raise errors.SQLError(
                    "42803",
                    f'column "{col}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            return f"_id.{col}", table.type_for(col)
        agg = _aggregate_of(term)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported HAVING term: {term.sql()}")
        acc, tag = _accumulator(agg[0], agg[1], table)
        if agg not in agg_fields:
            fname = f"__having_{len(agg_fields)}"
            accumulators[fname] = acc
            agg_fields[agg] = fname
        return agg_fields[agg], tag

    if isinstance(node, (exp.EQ, exp.NEQ)) or type(node) in _HAVING_CMP:
        left, right = node.this, node.expression
        term, lit, on_left = (left, right, True)
        if not isinstance(left, (exp.Column, *(_AGG_CLASSES.keys()))):
            term, lit, on_left = right, left, False
        field, tag = field_tag(term)
        value = typemap.coerce(_literal(lit), tag)
        if isinstance(node, exp.EQ):
            return {field: value}
        if isinstance(node, exp.NEQ):
            return {field: {"$ne": value}}
        op, flipped = _HAVING_CMP[type(node)]
        return {field: {(op if on_left else flipped): value}}

    raise errors.feature_not_supported(f"unsupported HAVING clause: {node.sql()}")


def _join_resolver(amap: dict[str, tuple[str, TableDef]]) -> Resolve:
    def resolve(node: exp.Expression) -> tuple[str, str]:
        if not isinstance(node, exp.Column):
            raise errors.feature_not_supported(f"expected a column: {node.sql()}")
        alias = node.table or None
        name = node.name
        if alias:
            if alias not in amap:
                raise errors.SQLError("42P01", f'missing FROM-clause entry for table "{alias}"')
            role, tdef = amap[alias]
        else:
            cands = [(a, v) for a, v in amap.items() if v[1].column(name) is not None]
            if not cands:
                raise errors.undefined_column(name)
            if len(cands) > 1:
                raise errors.SQLError("42702", f'column reference "{name}" is ambiguous')
            alias, (role, tdef) = cands[0][0], cands[0][1]
        path = tdef.field_for(name)
        if role != "base":
            path = f"{alias}.{path}"
        return path, tdef.type_for(name)

    return resolve


def _alias_col(node: exp.Expression) -> tuple[str | None, str]:
    if not isinstance(node, exp.Column):
        raise errors.feature_not_supported(f"ON must compare columns: {node.sql()}")
    return (node.table or None), node.name


def _plan_join_select(stmt: exp.Select, db: str, catalog: Any) -> PipelineSelectPlan:
    if (
        stmt.args.get("group")
        or stmt.args.get("having")
        or any(_aggregate_of(e) is not None for e in stmt.expressions)
    ):
        raise errors.feature_not_supported("JOIN with GROUP BY / aggregates is not supported yet")
    fr = stmt.find(exp.From).this
    base = catalog.get(db, fr.name)
    if base is None:
        raise errors.undefined_table(fr.name)
    base_alias = fr.alias or fr.name
    joins = stmt.args["joins"]
    if len(joins) != 1:
        raise errors.feature_not_supported("only a single JOIN is supported")
    jn = joins[0]
    jt = jn.this
    join_table = catalog.get(db, jt.name)
    if join_table is None:
        raise errors.undefined_table(jt.name)
    join_alias = jt.alias or jt.name
    side = str(jn.args.get("side") or "").upper()
    on = jn.args.get("on")
    if not isinstance(on, exp.EQ):
        raise errors.feature_not_supported("only a single equality ON condition is supported")

    amap: dict[str, tuple[str, TableDef]] = {
        base_alias: ("base", base),
        join_alias: ("join", join_table),
    }
    la, lc = _alias_col(on.this)
    ra, rc = _alias_col(on.expression)
    if la is None or ra is None or {la, ra} != {base_alias, join_alias}:
        raise errors.feature_not_supported("ON must reference both joined tables by alias")
    if la == base_alias:
        local_field, foreign_field = base.field_for(lc), join_table.field_for(rc)
    else:
        local_field, foreign_field = base.field_for(rc), join_table.field_for(lc)

    resolve = _join_resolver(amap)
    pipeline: list[dict[str, Any]] = [
        {
            "$lookup": {
                "from": join_table.collection,
                "localField": local_field,
                "foreignField": foreign_field,
                "as": join_alias,
            }
        },
        {"$unwind": {"path": f"${join_alias}", "preserveNullAndEmptyArrays": side == "LEFT"}},
    ]
    where = stmt.args.get("where")
    if where is not None:
        pipeline.append({"$match": _expr_to_filter(where.this, resolve)})

    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for a, (role, tdef) in amap.items():
                for c in tdef.columns:
                    name = names.fresh(c.name)
                    project[name] = f"${c.field if role == 'base' else f'{a}.{c.field}'}"
                    out_columns.append((name, c.type_tag))
            continue
        path, tag = resolve(inner)
        name = names.fresh(alias or _column_name(inner))
        project[name] = f"${path}"
        out_columns.append((name, tag))
    pipeline.append({"$project": project})
    _append_sort_limit(pipeline, stmt, {n for n, _ in out_columns})
    return PipelineSelectPlan(base.collection, {}, pipeline, out_columns)


def _append_sort_limit(
    pipeline: list[dict[str, Any]], stmt: exp.Expression, valid_names: set[str]
) -> None:
    order = stmt.args.get("order")
    if order is not None:
        sort: dict[str, int] = {}
        for o in order.expressions:
            col = _column_name(o.this)
            if col not in valid_names:
                raise errors.undefined_column(col)
            sort[col] = -1 if o.args.get("desc") else 1
        pipeline.append({"$sort": sort})
    limit, skip = _limit_skip(stmt)
    if skip:
        pipeline.append({"$skip": skip})
    if limit:
        pipeline.append({"$limit": limit})


def parse(sql: str) -> list[exp.Expression]:
    """Parse a (possibly multi-statement) SQL string into AST statements."""
    try:
        return [s for s in sqlglot.parse(sql, read="postgres") if s is not None]
    except sqlglot.errors.ParseError as exc:
        raise errors.syntax_error(str(exc).splitlines()[0]) from exc
