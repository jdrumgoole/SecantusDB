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

import contextvars
import json
import logging
import re
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

import sqlglot
from sqlglot import exp

from secantus.sql import errors, typemap
from secantus.sql.catalog import Column, ForeignKey, TableDef

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
class AlterTablePlan:
    name: str
    if_exists: bool
    actions: list[Any]  # the raw sqlglot action nodes, applied by the executor


@dataclass
class CreateIndexPlan:
    collection: str
    name: str
    key_spec: dict[str, int]
    unique: bool
    if_not_exists: bool


@dataclass
class DropIndexPlan:
    name: str
    if_exists: bool


@dataclass
class OnConflict:
    """An ``ON CONFLICT`` clause lowered for execution.

    ``conflict_fields`` are the storage field names of the conflict target
    (``ON CONFLICT (a, b)``); empty when none was given — a bare
    ``ON CONFLICT DO NOTHING`` matches *any* unique conflict, which the executor
    handles by inserting and swallowing the duplicate-key error. For
    ``action == "update"``, ``set_exprs`` is the list of
    ``(field, type_tag, raw expr)`` SET assignments — evaluated per conflicting
    row with ``EXCLUDED`` bound to the proposed insert row and the target table
    bound to the existing row — and ``where`` is an optional predicate that gates
    the update."""

    action: str  # "nothing" | "update"
    conflict_fields: list[str]
    set_exprs: list[tuple[str, str, Any]] = field(default_factory=list)
    where: Any = None


@dataclass
class InsertPlan:
    table: TableDef
    docs: list[dict[str, Any]]
    returning: list[tuple[str, Column, Any]] | None = None
    on_conflict: OnConflict | None = None


@dataclass
class ConstantSelectPlan:
    # A FROM-less ``SELECT <expr>, ...`` — no storage access. The headline P1 case
    # (``SELECT 1``), ``SELECT version()``, and constant expressions (``SELECT
    # 1 + 1``). ``emit`` is False when a constant ``WHERE`` evaluates false, so the
    # result has the column shape but zero rows.
    columns: list[tuple[str, str, Any]]  # (out_name, type_tag, python_value)
    emit: bool = True


@dataclass
class SelectPlan:
    table: TableDef
    filter: dict[str, Any]
    # ORDER BY as (field_path, direction, nulls_first); realized by a
    # Postgres-semantics Python sort in the executor.
    order: list[tuple[str, int, bool]]
    limit: int
    skip: int
    out_columns: list[tuple[str, Column]] = field(default_factory=list)
    count_star: bool = False
    count_alias: str = "count"


@dataclass
class CorrelatedSelectPlan:
    """A single-table SELECT whose WHERE references the outer row (EXISTS /
    correlated subquery), so it can't lower to a pushdown Mongo filter — the
    executor evaluates ``where`` per candidate row."""

    table: TableDef
    where: Any  # exp.Expression — the raw WHERE predicate
    out_columns: list[tuple[str, Column]] = field(default_factory=list)
    order: list[tuple[str, int, bool]] = field(default_factory=list)
    limit: int = 0
    skip: int = 0
    count_star: bool = False
    count_alias: str = "count"
    outer_alias: str | None = None


@dataclass
class UpdatePlan:
    table: TableDef
    filter: dict[str, Any]
    update: dict[str, Any]
    returning: list[tuple[str, Column, Any]] | None = None


@dataclass
class DeletePlan:
    table: TableDef
    filter: dict[str, Any]
    returning: list[tuple[str, Column, Any]] | None = None


Plan = CreateTablePlan | DropTablePlan | InsertPlan | SelectPlan | UpdatePlan | DeletePlan


# ---------------------------------------------------------------------------
# Literal / column extraction
# ---------------------------------------------------------------------------


def _literal(node: exp.Expression) -> Any:
    """Extract a Python value from a literal-ish AST node."""
    if isinstance(node, exp.Paren):
        return _literal(node.this)
    if isinstance(node, exp.Cast):
        # ``'x'::varchar`` / ``CAST($1 AS SMALLINT)`` — drivers (and SQLAlchemy's
        # reflection) annotate values with a target type. Honour a *numeric* cast
        # so a text-bound param (extended protocol decodes ``$1`` as a string)
        # compares numerically rather than as a string (Mongo orders numbers
        # before strings, so ``attnum > '0'`` would be wrongly false).
        return _coerce_cast(_literal(node.this), node.to)
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
    if isinstance(node, exp.Anonymous) and str(node.this).lower() == "to_regtype":
        # ``to_regtype('name')`` resolves a type name to its OID (NULL if unknown).
        # SQLAlchemy's psycopg dialect probes ``t.oid = to_regtype('hstore')`` at
        # connect time; an unknown type must yield NULL → matches no pg_type row.
        arg = node.expressions[0] if node.expressions else None
        return _to_regtype(_literal(arg)) if arg is not None else None
    raise errors.feature_not_supported(f"unsupported value expression: {node.sql()}")


def _to_regtype(name: Any) -> int | None:
    """Map a type name (as ``to_regtype`` takes) to its OID, or None if unknown."""
    if not isinstance(name, str):
        return None
    key = name.strip().lower()
    for tag, typname in typemap.PG_TYPENAME.items():
        if key in (typname, tag, typemap.SQL_TYPE_NAME.get(tag)):
            return typemap.PG_OID.get(tag)
    return None


# Catalog-relation OIDs for ``'pg_catalog.pg_class'::regclass``-style casts (used
# by SQLAlchemy's get_table_comment join on ``pg_description.classoid``).
_REGCLASS_OIDS = {
    "pg_class": 1259,
    "pg_type": 1247,
    "pg_attribute": 1249,
    "pg_constraint": 2606,
    "pg_namespace": 2615,
    "pg_index": 2610,
    "pg_description": 2609,
    "pg_proc": 1255,
}


def _regclass_oid(value: Any) -> Any:
    """``<name>::regclass`` → the catalog relation's OID (unchanged if unknown)."""
    if not isinstance(value, str):
        return value
    name = value.rsplit(".", 1)[-1].strip('"')
    return _REGCLASS_OIDS.get(name, value)


def _coerce_cast(value: Any, datatype: exp.Expression | None) -> Any:
    """Coerce a value to a Python number when a CAST targets a numeric type.

    Non-numeric casts (varchar/text/etc.) leave the value unchanged — the
    column-type coercion downstream handles those.
    """
    if value is None or datatype is None:
        return value
    if isinstance(datatype, exp.ObjectIdentifier) and str(datatype.this).upper() == "REGCLASS":
        return _regclass_oid(value)
    tag = typemap.type_tag_for_sql(datatype) if isinstance(datatype, exp.DataType) else None
    try:
        if tag in ("int4", "int8"):
            return int(value)
        if tag == "float8":
            return float(value)
        if tag == "numeric":
            from decimal import Decimal

            return value if isinstance(value, Decimal) else Decimal(str(value))
    except (TypeError, ValueError):
        return value
    return value


def _column_name(node: exp.Expression) -> str:
    if isinstance(node, exp.Column):
        return node.name
    if isinstance(node, exp.Identifier):
        return node.name
    raise errors.feature_not_supported(f"expected a column, got: {node.sql()}")


def _literal_default(node: exp.Expression, tag: str) -> tuple[bool, Any]:
    """A column DEFAULT expression → ``(has_default, coerced_value)``. Only literal
    defaults (number / string / bool / NULL) are stored; a function / expression
    default (e.g. ``now()``) is not modeled — it reads as "no static default"."""
    if isinstance(node, exp.Null):
        return True, None
    if isinstance(node, (exp.Literal, exp.Boolean, exp.Neg)):
        return True, typemap.coerce(_literal(node), tag)
    return False, None


def _column_default(coldef: exp.ColumnDef, tag: str) -> tuple[bool, Any]:
    for c in coldef.args.get("constraints") or []:
        if type(c.kind).__name__ == "DefaultColumnConstraint":
            return _literal_default(c.kind.this, tag)
    return False, None


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


def _array_elements(node: exp.Expression) -> list[exp.Expression]:
    """Unwrap an ``ARRAY[...]`` (possibly parenthesised) to its element nodes."""
    if isinstance(node, exp.Paren):
        return _array_elements(node.this)
    if isinstance(node, exp.Array):
        return list(node.expressions)
    raise errors.feature_not_supported(f"unsupported array operand: {node.sql()}")


_LITERAL_SENTINEL = object()


def _try_literal(node: exp.Expression) -> Any:
    """``_literal(node)`` or the sentinel if it isn't a constant expression."""
    try:
        return _literal(node)
    except errors.SQLError:
        return _LITERAL_SENTINEL


def _is_literalish(node: exp.Expression) -> bool:
    return _try_literal(node) is not _LITERAL_SENTINEL


def _field_literal_pair(
    left: exp.Expression, right: exp.Expression
) -> tuple[exp.Expression, exp.Expression] | None:
    """For ``field OP const`` (either order), return ``(field_node, const_node)``."""
    if _is_field_node(left) and _is_literalish(right):
        return (left, right)
    if _is_field_node(right) and _is_literalish(left):
        return (right, left)
    return None


# Comparison op -> the aggregation-expression operator used inside ``$expr`` when
# neither side is a constant (column-to-column / arithmetic predicates).
_EXPR_CMP: dict[type, str] = {
    exp.EQ: "$eq",
    exp.NEQ: "$ne",
    exp.GT: "$gt",
    exp.GTE: "$gte",
    exp.LT: "$lt",
    exp.LTE: "$lte",
}
_ARITH_OPS: dict[type, str] = {
    exp.Add: "$add",
    exp.Sub: "$subtract",
    exp.Mul: "$multiply",
    exp.Div: "$divide",
}


def _to_agg_expr(node: exp.Expression, resolve: Resolve) -> Any:
    """Lower a scalar WHERE operand to a Mongo aggregation expression for ``$expr``.

    Columns / jsonb paths become ``$field`` refs, arithmetic nests, and constants
    pass through (strings wrapped in ``$literal`` so a leading ``$`` isn't read as
    a field path). Anything else (function calls, etc.) raises — those predicates
    aren't supported yet."""
    if isinstance(node, exp.Paren):
        return _to_agg_expr(node.this, resolve)
    if _is_field_node(node):
        return "$" + _field(node, resolve)[0]
    if isinstance(node, exp.Cast):
        return _to_agg_expr(node.this, resolve)
    if type(node) in _ARITH_OPS:
        return {
            _ARITH_OPS[type(node)]: [
                _to_agg_expr(node.this, resolve),
                _to_agg_expr(node.expression, resolve),
            ]
        }
    val = _literal(node)
    return {"$literal": val} if isinstance(val, str) else val


# Catalog predicates that are functions of visibility/scope which, on a
# single-node SecantusDB where every relation lives in the default search path,
# are always true. SQLAlchemy's reflection emits these in its catalog WHEREs.
_ALWAYS_TRUE_PREDICATES = {"pg_table_is_visible", "pg_type_is_visible"}


@dataclass
class SubqueryCtx:
    """Carries what a WHERE subquery needs to evaluate itself (it runs the inner
    SELECT through the engine, so aggregates / WHERE / etc. all work)."""

    storage: Any
    db: str
    catalog: Any
    session: Any


# The single-table pushdown path threads a SubqueryCtx explicitly. The pipeline
# planners (join / GROUP BY / evaluated / DISTINCT) call `_where_filter` from many
# places, so `plan_pipeline_select` publishes the context here for the duration of
# planning and every `_where_filter` picks it up — one set-point, no signature
# churn. Planning-scoped and reset in a finally.
_pipeline_subctx: contextvars.ContextVar[SubqueryCtx | None] = contextvars.ContextVar(
    "pipeline_subctx", default=None
)


def _subquery_select(node: exp.Expression) -> exp.Expression:
    return node.this if isinstance(node, exp.Subquery) else node


def _subquery_has_outer_ref(select: exp.Select) -> bool:
    """Heuristic correlation check: a column qualified with an alias not defined
    inside the subquery itself references the outer query (correlated)."""
    inner: set[str | None] = set()
    from_node = select.find(exp.From)
    if from_node is not None:
        src = from_node.this
        inner.add(src.alias or getattr(src, "name", None))
    for jn in select.args.get("joins") or []:
        src = jn.this
        inner.add(src.alias or getattr(src, "name", None))
    return any(col.table and col.table not in inner for col in select.find_all(exp.Column))


def _validate_scalar_subquery(select: exp.Expression) -> exp.Select:
    if not isinstance(select, exp.Select):
        raise errors.feature_not_supported("unsupported subquery")
    exprs = select.expressions
    bare = (
        exprs[0].this
        if exprs and isinstance(exprs[0], exp.Alias)
        else (exprs[0] if exprs else None)
    )
    if len(exprs) != 1 or isinstance(bare, exp.Star):
        raise errors.feature_not_supported("a subquery here must select exactly one column")
    if _subquery_has_outer_ref(select):
        raise errors.feature_not_supported("correlated subqueries are not supported")
    return select


def _run_inner_select(select: exp.Select, subctx: SubqueryCtx) -> Any:
    # Lazy import to avoid a planner<->engine import cycle.
    from secantus.sql import engine

    return engine.run_inner_select(
        select, subctx.storage, subctx.db, subctx.catalog, subctx.session
    )


def _eval_in_subquery(query_node: exp.Expression, subctx: SubqueryCtx, tag: str) -> list[Any]:
    select = _validate_scalar_subquery(_subquery_select(query_node))
    res = _run_inner_select(select, subctx)
    return [typemap.coerce(row[0], tag) for row in res.rows]


def _eval_scalar_subquery(query_node: exp.Expression, subctx: SubqueryCtx, tag: str) -> Any:
    select = _validate_scalar_subquery(_subquery_select(query_node))
    res = _run_inner_select(select, subctx)
    return typemap.coerce(res.rows[0][0], tag) if res.rows else None


_FLIP_CMP = {"$gt": "$lt", "$gte": "$lte", "$lt": "$gt", "$lte": "$gte", "$eq": "$eq", "$ne": "$ne"}


def _comparison_subquery_filter(
    node: exp.Expression, resolve: Resolve, subctx: SubqueryCtx | None
) -> dict[str, Any] | None:
    """``field OP (SELECT scalar ...)`` → ``{field: {op: value}}`` (None if neither
    side is a subquery, so the caller falls through to the normal handling)."""
    left, right = node.this, node.expression
    if not (isinstance(left, exp.Subquery) or isinstance(right, exp.Subquery)):
        return None
    if subctx is None:
        raise errors.feature_not_supported("scalar subquery is not supported here")
    if isinstance(right, exp.Subquery) and _is_field_node(left):
        fld, sub, flip = left, right, False
    elif isinstance(left, exp.Subquery) and _is_field_node(right):
        fld, sub, flip = right, left, True
    else:
        raise errors.feature_not_supported(f"unsupported subquery comparison: {node.sql()}")
    field, tag = _field(fld, resolve)
    value = _eval_scalar_subquery(sub, subctx, tag)
    op = _EXPR_CMP[type(node)]
    if flip:
        op = _FLIP_CMP[op]
    return {field: value} if op == "$eq" else {field: {op: value}}


def _expr_to_filter(
    node: exp.Expression, resolve: Resolve, subctx: SubqueryCtx | None = None
) -> dict[str, Any]:
    if isinstance(node, exp.Paren):
        return _expr_to_filter(node.this, resolve, subctx)

    # A schema-qualified function predicate (``pg_catalog.pg_table_is_visible(...)``)
    # parses as Dot(Identifier, Anonymous); unwrap to the function call.
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        node = node.expression
    if isinstance(node, exp.Anonymous) and node.name.lower() in _ALWAYS_TRUE_PREDICATES:
        return {}

    if isinstance(node, exp.And):
        parts = [
            _expr_to_filter(node.this, resolve, subctx),
            _expr_to_filter(node.expression, resolve, subctx),
        ]
        return _merge_and(parts)

    if isinstance(node, exp.Or):
        return {
            "$or": [
                _expr_to_filter(node.this, resolve, subctx),
                _expr_to_filter(node.expression, resolve, subctx),
            ]
        }

    if isinstance(node, exp.Not):
        inner = node.this
        # IS NOT NULL parses as Not(Is(col, Null)).
        if isinstance(inner, exp.Is) and isinstance(inner.expression, exp.Null):
            field, _ = _field(inner.this, resolve)
            return {field: {"$ne": None}}
        return {"$nor": [_expr_to_filter(inner, resolve, subctx)]}

    if isinstance(node, exp.Exists):
        raise errors.feature_not_supported("EXISTS (subquery) is not supported")

    if isinstance(node, exp.Is):
        field, _ = _field(node.this, resolve)
        if isinstance(node.expression, exp.Null):
            return {field: None}
        raise errors.feature_not_supported(f"unsupported IS predicate: {node.sql()}")

    if isinstance(node, (exp.EQ, exp.NEQ, *_CMP_OPS.keys())):
        sub = _comparison_subquery_filter(node, resolve, subctx)
        if sub is not None:
            return sub

    if isinstance(node, exp.EQ):
        left, right = node.this, node.expression
        if isinstance(left, exp.Any) or isinstance(right, exp.Any):
            # ``col = ANY(ARRAY[...])`` is Postgres' IN — SQLAlchemy's reflection
            # emits ``relkind = ANY(ARRAY['r','p',...])``.
            anynode, fld = (left, right) if isinstance(left, exp.Any) else (right, left)
            field, tag = _field(fld, resolve)
            values = [typemap.coerce(_literal(e), tag) for e in _array_elements(anynode.this)]
            return {field: {"$in": values}}
        pair = _field_literal_pair(left, right)
        if pair is not None:
            field, tag = _field(pair[0], resolve)
            return {field: typemap.coerce(_literal(pair[1]), tag)}
        return {"$expr": {"$eq": [_to_agg_expr(left, resolve), _to_agg_expr(right, resolve)]}}

    if isinstance(node, exp.NEQ):
        left, right = node.this, node.expression
        pair = _field_literal_pair(left, right)
        if pair is not None:
            field, tag = _field(pair[0], resolve)
            return {field: {"$ne": typemap.coerce(_literal(pair[1]), tag)}}
        return {"$expr": {"$ne": [_to_agg_expr(left, resolve), _to_agg_expr(right, resolve)]}}

    for cls, (op, flipped) in _CMP_OPS.items():
        if isinstance(node, cls):
            left, right = node.this, node.expression
            if _is_field_node(left) and _is_literalish(right):
                field, tag = _field(left, resolve)
                return {field: {op: typemap.coerce(_literal(right), tag)}}
            if _is_field_node(right) and _is_literalish(left):
                field, tag = _field(right, resolve)
                return {field: {flipped: typemap.coerce(_literal(left), tag)}}
            return {
                "$expr": {
                    _EXPR_CMP[cls]: [_to_agg_expr(left, resolve), _to_agg_expr(right, resolve)]
                }
            }

    if isinstance(node, exp.In):
        field, tag = _field(node.this, resolve)
        if node.args.get("query") is not None:
            if subctx is None:
                raise errors.feature_not_supported("IN (subquery) is not supported")
            return {field: {"$in": _eval_in_subquery(node.args["query"], subctx, tag)}}
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

    if isinstance(node, exp.ArrayContainsAll):  # jsonb @> (contains)
        field, _ = _field(node.this, resolve)
        return _jsonb_contains_filter(field, _json_value(node.expression))

    if isinstance(node, exp.ArrayContainedBy):  # jsonb <@ (contained by)
        # ``const <@ field`` is exactly ``field @> const`` (the field contains the
        # constant), which pushes down. ``field <@ const`` is a subset constraint
        # on the stored value's whole shape — not a value lookup — so it can't
        # lower to a Mongo filter; faithful not-supported beats a silent divergence.
        if _is_field_node(node.expression) and _is_literalish(node.this):
            field, _ = _field(node.expression, resolve)
            return _jsonb_contains_filter(field, _json_value(node.this))
        raise errors.feature_not_supported(
            "the jsonb <@ (contained by) operator is only supported as "
            "<constant> <@ field (equivalently field @> <constant>)"
        )

    if isinstance(node, exp.JSONBContains):  # jsonb ? (top-level key / element exists)
        field, _ = _field(node.this, resolve)
        return {"$or": _jsonb_key_exists_clauses(field, str(_literal(node.expression)))}

    if isinstance(node, exp.JSONBContainsAnyTopKeys):  # jsonb ?| (any key exists)
        field, _ = _field(node.this, resolve)
        clauses: list[dict[str, Any]] = []
        for e in _array_elements(node.expression):
            clauses.extend(_jsonb_key_exists_clauses(field, str(_literal(e))))
        return {"$or": clauses}

    if isinstance(node, exp.JSONBContainsAllTopKeys):  # jsonb ?& (all keys exist)
        field, _ = _field(node.this, resolve)
        keys = [str(_literal(e)) for e in _array_elements(node.expression)]
        return {"$and": [{"$or": _jsonb_key_exists_clauses(field, k)} for k in keys]}

    # A bare boolean column used as a predicate (``WHERE flag`` / ``WHERE NOT
    # flag``) — Postgres treats it as ``flag IS TRUE``.
    if isinstance(node, exp.Column):
        field, _ = resolve(node)
        return {field: True}

    raise errors.feature_not_supported(f"unsupported WHERE clause: {node.sql()}")


def _json_value(node: exp.Expression) -> Any:
    """Decode a jsonb literal operand (``'{"a":1}'`` / ``'[1,2]'::jsonb``)."""
    raw = _literal(node)
    if isinstance(raw, str):
        try:
            return json.loads(raw)
        except (ValueError, TypeError) as exc:
            raise errors.feature_not_supported(f"invalid jsonb literal: {raw!r}") from exc
    return raw


def _jsonb_key_exists_clauses(path: str, key: str) -> list[dict[str, Any]]:
    """Mongo clauses for Postgres ``jsonb ? key`` at ``path``: the value is an
    object with top-level key ``key``, OR an array containing the string ``key``,
    OR the string ``key`` itself (``{path: key}`` matches the array / scalar cases
    by Mongo's array-aware equality)."""
    return [{f"{path}.{key}": {"$exists": True}}, {path: key}]


def _jsonb_contains_filter(path: str, value: Any) -> dict[str, Any]:
    """Translate Postgres ``field @> value`` containment into a Mongo filter.

    An object RHS becomes a conjunction of dotted-path equalities (recursively);
    an array RHS becomes ``$all`` (the field array contains every element); a
    scalar RHS becomes a plain equality."""
    if isinstance(value, dict):
        return _merge_and([_jsonb_contains_filter(f"{path}.{k}", v) for k, v in value.items()])
    if isinstance(value, list):
        return {path: {"$all": value}}
    return {path: value}


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


def _where_filter(
    stmt: exp.Expression, table: TableDef, subctx: SubqueryCtx | None = None
) -> dict[str, Any]:
    where = stmt.args.get("where")
    if where is None:
        return {}
    # The pipeline planners don't thread `subctx`; fall back to the one published
    # by `plan_pipeline_select` so WHERE subqueries work there too.
    ctx = subctx or _pipeline_subctx.get()
    return _expr_to_filter(where.this, table_resolver(table), ctx)


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
        if isinstance(coldef, exp.ForeignKey):
            # Table-level FOREIGN KEY — collected by _extract_foreign_keys below.
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
        has_default, default = _column_default(coldef, tag)
        columns.append(
            Column(
                name=coldef.name,
                type_tag=tag,
                field="_id" if is_pk else coldef.name,
                pk=is_pk,
                nullable=nullable,
                has_default=has_default,
                default=default,
            )
        )
    if not pk_seen:
        # No PK: the _id is auto-assigned by storage and not surfaced as a
        # column. Fine for the spike.
        pass
    fks = _extract_foreign_keys(schema, table_name)
    table = TableDef(name=table_name, collection=table_name, columns=columns, foreign_keys=fks)
    return CreateTablePlan(table=table, if_not_exists=bool(stmt.args.get("exists")))


def _ref_target(ref: exp.Reference) -> tuple[str, tuple[str, ...]]:
    """A ``REFERENCES`` clause → ``(ref_table, (ref_col, ...))``. An empty column
    list (``REFERENCES t``) points at the target's PRIMARY KEY, left empty here
    and resolved to ``_id`` by reflection."""
    schema = ref.this  # exp.Schema or exp.Table
    if isinstance(schema, exp.Schema):
        return schema.this.name, tuple(_column_name(c) for c in schema.expressions)
    if isinstance(schema, exp.Table):
        return schema.name, ()
    raise errors.feature_not_supported(f"unsupported REFERENCES target: {ref.sql()}")


def _ref_actions(ref: exp.Reference) -> tuple[str | None, str | None]:
    """Parse ``ON DELETE`` / ``ON UPDATE`` referential actions out of a
    ``Reference``'s option strings (e.g. ``"ON DELETE CASCADE"``)."""
    on_delete = on_update = None
    for opt in ref.args.get("options") or []:
        text = str(opt).upper()
        if text.startswith("ON DELETE "):
            on_delete = text[len("ON DELETE ") :].strip()
        elif text.startswith("ON UPDATE "):
            on_update = text[len("ON UPDATE ") :].strip()
    return on_delete, on_update


def _make_fk(
    table_name: str, cols: tuple[str, ...], ref: exp.Reference, name: str | None = None
) -> ForeignKey:
    ref_table, ref_cols = _ref_target(ref)
    on_delete, on_update = _ref_actions(ref)
    # Postgres' default constraint name: <table>_<firstcol>_fkey (an explicit
    # ``CONSTRAINT <name>`` wins when supplied, e.g. from ALTER TABLE ADD).
    con_name = name or (f"{table_name}_{cols[0]}_fkey" if cols else f"{table_name}_fkey")
    return ForeignKey(
        name=con_name,
        columns=cols,
        ref_table=ref_table,
        ref_columns=ref_cols,
        on_delete=on_delete,
        on_update=on_update,
    )


def _extract_foreign_keys(schema: exp.Schema, table_name: str) -> list[ForeignKey]:
    """Collect declared foreign keys from a ``CREATE TABLE`` column list — both
    column-level ``col type REFERENCES t(c)`` and table-level ``FOREIGN KEY (c)
    REFERENCES t(c)``."""
    fks: list[ForeignKey] = []
    for coldef in schema.expressions:
        if isinstance(coldef, exp.ForeignKey):  # table-level
            cols = tuple(_column_name(c) for c in coldef.args.get("expressions") or [])
            ref = coldef.args.get("reference")
            if ref is not None:
                fks.append(_make_fk(table_name, cols, ref))
            continue
        if isinstance(coldef, exp.ColumnDef):  # column-level REFERENCES
            for con in coldef.args.get("constraints") or []:
                if isinstance(con.kind, exp.Reference):
                    fks.append(_make_fk(table_name, (coldef.name,), con.kind))
    return fks


def _with_pk(col: Column, pk_name: str) -> Column:
    if col.name != pk_name:
        return col
    return Column(name=col.name, type_tag=col.type_tag, field="_id", pk=True, nullable=False)


def plan_drop_table(stmt: exp.Drop) -> DropTablePlan:
    return DropTablePlan(name=stmt.this.name, if_exists=bool(stmt.args.get("exists")))


def plan_alter_table(stmt: exp.Alter) -> AlterTablePlan:
    kind = str(stmt.args.get("kind") or "TABLE").upper()
    if kind != "TABLE":
        raise errors.feature_not_supported(f"ALTER {kind} is not supported")
    return AlterTablePlan(
        name=stmt.this.name,
        if_exists=bool(stmt.args.get("exists")),
        actions=list(stmt.args.get("actions") or []),
    )


def plan_create_index(stmt: exp.Create, table: TableDef) -> CreateIndexPlan:
    index = stmt.this  # exp.Index
    params = index.args.get("params")
    if params is None or not params.args.get("columns"):
        raise errors.feature_not_supported("CREATE INDEX requires a column list")
    key_spec: dict[str, int] = {}
    for col in params.args["columns"]:
        ordered = col if isinstance(col, exp.Ordered) else None
        col_node = ordered.this if ordered is not None else col
        name = _column_name(col_node)
        direction = -1 if (ordered is not None and ordered.args.get("desc")) else 1
        key_spec[table.field_for(name)] = direction
    name_ident = index.this
    index_name = name_ident.name if name_ident is not None else _default_index_name(key_spec)
    return CreateIndexPlan(
        collection=table.collection,
        name=index_name,
        key_spec=key_spec,
        unique=bool(stmt.args.get("unique")),
        if_not_exists=bool(stmt.args.get("exists")),
    )


def _default_index_name(key_spec: dict[str, int]) -> str:
    # Mirror mongod's auto-generated index name: field_dir joined by underscores.
    return "_".join(f"{field}_{direction}" for field, direction in key_spec.items())


def plan_drop_index(stmt: exp.Drop) -> DropIndexPlan:
    return DropIndexPlan(name=stmt.this.name, if_exists=bool(stmt.args.get("exists")))


def insert_target_columns(stmt: exp.Insert, table: TableDef) -> list[str]:
    """The target column names for an INSERT: the explicit ``(a, b)`` list, or
    every column of the table when no list is given."""
    schema = stmt.this
    if isinstance(schema, exp.Schema):
        return [_column_name(c) for c in schema.expressions]
    return [c.name for c in table.columns]


def _insert_doc(col_names: list[str], raw_values: list[Any], table: TableDef) -> dict[str, Any]:
    """Build one insert doc from raw Python values mapped positionally onto
    ``col_names`` — shared by the VALUES and INSERT…SELECT paths. Coerces per the
    target column's type, maps the PK column to ``_id``, and rejects a NULL (or
    omitted) NOT NULL column."""
    doc: dict[str, Any] = {}
    provided = set()
    for name, raw in zip(col_names, raw_values, strict=True):
        col = table.column(name)
        if col is None:
            if table.reflected:
                # Schema-on-read: an un-sampled field is a valid insert target
                # (the ``_id`` field is still the PK / NOT NULL).
                col = Column(name, "any", name, pk=(name == "_id"), nullable=(name != "_id"))
            else:
                raise errors.undefined_column(name)
        if raw is None and not col.nullable:
            raise errors.not_null_violation(name)
        doc[col.field] = typemap.coerce(raw, col.type_tag)
        provided.add(name)
    # An omitted column takes its DEFAULT if it has one; otherwise a NOT NULL
    # omission is a violation.
    for col in table.columns:
        if col.name in provided:
            continue
        if col.has_default:
            doc[col.field] = typemap.coerce(col.default, col.type_tag)
        elif not col.nullable:
            raise errors.not_null_violation(col.name)
    return doc


def plan_insert(stmt: exp.Insert, table: TableDef) -> InsertPlan:
    col_names = insert_target_columns(stmt, table)
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
        docs.append(_insert_doc(col_names, [_literal(c) for c in cells], table))
    return InsertPlan(
        table=table,
        docs=docs,
        returning=_returning_columns(stmt, table),
        on_conflict=_plan_on_conflict(stmt, table),
    )


def plan_insert_rows(stmt: exp.Insert, table: TableDef, rows: list[tuple[Any, ...]]) -> InsertPlan:
    """Plan an ``INSERT … SELECT`` from the source query's already-evaluated rows
    (the engine runs the source query, since planning is storage-free). Each
    row's values map positionally onto the target columns."""
    col_names = insert_target_columns(stmt, table)
    docs = [_insert_doc(col_names, list(row), table) for row in rows]
    return InsertPlan(
        table=table,
        docs=docs,
        returning=_returning_columns(stmt, table),
        on_conflict=_plan_on_conflict(stmt, table),
    )


def _ordered_target(node: exp.Expression) -> exp.Expression:
    """Unwrap an ``ON CONFLICT (col)`` key, which sqlglot wraps in ``Ordered``."""
    return node.this if isinstance(node, exp.Ordered) else node


def _plan_on_conflict(stmt: exp.Insert, table: TableDef) -> OnConflict | None:
    """Lower an ``ON CONFLICT`` clause to an :class:`OnConflict`, or None.

    Supports ``DO NOTHING`` (with or without a conflict target) and
    ``DO UPDATE SET … [WHERE …]`` with a column conflict target. ``ON CONSTRAINT
    <name>`` is rejected — we have no named-constraint registry, so the user must
    name the conflicting column(s)."""
    clause = stmt.args.get("conflict")
    if clause is None:
        return None
    if clause.args.get("constraint") is not None:
        raise errors.feature_not_supported(
            "ON CONFLICT ON CONSTRAINT is not supported; name the conflict column(s) instead"
        )
    action = clause.args.get("action")
    action_text = (action.name if action is not None else "").upper()
    conflict_fields = [
        table.field_for(_column_name(_ordered_target(k)))
        for k in (clause.args.get("conflict_keys") or [])
    ]
    if "NOTHING" in action_text:
        return OnConflict(action="nothing", conflict_fields=conflict_fields)
    if "UPDATE" not in action_text:
        raise errors.feature_not_supported(f"unsupported ON CONFLICT action: {action_text}")
    if not conflict_fields:
        # Postgres requires an arbiter index for DO UPDATE — i.e. a conflict
        # target — so it knows which row to update.
        raise errors.syntax_error("ON CONFLICT DO UPDATE requires a conflict target")
    set_exprs: list[tuple[str, str, Any]] = []
    for assignment in clause.args.get("expressions") or []:
        if not isinstance(assignment, exp.EQ):
            raise errors.feature_not_supported(
                f"unsupported ON CONFLICT SET assignment: {assignment.sql()}"
            )
        col_name = _column_name(assignment.this)
        set_exprs.append(
            (table.field_for(col_name), table.type_for(col_name), assignment.expression)
        )
    where_node = clause.args.get("where")
    return OnConflict(
        action="update",
        conflict_fields=conflict_fields,
        set_exprs=set_exprs,
        where=where_node.this if where_node is not None else None,
    )


def _nulls_first(o: exp.Ordered) -> bool:
    """Whether NULLs sort ahead of non-NULLs for this ORDER BY term. sqlglot fills
    ``nulls_first`` with Postgres's default when the clause is implicit (DESC →
    first, ASC → last), and with the explicit value for ``NULLS FIRST/LAST``."""
    nf = o.args.get("nulls_first")
    return bool(nf) if nf is not None else bool(o.args.get("desc"))


def _order_terms(stmt: exp.Expression, table: TableDef) -> list[tuple[str, int, bool]]:
    """ORDER BY lowered to ``(field_path, direction, nulls_first)`` triples — the
    single-table / correlated form, realized by a Postgres-semantics Python sort
    in the executor (so NULL placement matches Postgres, not Mongo sort order)."""
    order = stmt.args.get("order")
    if order is None:
        return []
    terms: list[tuple[str, int, bool]] = []
    for o in order.expressions:
        col = _column_name(o.this)
        terms.append((table.field_for(col), -1 if o.args.get("desc") else 1, _nulls_first(o)))
    return terms


def _emit_pipeline_sort(pipeline: list[dict[str, Any]], terms: list[tuple[str, int, bool]]) -> None:
    """Append a NULL-aware ``$sort`` for ``terms`` (``(field, direction,
    nulls_first)``). Mongo's ``$sort`` orders NULL/missing as the lowest value, so
    each term gets a companion ``$cond`` null-rank field sorted ahead of it — that
    places NULLs first or last per ``nulls_first``, independent of direction, the
    way Postgres does. The rank fields are dropped again after the sort."""
    if not terms:
        return
    nullranks: dict[str, Any] = {}
    sort: dict[str, int] = {}
    for k, (name, direction, nulls_first) in enumerate(terms):
        nr = f"__nr_{k}"
        nullranks[nr] = {
            "$cond": [
                {"$eq": [{"$ifNull": [f"${name}", None]}, None]},
                0 if nulls_first else 1,
                1 if nulls_first else 0,
            ]
        }
        sort[nr] = 1
        sort[name] = direction
    pipeline.append({"$addFields": nullranks})
    pipeline.append({"$sort": sort})
    pipeline.append({"$unset": list(nullranks)})


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


def _const_scope(node: exp.Expression) -> Any:
    """The scope for a FROM-less SELECT: any column reference is undefined."""
    name = node.name if isinstance(node, exp.Column) else node.sql()
    raise errors.undefined_column(name)


def plan_constant_select(stmt: exp.Select, session: Any) -> ConstantSelectPlan:
    """Plan a FROM-less ``SELECT <expr>, ... [WHERE <const>]``.

    Literals are read directly; session/info functions (``version()``,
    ``current_database()``, ``current_setting(...)``, ...) resolve against the
    connection ``session``; any other constant expression (arithmetic, ``||``,
    function calls, ``CASE`` …) is evaluated by the scalar evaluator against an
    empty scope. A constant ``WHERE`` that evaluates false yields zero rows.
    """
    from secantus.sql import functions, scalar

    if stmt.args.get("group") or stmt.args.get("joins"):
        raise errors.feature_not_supported("FROM-less SELECT supports only constant projections")
    ctx = scalar.ScalarContext(storage=None, catalog=None, db=None, session=session)
    where = stmt.args.get("where")
    emit = where is None or scalar._truthy(scalar.evaluate(where.this, _const_scope, ctx))
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
            value = scalar.evaluate(target, _const_scope, ctx)
            columns.append((alias or "?column?", _infer_scalar_tag(target, _const_scope), value))
    return ConstantSelectPlan(columns=columns, emit=emit)


def plan_select(stmt: exp.Select, table: TableDef, subctx: SubqueryCtx | None = None) -> SelectPlan:
    if stmt.args.get("joins"):
        raise errors.feature_not_supported("JOIN is not supported yet")
    if stmt.args.get("group") or stmt.args.get("having"):
        raise errors.feature_not_supported("GROUP BY / HAVING is not supported yet")
    if stmt.args.get("distinct"):
        raise errors.feature_not_supported("SELECT DISTINCT is not supported yet")

    filt = _where_filter(stmt, table, subctx)
    order = _order_terms(stmt, table)
    limit, skip = _limit_skip(stmt)

    count_alias = _count_star_alias(stmt)
    if count_alias is not None:
        return SelectPlan(
            table=table,
            filter=filt,
            order=order,
            limit=limit,
            skip=skip,
            count_star=True,
            count_alias=count_alias,
        )
    return SelectPlan(
        table=table,
        filter=filt,
        order=order,
        limit=limit,
        skip=skip,
        out_columns=_select_out_columns(stmt, table),
    )


def _count_star_alias(stmt: exp.Select) -> str | None:
    """The output alias if this SELECT is a sole ``COUNT(*)`` (no GROUP BY), else
    None."""
    exprs = stmt.expressions
    if len(exprs) != 1 or not isinstance(exprs[0], (exp.Count, exp.Alias)):
        return None
    inner = exprs[0].this if isinstance(exprs[0], exp.Alias) else exprs[0]
    if isinstance(inner, exp.Count) and isinstance(inner.this, exp.Star):
        alias = exprs[0].alias if isinstance(exprs[0], exp.Alias) else "count"
        return alias or "count"
    return None


def _out_columns(exprs: list[exp.Expression], table: TableDef) -> list[tuple[str, Column]]:
    """The projected ``(output_name, Column)`` list for a column / ``*`` / jsonb
    projection over ``table`` — shared by the SELECT pushdown, correlated-WHERE,
    and ``RETURNING`` plans."""
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
    return out_columns


def _select_out_columns(stmt: exp.Select, table: TableDef) -> list[tuple[str, Column]]:
    """The projected columns for a plain (non-aggregate) SELECT."""
    return _out_columns(stmt.expressions, table)


def _returning_columns(
    stmt: exp.Expression, table: TableDef
) -> list[tuple[str, Column, Any]] | None:
    """The projected items for a write statement's ``RETURNING`` clause, or None
    when there's no ``RETURNING``. Each item is ``(name, Column, expr)``: ``expr``
    is None for a plain column / ``*`` / jsonb path (read straight from the doc);
    for a computed expression (arithmetic, ``||``, function calls, ``CASE`` …) it
    is the raw node, evaluated per returned row by the executor."""
    returning = stmt.args.get("returning")
    if returning is None:
        return None
    items: list[tuple[str, Column, Any]] = []
    resolve = table_resolver(table)
    for e in returning.expressions:
        if isinstance(e, exp.Star):
            items.extend((col.name, col, None) for col in table.columns)
            continue
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, exp.Column):
            cname = _column_name(target)
            col = table.column(cname)
            if col is None:
                if table.reflected:
                    col = Column(cname, "any", cname, pk=False, nullable=True)
                else:
                    raise errors.undefined_column(cname)
            items.append((alias or cname, col, None))
        elif isinstance(target, _JSONB_CLASSES):
            path, tag = _field(target, resolve)
            out_name = alias or "?column?"
            items.append((out_name, Column(out_name, tag, path, pk=False, nullable=True), None))
        else:
            # A computed expression — evaluated per returned row (field unused).
            out_name = alias or "?column?"
            tag = _infer_scalar_tag(target, resolve)
            items.append(
                (out_name, Column(out_name, tag, out_name, pk=False, nullable=True), target)
            )
    return items


def where_needs_per_row(stmt: exp.Select) -> bool:
    """Whether the WHERE clause must be evaluated per-row in Python rather than
    pushed down as a Mongo filter: it contains an ``EXISTS`` predicate or a
    correlated subquery (one that references the outer row). Non-correlated
    ``IN`` / scalar ``= (SELECT …)`` subqueries stay on the fast pushdown path."""
    where = stmt.args.get("where")
    if where is None:
        return False
    node = where.this
    if node.find(exp.Exists) is not None:
        return True
    return any(_subquery_has_outer_ref(sub) for sub in node.find_all(exp.Select))


def plan_correlated_select(stmt: exp.Select, table: TableDef) -> CorrelatedSelectPlan:
    """Plan a single-table SELECT whose WHERE needs per-row evaluation (EXISTS /
    correlated subquery). The whole WHERE is carried verbatim and evaluated by
    the executor against each candidate row via the scalar evaluator."""
    if stmt.args.get("joins") or stmt.args.get("group") or stmt.args.get("having"):
        raise errors.feature_not_supported(
            "correlated subqueries are supported only in a single-table SELECT"
        )
    if stmt.args.get("distinct"):
        raise errors.feature_not_supported("SELECT DISTINCT is not supported yet")
    order = _order_terms(stmt, table)
    limit, skip = _limit_skip(stmt)
    from_node = stmt.find(exp.From)
    outer_alias = from_node.this.alias or None if from_node is not None else None
    count_alias = _count_star_alias(stmt)
    return CorrelatedSelectPlan(
        table=table,
        where=stmt.args["where"].this,
        out_columns=[] if count_alias is not None else _select_out_columns(stmt, table),
        order=order,
        limit=limit,
        skip=skip,
        count_star=count_alias is not None,
        count_alias=count_alias or "count",
        outer_alias=outer_alias,
    )


def plan_update(stmt: exp.Update, table: TableDef) -> UpdatePlan:
    set_doc: dict[str, Any] = {}
    for assign in stmt.expressions:
        if not isinstance(assign, exp.EQ):
            raise errors.feature_not_supported(f"unsupported SET item: {assign.sql()}")
        col_name = _column_name(assign.this)
        col = table.column(col_name)
        if col is None:
            if table.reflected:
                # Schema-on-read: any field is a valid SET target (still can't
                # rewrite the PK, which maps to the immutable Mongo ``_id``).
                col = Column(col_name, "any", col_name, pk=(col_name == "_id"), nullable=True)
            else:
                raise errors.undefined_column(col_name)
        if col.pk:
            raise errors.feature_not_supported("updating the primary key is not supported")
        raw = _literal(assign.expression)
        if raw is None and not col.nullable:
            raise errors.not_null_violation(col_name)
        set_doc[col.field] = typemap.coerce(raw, col.type_tag)
    return UpdatePlan(
        table=table,
        filter=_where_filter(stmt, table),
        update={"$set": set_doc},
        returning=_returning_columns(stmt, table),
    )


def plan_delete(stmt: exp.Delete, table: TableDef) -> DeletePlan:
    return DeletePlan(
        table=table,
        filter=_where_filter(stmt, table),
        returning=_returning_columns(stmt, table),
    )


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
class DerivedTable:
    """A ``(SELECT ...) AS alias`` join source, materialized before the main
    pipeline runs. ``name`` is the ephemeral collection the executor registers
    the sub-plan's rows under (and the join's ``$lookup`` reads from)."""

    name: str
    plan: Any  # a sub-plan (PipelineSelectPlan / EvaluatedSelectPlan)
    columns: list[tuple[str, str]]  # (output_name, type_tag)


@dataclass
class PipelineSelectPlan:
    base_collection: str
    base_filter: dict[str, Any]
    pipeline: list[dict[str, Any]]
    out_columns: list[tuple[str, str]]  # (output_name, type_tag)
    derived: list[DerivedTable] = field(default_factory=list)
    # A WHERE that references the outer row (EXISTS / correlated subquery) can't
    # lower to a Mongo ``$match``; it's carried here and evaluated in Python by the
    # executor. ``residual_split`` is how many leading pipeline stages run *before*
    # the filter — 0 for a single-table GROUP BY (filter the base docs, then group),
    # or the join-prefix length for a JOIN + GROUP BY (join, filter the joined rows,
    # then group), so the survivors are what gets grouped.
    residual_where: exp.Expression | None = None
    residual_resolve: Resolve | None = None
    residual_split: int = 0


@dataclass
class EvaluatedSelectPlan:
    """A join whose SELECT list / ORDER BY has scalar expressions (functions,
    CASE, correlated subqueries) that can't be lowered to a ``$project``.

    The ``pipeline`` performs the joins + WHERE and yields the full joined docs;
    the executor evaluates each output expression in Python per row (via
    ``secantus.sql.scalar``), then applies DISTINCT / ORDER BY / LIMIT.
    """

    base_collection: str
    base_filter: dict[str, Any]
    pipeline: list[dict[str, Any]]  # join + where; NO final $project
    out_columns: list[tuple[str, str]]  # (output_name, type_tag)
    out_exprs: list[exp.Expression]  # parallel to out_columns; AST per output
    resolve: Resolve  # join resolver: Column node -> (field_path, tag)
    order: list[tuple[exp.Expression, int, bool]]  # (expr, direction, nulls_first)
    distinct: bool
    limit: int
    skip: int
    derived: list[DerivedTable] = field(default_factory=list)
    # A correlated / EXISTS WHERE that couldn't lower to a ``$match`` — evaluated
    # per joined row (via ``resolve`` as the outer scope) after the pipeline.
    where: exp.Expression | None = None
    # ``DISTINCT ON (exprs)`` — keep the first row (in ORDER BY order) per distinct
    # value of these expressions. Mutually exclusive with plain ``distinct``.
    distinct_on: list[exp.Expression] = field(default_factory=list)


_AGG_CLASSES: dict[type, str] = {
    exp.Count: "count",
    exp.Sum: "sum",
    exp.Avg: "avg",
    exp.Min: "min",
    exp.Max: "max",
    exp.LogicalAnd: "bool_and",
    exp.LogicalOr: "bool_or",
}

_HAVING_CMP: dict[type, tuple[str, str]] = {
    exp.GT: ("$gt", "$lt"),
    exp.GTE: ("$gte", "$lte"),
    exp.LT: ("$lt", "$gt"),
    exp.LTE: ("$lte", "$gte"),
}


def _aggregate_of(node: exp.Expression) -> tuple[str, str | None, bool] | None:
    """If ``node`` (or its alias target) is an aggregate, return
    ``(func, column, distinct)``. ``column`` is None for ``COUNT(*)``; the
    argument of a ``COUNT(DISTINCT x)`` is unwrapped from its ``exp.Distinct``."""
    inner = node.this if isinstance(node, exp.Alias) else node
    for cls, name in _AGG_CLASSES.items():
        if isinstance(inner, cls):
            arg = inner.this
            distinct = isinstance(arg, exp.Distinct)
            if distinct:
                exprs = arg.expressions
                arg = exprs[0] if exprs else None
            col = _column_name(arg) if isinstance(arg, exp.Column) else None
            return name, col, distinct
    return None


def _array_agg_arg(node: exp.Expression) -> exp.Expression | None:
    """If ``node`` is ``array_agg(<arg>)``, return its argument expression."""
    inner = node.this if isinstance(node, exp.Alias) else node
    return inner.this if isinstance(inner, exp.ArrayAgg) else None


def _srf_of(node: exp.Expression) -> tuple[str, exp.Expression] | None:
    """If ``node`` is a set-returning function, return (kind, array_expr).

    ``unnest(arr)`` (sqlglot ``Explode``) → ('unnest', arr); ``generate_subscripts
    (arr, dim)`` (``Anonymous``) → ('generate_subscripts', arr). The dimension
    argument is ignored (our arrays are one-dimensional)."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Explode):
        return ("unnest", inner.this)
    if isinstance(inner, exp.Dot) and isinstance(inner.expression, exp.Anonymous):
        inner = inner.expression
    if isinstance(inner, exp.Anonymous):
        name = (
            (inner.this if isinstance(inner.this, str) else inner.name).rsplit(".", 1)[-1].lower()
        )
        if name == "unnest" and inner.expressions:
            return ("unnest", inner.expressions[0])
        if name == "generate_subscripts" and inner.expressions:
            return ("generate_subscripts", inner.expressions[0])
        # jsonb set-returning functions: one row per array element / object key.
        if name in ("jsonb_array_elements", "json_array_elements") and inner.expressions:
            return ("jsonb_array_elements", inner.expressions[0])
        if name in ("jsonb_object_keys", "json_object_keys") and inner.expressions:
            return ("jsonb_object_keys", inner.expressions[0])
    return None


def _agg_arg_to_expr(node: exp.Expression, table: TableDef) -> Any:
    """Lower an aggregate argument to a Mongo aggregation expression.

    Used by ``array_agg`` (``$push``). Catalog functions with no Mongo analogue
    that are always NULL in our model (``pg_get_constraintdef`` / ``pg_get_expr``)
    lower to a literal NULL — sound because we store no constraints/defaults.
    """
    if isinstance(node, exp.Paren):
        return _agg_arg_to_expr(node.this, table)
    if isinstance(node, exp.Order):
        # ``array_agg(x ORDER BY y)`` — the intra-aggregate ordering isn't modeled
        # (our only use is over empty catalogs); aggregate the bare expression.
        return _agg_arg_to_expr(node.this, table)
    if isinstance(node, exp.Cast):
        return _agg_arg_to_expr(node.this, table)
    if isinstance(node, exp.Column):
        return f"${table.field_for(node.name)}"
    if isinstance(node, (exp.Literal, exp.Boolean, exp.Null, exp.Neg)):
        return {"$literal": _literal(node)}
    fname = None
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        fname = node.expression.name
    elif isinstance(node, exp.Anonymous):
        fname = node.this if isinstance(node.this, str) else node.name
    if fname is not None and str(fname).rsplit(".", 1)[-1].lower() in (
        "pg_get_constraintdef",
        "pg_get_expr",
    ):
        return {"$literal": None}
    raise errors.feature_not_supported(f"unsupported array_agg argument: {node.sql()}")


def select_needs_pipeline(stmt: exp.Select) -> bool:
    """Whether a SELECT must be compiled to an aggregation pipeline."""
    if (
        stmt.args.get("joins")
        or stmt.args.get("group")
        or stmt.args.get("having")
        or stmt.args.get("distinct")
    ):
        return True
    # A ``(SELECT ...) AS alias`` derived table in FROM — e.g. an expanded view —
    # is materialized by the pipeline path's ``_resolve_source``.
    from_node = next((v for v in stmt.args.values() if isinstance(v, exp.From)), None)
    if from_node is not None and isinstance(from_node.this, exp.Subquery):
        return True
    # A SELECT list / ORDER BY with set-returning or scalar functions, CASE, or
    # subqueries needs per-row evaluation (the pipeline path), not a plain find.
    if _stmt_needs_evaluation(stmt):
        return True
    aggs = [
        e for e in stmt.expressions if _aggregate_of(e) is not None or _array_agg_arg(e) is not None
    ]
    if not aggs:
        return False
    # A lone COUNT(*) (no GROUP BY) is served by the simpler find path.
    if len(stmt.expressions) == 1:
        only = _aggregate_of(stmt.expressions[0])
        if only is not None and only == ("count", None, False):
            return False
    return True


def _lookup_table_def(
    catalog: Any, db: str, table_node: exp.Table, storage: Any = None
) -> TableDef | None:
    """Resolve a (possibly schema-qualified) table to a TableDef.

    Tries the user catalog first, then the ``pg_catalog`` / ``information_schema``
    virtual tables, then — when ``storage`` is supplied and the name is not
    schema-qualified — a reflected (schema-on-read) view of an existing Mongo
    collection. This is what lets joins / aggregates span user tables, the system
    catalogs, *and* un-declared collections written via ``pymongo``.
    """
    from secantus.sql import reflect, virtual

    table = catalog.get(db, table_node.name)
    if table is not None:
        return table
    schema = table_node.args.get("db")
    schema_name = schema.name if schema is not None else None
    vtable = virtual.lookup(schema_name, table_node.name)
    if vtable is not None:
        return vtable.table_def()
    # A reflected collection only makes sense for an unqualified name (a schema
    # qualifier means the caller asked for a specific catalog relation).
    if storage is not None and schema_name is None:
        return reflect.reflect(storage, db, table_node.name)
    return None


def plan_pipeline_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> PipelineSelectPlan | EvaluatedSelectPlan:
    # Publish the subquery context so any WHERE `$match` in the pipeline planners
    # can evaluate a scalar / IN subquery (the same as the single-table pushdown).
    token = _pipeline_subctx.set(SubqueryCtx(storage=storage, db=db, catalog=catalog, session=None))
    try:
        return _plan_pipeline_select(stmt, db, catalog, storage)
    finally:
        _pipeline_subctx.reset(token)


def _plan_pipeline_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> PipelineSelectPlan | EvaluatedSelectPlan:
    if stmt.args.get("joins"):
        if _has_grouping_sets(stmt):
            raise errors.feature_not_supported("GROUPING SETS over a JOIN is not supported")
        has_agg = any(
            _aggregate_of(e) is not None or _array_agg_arg(e) is not None for e in stmt.expressions
        )
        grouped = bool(stmt.args.get("group") or stmt.args.get("having") or has_agg)
        if _select_has_window(stmt) and (grouped or _group_agg_nodes(stmt)):
            # Window functions over a JOIN + GROUP BY (or implicit aggregation).
            return _plan_join_group_window_select(stmt, db, catalog, storage)
        if grouped:
            return _plan_join_group_select(stmt, db, catalog, storage)
        return _plan_join_select(stmt, db, catalog, storage)
    from_node = next((v for v in stmt.args.values() if isinstance(v, exp.From)), None)
    if from_node is None:
        raise errors.feature_not_supported("aggregate without FROM is not supported")
    # The FROM may be a real table or a ``(SELECT ...) AS alias`` derived table
    # (materialized into an ephemeral collection by the executor).
    derived: list[DerivedTable] = []
    _alias, table = _resolve_source(from_node.this, db, catalog, storage, derived)

    has_aggregate = any(
        _aggregate_of(e) is not None or _array_agg_arg(e) is not None for e in stmt.expressions
    )
    grouped = bool(stmt.args.get("group") or stmt.args.get("having") or has_aggregate)
    if _has_grouping_sets(stmt):
        if _select_has_window(stmt):
            raise errors.feature_not_supported(
                "window functions over GROUPING SETS are not supported"
            )
        plan: PipelineSelectPlan | EvaluatedSelectPlan = _plan_grouping_sets_select(stmt, table)
    elif _select_has_window(stmt) and (grouped or _group_agg_nodes(stmt)):
        # Window functions computed over GROUP BY aggregates (or an implicit
        # whole-table aggregation) — a two-phase group-then-window plan.
        plan = _plan_group_window_select(stmt, table)
    elif grouped:
        plan = _plan_group_select(stmt, table)
    elif _stmt_needs_evaluation(stmt) or _distinct_on(stmt):
        # DISTINCT ON needs the evaluated path's sort-then-keep-first-per-key.
        plan = _build_evaluated_single(stmt, table)
    elif stmt.args.get("distinct"):
        plan = _plan_distinct_select(stmt, table)
    else:
        plan = _plan_plain_select(stmt, table)
    plan.derived = derived
    return plan


def _plan_plain_select(stmt: exp.Select, table: TableDef) -> PipelineSelectPlan:
    """A plain projection over a (derived) table — ``$project`` the columns."""
    base_filter = _where_filter(stmt, table)
    resolve = table_resolver(table)
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for col in table.columns:
                nm = names.fresh(col.name)
                project[nm] = f"${col.field}"
                out_columns.append((nm, col.type_tag))
            continue
        path, tag = _field(inner, resolve)
        nm = names.fresh(alias or _column_name(inner))
        project[nm] = f"${path}"
        out_columns.append((nm, tag))
    pipeline: list[dict[str, Any]] = [{"$project": project}]
    _append_sort_limit(pipeline, stmt, {n for n, _ in out_columns})
    return PipelineSelectPlan(table.collection, base_filter, pipeline, out_columns)


def _build_evaluated_single(stmt: exp.Select, table: TableDef) -> EvaluatedSelectPlan:
    """A single-table SELECT needing per-row evaluation (SRFs / scalar funcs).

    The base collection is read with the WHERE filter; the executor evaluates
    each output expression per row (expanding set-returning functions)."""
    resolve = table_resolver(table)
    base_filter = _where_filter(stmt, table)
    out_columns: list[tuple[str, str]] = []
    out_exprs: list[exp.Expression] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for col in table.columns:
                out_columns.append((names.fresh(col.name), col.type_tag))
                out_exprs.append(exp.column(col.name))
            continue
        name = alias or (_column_name(inner) if isinstance(inner, exp.Column) else "?column?")
        out_columns.append((names.fresh(name), _infer_scalar_tag(inner, resolve)))
        out_exprs.append(inner)
    order: list[tuple[exp.Expression, int, bool]] = []
    order_node = stmt.args.get("order")
    if order_node is not None:
        for o in order_node.expressions:
            order.append((o.this, -1 if o.args.get("desc") else 1, _nulls_first(o)))
    limit, skip = _limit_skip(stmt)
    don = _distinct_on(stmt)
    return EvaluatedSelectPlan(
        base_collection=table.collection,
        base_filter=base_filter,
        pipeline=[],
        out_columns=out_columns,
        out_exprs=out_exprs,
        resolve=resolve,
        order=order,
        distinct=bool(stmt.args.get("distinct")) and not don,
        limit=limit,
        skip=skip,
        distinct_on=don,
    )


def _distinct_on(stmt: exp.Select) -> list[exp.Expression]:
    """The expressions of a ``SELECT DISTINCT ON (…)``, or ``[]`` for plain / no
    DISTINCT. Postgres keeps the first row per distinct value of these, in the
    query's ORDER BY order."""
    d = stmt.args.get("distinct")
    if isinstance(d, exp.Distinct) and d.args.get("on") is not None:
        on = d.args["on"]
        return list(on.expressions) if isinstance(on, exp.Tuple) else [on]
    return []


def _plan_distinct_select(stmt: exp.Select, table: TableDef) -> PipelineSelectPlan:
    """A single-table ``SELECT DISTINCT`` → project the columns, then dedup."""
    base_filter = _where_filter(stmt, table)
    resolve = table_resolver(table)
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for col in table.columns:
                nm = names.fresh(col.name)
                project[nm] = f"${col.field}"
                out_columns.append((nm, col.type_tag))
            continue
        path, tag = _field(inner, resolve)
        nm = names.fresh(alias or _column_name(inner))
        project[nm] = f"${path}"
        out_columns.append((nm, tag))
    pipeline: list[dict[str, Any]] = [{"$project": project}]
    _append_distinct(pipeline, out_columns)
    _append_sort_limit(pipeline, stmt, {n for n, _ in out_columns})
    return PipelineSelectPlan(table.collection, base_filter, pipeline, out_columns)


def _accumulator_for(func: str, field: str | None, tag: str | None) -> tuple[dict[str, Any], str]:
    """Build a ``$group`` accumulator from an already-resolved field path + tag.

    ``field`` is the pipeline field path (e.g. ``amt`` or ``b.amt`` after a join)
    and is None only for ``COUNT(*)``. This is the field-resolved core shared by
    the single-table (`_accumulator`) and join (`_join_accumulator`) paths."""
    if func == "count":
        if field is None:
            return {"$sum": 1}, "int8"
        # COUNT(col) counts non-null values.
        return {"$sum": {"$cond": [{"$ne": [f"${field}", None]}, 1, 0]}}, "int8"
    if func == "sum":
        return {"$sum": f"${field}"}, (
            tag if tag in ("int4", "int8", "numeric", "float8") else "float8"
        )
    if func == "avg":
        return {"$avg": f"${field}"}, "float8"
    if func == "min":
        return {"$min": f"${field}"}, (tag or "text")
    if func == "max":
        return {"$max": f"${field}"}, (tag or "text")
    # bool_and = all-true = the minimum boolean (false < true); bool_or = max.
    if func == "bool_and":
        return {"$min": f"${field}"}, "bool"
    if func == "bool_or":
        return {"$max": f"${field}"}, "bool"
    raise errors.feature_not_supported(f"aggregate {func} is not supported")


def _accumulator(func: str, col: str | None, table: TableDef) -> tuple[dict[str, Any], str]:
    if col is None:
        return _accumulator_for(func, None, None)
    return _accumulator_for(func, table.field_for(col), table.type_for(col))


# DISTINCT changes the result only for these — MIN/MAX of a set equal MIN/MAX of
# the raw values, so a DISTINCT min/max just runs the ordinary accumulator.
_DISTINCT_FUNCS = {"count", "sum", "avg"}


def _agg_out_tag(func: str, tag: str | None) -> str:
    if func == "count":
        return "int8"
    if func == "sum":
        return tag if tag in ("int4", "int8", "numeric", "float8") else "float8"
    if func == "avg":
        return "float8"
    return tag or "text"


def _distinct_reduction(func: str, set_field: str) -> dict[str, Any]:
    """Reduce a ``$addToSet`` result (at ``set_field``, e.g. ``$tmp``) to the
    DISTINCT aggregate value, dropping NULLs (SQL aggregates ignore NULL)."""
    nonnull = {"$filter": {"input": set_field, "as": "v", "cond": {"$ne": ["$$v", None]}}}
    if func == "count":
        return {"$size": nonnull}
    total = {
        "$reduce": {"input": nonnull, "initialValue": 0, "in": {"$add": ["$$value", "$$this"]}}
    }
    if func == "sum":
        return total
    if func == "avg":
        cnt = {"$size": nonnull}
        return {"$cond": [{"$eq": [cnt, 0]}, None, {"$divide": [total, cnt]}]}
    raise errors.feature_not_supported(f"DISTINCT is not supported for {func}()")


def _register_distinct_agg(
    func: str,
    field: str,
    tag: str | None,
    alias: str | None,
    names: _NameAllocator,
    accumulators: dict[str, Any],
    reductions: dict[str, Any],
) -> tuple[str, str]:
    """Wire a DISTINCT count/sum/avg: a ``$addToSet`` accumulator collects the
    distinct values; a post-``$group`` ``$addFields`` reduces the set. Returns
    the output field name and its type tag."""
    set_name = names.fresh(f"{alias or func}__distinct")
    accumulators[set_name] = {"$addToSet": f"${field}"}
    fname = names.fresh(alias or func)
    reductions[fname] = _distinct_reduction(func, f"${set_name}")
    return fname, _agg_out_tag(func, tag)


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


def _has_grouping_sets(stmt: exp.Select) -> bool:
    g = stmt.args.get("group")
    return bool(g and (g.args.get("rollup") or g.args.get("cube") or g.args.get("grouping_sets")))


def _grouping_set_cols(node: exp.Expression) -> list[str]:
    """Column names in one grouping-set element — a ``(a, b)`` Tuple, a ``(a)``
    Paren, a bare column, or the empty set ``()`` (→ ``[]``)."""
    if isinstance(node, exp.Tuple):
        cols: list[str] = []
        for x in node.expressions:
            cols.extend(_grouping_set_cols(x))
        return cols
    if isinstance(node, exp.Paren):
        return _grouping_set_cols(node.this)
    if isinstance(node, exp.Column):
        return [_column_name(node)]
    return []  # empty () or a stray literal → no columns


def _grouping_sets(group_node: exp.Group) -> list[list[str]]:
    """Enumerate the grouping sets (each a list of column names) for a GROUP BY
    that uses ROLLUP / CUBE / GROUPING SETS. A plain leading ``GROUP BY a, …`` is
    a prefix present in every set; ROLLUP / CUBE / explicit GROUPING SETS each
    contribute a list of alternatives, cross-producted together (Postgres
    semantics)."""
    base = [_column_name(c) for c in group_node.expressions]
    factors: list[list[list[str]]] = []
    for r in group_node.args.get("rollup") or []:
        cols = [_column_name(c) for c in r.expressions]
        factors.append([cols[:i] for i in range(len(cols), -1, -1)])
    for cnode in group_node.args.get("cube") or []:
        cols = [_column_name(x) for x in cnode.expressions]
        subsets = [
            [cols[i] for i in range(len(cols)) if mask & (1 << i)] for mask in range(2 ** len(cols))
        ]
        factors.append(subsets)
    for gs in group_node.args.get("grouping_sets") or []:
        factors.append([_grouping_set_cols(n) for n in gs.expressions])
    result: list[list[str]] = [list(base)]
    for factor in factors:
        result = [r + s for r in result for s in factor]
    seen: set[tuple[str, ...]] = set()
    deduped: list[list[str]] = []
    for s in result:
        key = tuple(s)
        if key not in seen:
            seen.add(key)
            deduped.append(s)
    return deduped


def _grouping_set_branch(
    stmt: exp.Select, table: TableDef, gset: list[str]
) -> tuple[list[dict[str, Any]], list[tuple[str, str]]]:
    """One grouping set's ``[$group, $project]`` sub-pipeline. Group columns not in
    this set project as literal NULL (Postgres' grouping-set semantics), so every
    branch has the same output shape (required for the ``$unionWith``)."""
    in_set = set(gset)
    group_id = {c: f"${table.field_for(c)}" for c in gset} or None
    accumulators: dict[str, Any] = {}
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        arr_arg = _array_agg_arg(e)
        agg = _aggregate_of(e)
        if arr_arg is not None:
            fname = names.fresh(alias or "array_agg")
            accumulators[fname] = {"$push": _agg_arg_to_expr(arr_arg, table)}
            project[fname] = f"${fname}"
            out_columns.append((fname, "json"))
        elif agg is not None:
            func, col, _distinct = agg
            acc, tag = _accumulator(func, col, table)
            fname = names.fresh(alias or func)
            accumulators[fname] = acc
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        else:
            inner = e.this if isinstance(e, exp.Alias) else e
            if isinstance(inner, exp.Star):
                raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
            col = _column_name(inner)
            out_name = names.fresh(alias or col)
            if col in in_set:
                project[out_name] = f"$_id.{col}"
            else:
                # A group column absent from this set (or an ungrouped column) reads
                # NULL for these rows.
                table.field_for(col)  # validate it's a real column
                project[out_name] = {"$literal": None}
            out_columns.append((out_name, table.type_for(col)))
    return [{"$group": {"_id": group_id, **accumulators}}, {"$project": project}], out_columns


def _plan_grouping_sets_select(stmt: exp.Select, table: TableDef) -> PipelineSelectPlan:
    """GROUP BY ROLLUP / CUBE / GROUPING SETS → the UNION (via ``$unionWith``) of a
    plain GROUP BY per enumerated grouping set."""
    if stmt.args.get("having") is not None:
        raise errors.feature_not_supported("HAVING with GROUPING SETS is not supported")
    if _residual_where(stmt, table) is not None:
        raise errors.feature_not_supported("a correlated WHERE with GROUPING SETS is not supported")
    if any(a[2] for e in stmt.expressions if (a := _aggregate_of(e)) is not None):
        raise errors.feature_not_supported("DISTINCT aggregate with GROUPING SETS is not supported")
    base_filter = _where_filter(stmt, table)
    sets = _grouping_sets(stmt.args["group"])
    branches = [_grouping_set_branch(stmt, table, gs) for gs in sets]
    pipeline = list(branches[0][0])
    out_columns = branches[0][1]
    prefix = [{"$match": base_filter}] if base_filter else []
    for sub, _cols in branches[1:]:
        pipeline.append({"$unionWith": {"coll": table.collection, "pipeline": prefix + sub}})
    _append_sort_limit(pipeline, stmt, {n for n, _ in out_columns})
    return PipelineSelectPlan(table.collection, base_filter, pipeline, out_columns)


def _plan_group_select(stmt: exp.Select, table: TableDef) -> PipelineSelectPlan:
    # A correlated / EXISTS WHERE can't push to a Mongo filter — carry it for
    # per-base-doc evaluation before the $group (the executor filters, then groups).
    residual = _residual_where(stmt, table)
    base_filter = {} if residual is not None else _where_filter(stmt, table)
    group_node = stmt.args.get("group")
    group_cols = [_column_name(c) for c in group_node.expressions] if group_node else []
    for c in group_cols:
        table.field_for(c)  # validate
    group_id = {c: f"${table.field_for(c)}" for c in group_cols} or None

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    agg_fields: dict[tuple[str, str | None, bool], str] = {}

    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        arr_arg = _array_agg_arg(e)
        agg = _aggregate_of(e)
        if arr_arg is not None:
            fname = names.fresh(alias or "array_agg")
            accumulators[fname] = {"$push": _agg_arg_to_expr(arr_arg, table)}
            project[fname] = f"${fname}"
            out_columns.append((fname, "json"))
        elif agg is not None:
            func, col, distinct = agg
            if distinct and func in _DISTINCT_FUNCS:
                if col is None:
                    raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
                fname, tag = _register_distinct_agg(
                    func,
                    table.field_for(col),
                    table.type_for(col),
                    alias,
                    names,
                    accumulators,
                    reductions,
                )
            else:
                acc, tag = _accumulator(func, col, table)
                fname = names.fresh(alias or func)
                accumulators[fname] = acc
            agg_fields[(func, col, distinct)] = fname
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
    # Reduce any DISTINCT sets to their scalar value before HAVING / projection.
    if reductions:
        pipeline.append({"$addFields": reductions})
    if having_match is not None:
        pipeline.append({"$match": having_match})
    pipeline.append({"$project": project})
    _append_sort_limit(pipeline, stmt, {n for n, _ in out_columns})
    return PipelineSelectPlan(
        table.collection,
        base_filter,
        pipeline,
        out_columns,
        residual_where=residual,
        residual_resolve=table_resolver(table) if residual is not None else None,
    )


def _residual_where(stmt: exp.Select, table: TableDef) -> exp.Expression | None:
    """The WHERE predicate to evaluate per-row (rather than push down) when it
    references the outer row — an ``EXISTS`` or correlated subquery. ``None`` when
    the WHERE (if any) lowers cleanly to a Mongo filter."""
    where = stmt.args.get("where")
    if where is None or not where_needs_per_row(stmt):
        return None
    return where.this


def _select_has_window(stmt: exp.Select) -> bool:
    """Whether any SELECT item or ORDER BY term contains a window (``OVER``)."""
    roots: list[exp.Expression] = list(stmt.expressions)
    order = stmt.args.get("order")
    if order is not None:
        roots.extend(o.this for o in order.expressions)
    return any(next(r.find_all(exp.Window), None) is not None for r in roots)


def _group_agg_nodes(stmt: exp.Select) -> list[exp.AggFunc]:
    """Every aggregate (``SUM``/``COUNT``/… / ``array_agg``) in the SELECT list and
    ORDER BY that is *not* itself a window function — i.e. not the direct operand
    of an ``OVER`` clause. These are the GROUP BY aggregates; a window aggregate
    like ``SUM(...) OVER (...)`` is computed later, over the grouped rows. An
    aggregate nested inside a window aggregate (``SUM(SUM(x)) OVER ()``) is still
    a group aggregate — only the outermost, window-owned one is excluded."""
    roots: list[exp.Expression] = list(stmt.expressions)
    order = stmt.args.get("order")
    if order is not None:
        roots.extend(o.this for o in order.expressions)
    found: list[exp.AggFunc] = []
    for root in roots:
        for n in root.find_all(exp.AggFunc):
            parent = n.parent
            if isinstance(parent, exp.Window) and parent.this is n:
                continue  # a window aggregate — resolved over the grouped rows
            found.append(n)
    return found


def _synthetic_resolver(field_tags: dict[str, str]) -> Resolve:
    """A column resolver over the flat, post-``$group`` document — group columns
    and synthetic aggregate fields resolve to themselves; anything else is a
    non-grouped column reference, which Postgres rejects with 42803."""

    def resolve(node: exp.Expression) -> tuple[str, str]:
        col = _column_name(node)
        if col in field_tags:
            return col, field_tags[col]
        raise errors.SQLError(
            "42803",
            f'column "{col}" must appear in the GROUP BY clause '
            "or be used in an aggregate function",
        )

    return resolve


def _plan_group_window_select(stmt: exp.Select, table: TableDef) -> EvaluatedSelectPlan:
    """GROUP BY (or an implicit whole-table aggregation) combined with window
    functions in the same SELECT — e.g. ``SELECT dept, SUM(sal),
    RANK() OVER (ORDER BY SUM(sal)) FROM emp GROUP BY dept``.

    Phase 1 (aggregation pipeline): a ``$group`` computes the grouping columns and
    every group aggregate into flat fields. Phase 2 (the evaluated executor): the
    window functions run over those grouped rows, and each aggregate reference —
    inside the window's args / PARTITION BY / ORDER BY, or standing alone in the
    SELECT list — resolves to its precomputed field."""
    stmt = stmt.copy()  # we mutate the tree, replacing aggregates with columns
    base_filter = _where_filter(stmt, table)
    group_node = stmt.args.get("group")
    group_cols = [_column_name(c) for c in group_node.expressions] if group_node else []
    for c in group_cols:
        table.field_for(c)  # validate
    group_id = {c: f"${table.field_for(c)}" for c in group_cols} or None

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    names = _NameAllocator()
    for c in group_cols:  # reserve group names so synthetic fields never collide
        names.fresh(c)
    field_tags: dict[str, str] = {c: table.type_for(c) for c in group_cols}
    agg_fields: dict[tuple[str, str | None, bool], str] = {}
    agg_field_names: list[str] = []

    def register_agg(node: exp.AggFunc) -> str:
        arr_arg = _array_agg_arg(node)
        if arr_arg is not None:
            fname = names.fresh("array_agg")
            accumulators[fname] = {"$push": _agg_arg_to_expr(arr_arg, table)}
            field_tags[fname] = "json"
            agg_field_names.append(fname)
            return fname
        agg = _aggregate_of(node)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported aggregate: {node.sql()}")
        if agg in agg_fields:
            return agg_fields[agg]
        func, col, distinct = agg
        if distinct and func in _DISTINCT_FUNCS:
            if col is None:
                raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
            fname, tag = _register_distinct_agg(
                func,
                table.field_for(col),
                table.type_for(col),
                None,
                names,
                accumulators,
                reductions,
            )
        else:
            acc, tag = _accumulator(func, col, table)
            fname = names.fresh(func)
            accumulators[fname] = acc
        agg_fields[agg] = fname
        field_tags[fname] = tag
        agg_field_names.append(fname)
        return fname

    # Replace each group aggregate with a reference to its computed field. The
    # nodes were collected from the original tree (parents intact); group
    # aggregates never nest without a window between them, so replacement order
    # is immaterial.
    for node in _group_agg_nodes(stmt):
        node.replace(exp.column(register_agg(node)))

    having = stmt.args.get("having")
    having_match = (
        _having_to_match(having.this, table, accumulators, agg_fields, group_cols)
        if having is not None
        else None
    )

    pipeline: list[dict[str, Any]] = [{"$group": {"_id": group_id, **accumulators}}]
    if reductions:
        pipeline.append({"$addFields": reductions})
    if having_match is not None:
        pipeline.append({"$match": having_match})
    project: dict[str, Any] = {"_id": 0}
    for c in group_cols:
        project[c] = f"$_id.{c}"
    for fname in agg_field_names:
        project[fname] = f"${fname}"
    pipeline.append({"$project": project})
    return _finish_group_window(stmt, table.collection, base_filter, pipeline, field_tags)


def _finish_group_window(
    stmt: exp.Select,
    base_collection: str,
    base_filter: dict[str, Any],
    pipeline: list[dict[str, Any]],
    field_tags: dict[str, str],
    derived: list[DerivedTable] | None = None,
) -> EvaluatedSelectPlan:
    """Shared tail of the group-then-window planners: with the grouped rows'
    field→tag map in hand, build the per-row output expressions, the window-alias
    aware ORDER BY, and the ``EvaluatedSelectPlan`` that runs the window phase."""
    resolve = _synthetic_resolver(field_tags)
    out_columns: list[tuple[str, str]] = []
    out_exprs: list[exp.Expression] = []
    alias_exprs: dict[str, exp.Expression] = {}
    onames = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
        name = alias or (_column_name(inner) if isinstance(inner, exp.Column) else "?column?")
        out_columns.append((onames.fresh(name), _infer_scalar_tag(inner, resolve)))
        out_exprs.append(inner)
        if alias is not None:
            alias_exprs[alias] = inner

    # ORDER BY may reference a SELECT output alias (``ORDER BY rk``) — Postgres
    # resolves it to that output expression, so sorting on a window/aggregate
    # alias works even though the grouped rows carry no such field.
    order: list[tuple[exp.Expression, int, bool]] = []
    order_node = stmt.args.get("order")
    if order_node is not None:
        for o in order_node.expressions:
            term = o.this
            if isinstance(term, exp.Column) and not term.table and term.name in alias_exprs:
                term = alias_exprs[term.name]
            order.append((term, -1 if o.args.get("desc") else 1, _nulls_first(o)))

    limit, skip = _limit_skip(stmt)
    return EvaluatedSelectPlan(
        base_collection=base_collection,
        base_filter=base_filter,
        pipeline=pipeline,
        out_columns=out_columns,
        out_exprs=out_exprs,
        resolve=resolve,
        order=order,
        distinct=bool(stmt.args.get("distinct")),
        limit=limit,
        skip=skip,
        derived=derived or [],
    )


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
        func, col, distinct = agg
        if distinct and func in _DISTINCT_FUNCS:
            raise errors.feature_not_supported(
                f"DISTINCT inside {func}() is not supported in HAVING"
            )
        acc, tag = _accumulator(func, col, table)
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


def _alias_field_path(amap: dict[str, tuple[str, TableDef]], alias: str, col: str) -> str:
    """Resolve ``alias.col`` to its pipeline field path (bare for the base)."""
    role, tdef = amap[alias]
    field = tdef.field_for(col)
    return field if role == "base" else f"{alias}.{field}"


def _and_conjuncts(node: exp.Expression) -> list[exp.Expression]:
    if isinstance(node, exp.Paren):
        return _and_conjuncts(node.this)
    if isinstance(node, exp.And):
        return _and_conjuncts(node.this) + _and_conjuncts(node.expression)
    return [node]


class _OnTranslator:
    """Translate a (possibly compound) JOIN ON into an aggregation ``$expr`` for
    a ``$lookup`` ``let``/``pipeline`` stage.

    References to the *new* (being-joined) table become ``$field`` paths inside
    the lookup sub-pipeline; references to already-known tables become ``$$vN``
    let variables bound to the outer document's field paths.
    """

    _OPS = {
        exp.EQ: "$eq",
        exp.NEQ: "$ne",
        exp.GT: "$gt",
        exp.GTE: "$gte",
        exp.LT: "$lt",
        exp.LTE: "$lte",
    }

    def __init__(self, new_alias: str, new_table: TableDef, amap: dict[str, tuple[str, TableDef]]):
        self.new_alias = new_alias
        self.new_table = new_table
        self.amap = amap
        self.lets: dict[str, str] = {}  # outer field path -> let var name

    def _let_for(self, path: str) -> str:
        if path not in self.lets:
            self.lets[path] = f"v{len(self.lets)}"
        return self.lets[path]

    def _is_new(self, alias: str | None, name: str) -> bool:
        if alias is not None:
            return alias == self.new_alias
        return self.new_table.column(name) is not None

    def _column(self, node: exp.Column) -> str:
        alias, name = node.table or None, node.name
        if self._is_new(alias, name):
            return f"${self.new_table.field_for(name)}"
        # A known (outer) table reference -> a let-bound variable.
        if alias is None:
            cands = [a for a, (_r, t) in self.amap.items() if t.column(name) is not None]
            if len(cands) != 1:
                raise errors.SQLError("42702", f'column reference "{name}" is ambiguous')
            alias = cands[0]
        if alias not in self.amap:
            raise errors.SQLError("42P01", f'missing FROM-clause entry for table "{alias}"')
        return f"$${self._let_for(_alias_field_path(self.amap, alias, name))}"

    def expr(self, node: exp.Expression) -> Any:
        if isinstance(node, exp.Paren):
            return self.expr(node.this)
        if isinstance(node, exp.Column):
            return self._column(node)
        if isinstance(node, (exp.Literal, exp.Boolean, exp.Null, exp.Neg)):
            return _literal(node)
        if isinstance(node, exp.Cast):
            return _coerce_cast(self.expr(node.this), node.to)
        if isinstance(node, exp.And):
            return {"$and": [self.expr(node.this), self.expr(node.expression)]}
        if isinstance(node, exp.Or):
            return {"$or": [self.expr(node.this), self.expr(node.expression)]}
        if isinstance(node, exp.Not):
            return {"$not": [self.expr(node.this)]}
        if isinstance(node, exp.Is) and isinstance(node.expression, exp.Null):
            return {"$eq": [self.expr(node.this), None]}
        # ``col = ANY(ARRAY[...])`` → ``$in`` (Postgres IN, as in SQLAlchemy's
        # ``contype = ANY(ARRAY['p','u','x'])`` index-reflection join condition).
        if isinstance(node, exp.EQ) and isinstance(node.expression, exp.Any):
            elems = [self.expr(e) for e in _array_elements(node.expression.this)]
            return {"$in": [self.expr(node.this), elems]}
        for cls, op in self._OPS.items():
            if isinstance(node, cls):
                return {op: [self.expr(node.this), self.expr(node.expression)]}
        raise errors.feature_not_supported(f"unsupported JOIN ON term: {node.sql()}")


def _on_is_simple_equality(
    on: exp.Expression, join_alias: str, amap: dict[str, tuple[str, TableDef]]
) -> tuple[str, str, str] | None:
    """If ``on`` is a single equality relating the new table to a known one,
    return (new_col, known_alias, known_col); else None (→ pipeline form)."""
    conjuncts = _and_conjuncts(on)
    if len(conjuncts) != 1 or not isinstance(conjuncts[0], exp.EQ):
        return None
    eq = conjuncts[0]
    la, lc = _alias_col(eq.this)
    ra, rc = _alias_col(eq.expression)
    if la is None or ra is None:
        return None
    if join_alias == la and ra != join_alias and ra in amap:
        return lc, ra, rc
    if join_alias == ra and la != join_alias and la in amap:
        return rc, la, lc
    return None


def _lookup_stage(
    on: exp.Expression, join_alias: str, join_table: TableDef, amap: dict[str, tuple[str, TableDef]]
) -> dict[str, Any]:
    """Build the ``$lookup`` stage for one JOIN.

    A single equality uses the simple ``localField``/``foreignField`` form (so a
    user-table join keeps index acceleration). A compound ON (multi-key join or
    residual predicates on the joined table) uses the ``let``/``pipeline`` form.
    """
    simple = _on_is_simple_equality(on, join_alias, amap)
    if simple is not None:
        new_col, known_alias, known_col = simple
        return {
            "$lookup": {
                "from": join_table.collection,
                "localField": _alias_field_path(amap, known_alias, known_col),
                "foreignField": join_table.field_for(new_col),
                "as": join_alias,
            }
        }
    tr = _OnTranslator(join_alias, join_table, amap)
    cond = tr.expr(on)
    return {
        "$lookup": {
            "from": join_table.collection,
            "let": {var: f"${path}" for path, var in tr.lets.items()},
            "pipeline": [{"$match": {"$expr": cond}}],
            "as": join_alias,
        }
    }


def _lateral_stage(
    lateral: exp.Lateral,
    side: str,
    amap: dict[str, tuple[str, TableDef]],
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
) -> tuple[str, TableDef, list[dict[str, Any]]]:
    """Lower a ``LATERAL (SELECT … FROM inner [WHERE …] [ORDER BY …] [LIMIT n])``
    to a correlated ``$lookup`` + ``$unwind``.

    The subquery may reference columns from the preceding FROM items (that's what
    makes it lateral); those become ``let``-bound ``$$vars`` in the lookup's
    sub-pipeline via ``_OnTranslator`` (the same inner-``$field`` / outer-``$$var``
    split a compound JOIN ON uses). Scope is a single-table subquery with an
    optional WHERE / ORDER BY / LIMIT — a join / GROUP BY / scalar-fn subquery is
    rejected rather than mis-lowered."""
    alias = lateral.alias
    if not alias:
        raise errors.feature_not_supported("a LATERAL subquery requires an alias")
    sub = lateral.this
    if isinstance(sub, exp.Subquery):
        sub = sub.this
    if not isinstance(sub, exp.Select):
        raise errors.feature_not_supported(f"unsupported LATERAL source: {lateral.sql()}")
    has_agg = any(
        _aggregate_of(e) is not None or _array_agg_arg(e) is not None for e in sub.expressions
    )
    if (
        sub.args.get("joins")
        or sub.args.get("group")
        or sub.args.get("having")
        or sub.args.get("distinct")
        or has_agg
        or _stmt_needs_evaluation(sub)
    ):
        raise errors.feature_not_supported(
            "only a single-table LATERAL subquery (projection + WHERE + ORDER BY / LIMIT) "
            "is supported"
        )
    from_node = sub.find(exp.From)
    if from_node is None:
        raise errors.feature_not_supported("a LATERAL subquery requires a FROM clause")
    inner_alias, inner = _resolve_source(from_node.this, db, catalog, storage, derived)
    inner_resolve = table_resolver(inner)

    sub_pipeline: list[dict[str, Any]] = []
    lets: dict[str, str] = {}
    where = sub.args.get("where")
    if where is not None:
        tr = _OnTranslator(inner_alias, inner, amap)
        cond = tr.expr(where.this)
        lets = tr.lets
        sub_pipeline.append({"$match": {"$expr": cond}})
    order = sub.args.get("order")
    if order is not None:
        sort_spec: dict[str, int] = {}
        for o in order.expressions:
            path, _ = inner_resolve(o.this)
            sort_spec[path] = -1 if o.args.get("desc") else 1
        sub_pipeline.append({"$sort": sort_spec})
    limit, skip = _limit_skip(sub)
    if skip:
        sub_pipeline.append({"$skip": skip})
    if limit:
        sub_pipeline.append({"$limit": limit})

    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    for e in sub.expressions:
        col_alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, exp.Star):
            for c in inner.columns:
                nm = names.fresh(c.name)
                project[nm] = f"${c.field}"
                out_columns.append((nm, c.type_tag))
            continue
        path, tag = _field(target, inner_resolve)
        nm = names.fresh(col_alias or _column_name(target))
        project[nm] = f"${path}"
        out_columns.append((nm, tag))
    sub_pipeline.append({"$project": project})

    tdef = TableDef(
        name=alias,
        collection=alias,
        columns=[Column(n, t, n, pk=False, nullable=True) for n, t in out_columns],
    )
    stages: list[dict[str, Any]] = [
        {
            "$lookup": {
                "from": inner.collection,
                "let": {var: f"${path}" for path, var in lets.items()},
                "pipeline": sub_pipeline,
                "as": alias,
            }
        },
        {"$unwind": {"path": f"${alias}", "preserveNullAndEmptyArrays": side == "LEFT"}},
    ]
    return alias, tdef, stages


def _resolve_source(
    node: exp.Expression, db: str, catalog: Any, storage: Any, derived: list[DerivedTable]
) -> tuple[str, TableDef]:
    """Resolve a FROM / JOIN source to (alias, TableDef).

    A plain table resolves through the catalog / virtual / reflection lookup. A
    ``(SELECT ...) AS alias`` derived table is planned as a sub-plan and recorded
    in ``derived`` (the executor materializes it into an ephemeral collection
    named by the alias before running the main pipeline)."""
    if isinstance(node, exp.Lateral):
        raise errors.feature_not_supported("LATERAL cannot be the first FROM item")
    if isinstance(node, exp.Subquery):
        alias = node.alias
        if not alias:
            raise errors.feature_not_supported("a derived table requires an alias")
        sub = node.this
        if not isinstance(sub, exp.Select):
            raise errors.feature_not_supported(f"unsupported derived table: {node.sql()}")
        sub_plan = plan_pipeline_select(sub, db, catalog, storage)
        cols = sub_plan.out_columns
        tdef = TableDef(
            name=alias,
            collection=alias,
            columns=[Column(n, t, n, pk=False, nullable=True) for n, t in cols],
        )
        derived.append(DerivedTable(name=alias, plan=sub_plan, columns=cols))
        return alias, tdef
    tdef = _lookup_table_def(catalog, db, node, storage)
    if tdef is None:
        raise errors.undefined_table(node.name)
    return (node.alias or node.name), tdef


def _build_join_pipeline(
    stmt: exp.Select, db: str, catalog: Any, storage: Any
) -> tuple[
    TableDef, dict[str, tuple[str, TableDef]], Resolve, list[dict[str, Any]], list[DerivedTable]
]:
    """Build the $lookup/$unwind (+ WHERE $match) prefix shared by the join
    builders. Returns (base, amap, resolve, pipeline, derived)."""
    derived: list[DerivedTable] = []
    fr = stmt.find(exp.From).this
    base_alias, base = _resolve_source(fr, db, catalog, storage, derived)
    joins = stmt.args["joins"]

    # ``$lookup`` is inherently left-driven (for each base doc, fetch matching
    # foreign docs), so RIGHT / FULL OUTER need the base swapped and (for FULL) an
    # anti-join union. That only composes cleanly for a single two-table join — a
    # chain mixing in a RIGHT/FULL is rejected rather than silently mis-joined.
    if any(str(jn.args.get("side") or "").upper() in ("RIGHT", "FULL") for jn in joins):
        if len(joins) != 1:
            raise errors.feature_not_supported(
                "RIGHT / FULL OUTER JOIN is only supported between two tables"
            )
        return _build_outer_join_pipeline(
            stmt, base_alias, base, joins[0], db, catalog, storage, derived
        )

    amap: dict[str, tuple[str, TableDef]] = {base_alias: ("base", base)}
    pipeline: list[dict[str, Any]] = []

    # Each JOIN compiles to a $lookup + $unwind. The lookup's localField may point
    # into an already-joined alias (a chain like a⋈b⋈c where c joins on b), which
    # Mongo's dotted localField handles since b was unwound into the doc.
    for jn in joins:
        jt = jn.this
        side = str(jn.args.get("side") or "").upper()
        on = jn.args.get("on")
        if isinstance(jt, exp.Lateral):
            # A LATERAL subquery correlates *inside* itself (its WHERE references
            # outer columns), so the join ON is only ever TRUE (or absent for the
            # comma / CROSS form). A real ON predicate here isn't supported.
            if on is not None and not (isinstance(on, exp.Boolean) and bool(on.this)):
                raise errors.feature_not_supported(
                    "LATERAL join ON must be TRUE — correlate inside the subquery's WHERE"
                )
            lat_alias, lat_table, stages = _lateral_stage(
                jt, side, amap, db, catalog, storage, derived
            )
            pipeline.extend(stages)
            amap[lat_alias] = ("join", lat_table)
            continue
        join_alias, join_table = _resolve_source(jt, db, catalog, storage, derived)
        if on is None:
            # No ON: a CROSS JOIN or an implicit comma-join — the cartesian
            # product (an empty `$lookup` pipeline returns every foreign doc, then
            # `$unwind` pairs each with the outer row). An outer join without ON is
            # not valid SQL.
            if side in ("LEFT", "RIGHT", "FULL"):
                raise errors.syntax_error(f"{side} JOIN requires an ON clause")
            pipeline.append(
                {"$lookup": {"from": join_table.collection, "pipeline": [], "as": join_alias}}
            )
            pipeline.append(
                {"$unwind": {"path": f"${join_alias}", "preserveNullAndEmptyArrays": False}}
            )
            amap[join_alias] = ("join", join_table)
            continue
        pipeline.append(_lookup_stage(on, join_alias, join_table, amap))
        pipeline.append(
            {"$unwind": {"path": f"${join_alias}", "preserveNullAndEmptyArrays": side == "LEFT"}}
        )
        amap[join_alias] = ("join", join_table)

    resolve = _join_resolver(amap)
    where = stmt.args.get("where")
    # A correlated / EXISTS WHERE is left for per-row evaluation (see
    # ``_build_evaluated_join``); only a pushdown-able WHERE becomes a ``$match``.
    if where is not None and not where_needs_per_row(stmt):
        pipeline.append({"$match": _expr_to_filter(where.this, resolve, _pipeline_subctx.get())})
    return base, amap, resolve, pipeline, derived


def _build_outer_join_pipeline(
    stmt: exp.Select,
    a_alias: str,
    a_table: TableDef,
    jn: exp.Expression,
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
) -> tuple[
    TableDef, dict[str, tuple[str, TableDef]], Resolve, list[dict[str, Any]], list[DerivedTable]
]:
    """Build the prefix for a single ``A <RIGHT|FULL> JOIN B ON …``.

    ``A RIGHT JOIN B`` is ``B LEFT JOIN A``: drive the pipeline from B, look A up,
    and preserve unmatched B rows. ``A FULL JOIN B`` is the LEFT join from A
    (preserving unmatched A) unioned with the B rows that found no A match
    (reshaped so B's columns sit under its alias and A's columns read as NULL).
    The ``amap`` is always inserted in FROM order (A then B) so ``SELECT *`` keeps
    Postgres's left-to-right column order regardless of which side drives."""
    side = str(jn.args.get("side") or "").upper()
    b_alias, b_table = _resolve_source(jn.this, db, catalog, storage, derived)
    on = jn.args.get("on")
    if on is None:
        raise errors.feature_not_supported("JOIN without ON is not supported")

    if side == "RIGHT":
        amap = {a_alias: ("join", a_table), b_alias: ("base", b_table)}
        pipeline = [
            _lookup_stage(on, a_alias, a_table, amap),
            {"$unwind": {"path": f"${a_alias}", "preserveNullAndEmptyArrays": True}},
        ]
        base = b_table
    else:  # FULL
        amap = {a_alias: ("base", a_table), b_alias: ("join", b_table)}
        pipeline = [
            _lookup_stage(on, b_alias, b_table, amap),
            {"$unwind": {"path": f"${b_alias}", "preserveNullAndEmptyArrays": True}},
            {
                "$unionWith": {
                    "coll": b_table.collection,
                    "pipeline": _full_join_anti_branch(on, a_alias, a_table, b_alias, b_table),
                }
            },
        ]
        base = a_table

    resolve = _join_resolver(amap)
    where = stmt.args.get("where")
    if where is not None and not where_needs_per_row(stmt):
        pipeline.append({"$match": _expr_to_filter(where.this, resolve, _pipeline_subctx.get())})
    return base, amap, resolve, pipeline, derived


def _full_join_anti_branch(
    on: exp.Expression, a_alias: str, a_table: TableDef, b_alias: str, b_table: TableDef
) -> list[dict[str, Any]]:
    """The FULL-join's right anti-join arm: B rows with no A match, reshaped to the
    main branch's layout. Driving from B, look A up; keep only the B rows whose
    lookup came back empty; then nest the whole B doc under its alias so ``b.col``
    paths resolve and A's (base) bare-field paths are absent (→ NULL)."""
    amap_b = {b_alias: ("base", b_table)}
    return [
        _lookup_stage(on, a_alias, a_table, amap_b),
        {"$match": {a_alias: {"$size": 0}}},
        {"$replaceWith": {b_alias: "$$ROOT"}},
    ]


def _plan_join_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> PipelineSelectPlan | EvaluatedSelectPlan:
    base, amap, resolve, pipeline, derived = _build_join_pipeline(stmt, db, catalog, storage)

    # A scalar SELECT list / ORDER BY, a correlated / EXISTS WHERE (which the
    # pipeline builder deliberately left un-pushed), or DISTINCT ON (keep-first
    # per key) all need the per-row evaluator.
    if _stmt_needs_evaluation(stmt) or where_needs_per_row(stmt) or _distinct_on(stmt):
        return _build_evaluated_join(stmt, base, amap, resolve, pipeline, derived)

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
    _append_join_tail(pipeline, stmt, resolve, project, out_columns)
    return PipelineSelectPlan(base.collection, {}, pipeline, out_columns, derived=derived)


def _join_aggregate_of(
    node: exp.Expression,
) -> tuple[str, exp.Expression | None, bool] | None:
    """Like ``_aggregate_of`` but keeps the argument NODE so the join resolver can
    map a qualified column (``b.amt``). Returns ``(func, arg_node, distinct)``; a
    None argument means ``COUNT(*)`` and a ``DISTINCT`` argument is unwrapped."""
    inner = node.this if isinstance(node, exp.Alias) else node
    for cls, name in _AGG_CLASSES.items():
        if isinstance(inner, cls):
            arg = inner.this
            distinct = isinstance(arg, exp.Distinct)
            if distinct:
                exprs = arg.expressions
                arg = exprs[0] if exprs else None
            return name, (None if arg is None or isinstance(arg, exp.Star) else arg), distinct
    return None


def _join_accumulator(
    func: str, arg: exp.Expression | None, resolve: Resolve
) -> tuple[dict[str, Any], str]:
    if arg is None:
        return _accumulator_for(func, None, None)
    path, tag = resolve(arg)
    return _accumulator_for(func, path, tag)


def _agg_key(
    func: str, arg: exp.Expression | None, resolve: Resolve, distinct: bool = False
) -> str:
    """A hashable identity for an aggregate (for HAVING accumulator dedup)."""
    return f"{func}:{'d' if distinct else ''}:{'*' if arg is None else resolve(arg)[0]}"


def _plan_join_group_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> PipelineSelectPlan:
    """JOIN combined with GROUP BY / aggregates: build the $lookup/$unwind/$match
    prefix, then a $group whose keys and accumulators resolve through the join
    resolver (so ``a.region`` / ``SUM(b.amt)`` map to the post-unwind paths)."""
    base, _amap, resolve, pipeline, derived = _build_join_pipeline(stmt, db, catalog, storage)
    # A correlated / EXISTS WHERE wasn't pushed into a ``$match`` (see
    # ``_build_join_pipeline``); it's filtered per joined row after the join prefix
    # and before the ``$group`` below. ``residual_split`` marks that boundary.
    where_node = stmt.args.get("where")
    residual = where_node.this if (where_node is not None and where_needs_per_row(stmt)) else None
    residual_split = len(pipeline)

    group_node = stmt.args.get("group")
    group_keys: dict[str, str] = {}  # _id key name -> resolved "$path"
    key_tag: dict[str, str] = {}
    if group_node is not None:
        for c in group_node.expressions:
            if not isinstance(c, exp.Column):
                raise errors.feature_not_supported(f"GROUP BY expression not supported: {c.sql()}")
            keyname = _column_name(c)
            path, tag = resolve(c)
            group_keys[keyname] = f"${path}"
            key_tag[keyname] = tag
    group_id = group_keys or None

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    agg_fields: dict[str, str] = {}

    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        arr_arg = _array_agg_arg(e)
        agg = _join_aggregate_of(e)
        if arr_arg is not None:
            fname = names.fresh(alias or "array_agg")
            path, _ = resolve(arr_arg)
            accumulators[fname] = {"$push": f"${path}"}
            project[fname] = f"${fname}"
            out_columns.append((fname, "json"))
        elif agg is not None:
            func, arg, distinct = agg
            if distinct and func in _DISTINCT_FUNCS:
                if arg is None:
                    raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
                path, tag = resolve(arg)
                fname, tag = _register_distinct_agg(
                    func, path, tag, alias, names, accumulators, reductions
                )
            else:
                acc, tag = _join_accumulator(func, arg, resolve)
                fname = names.fresh(alias or func)
                accumulators[fname] = acc
            agg_fields[_agg_key(func, arg, resolve, distinct)] = fname
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        else:
            inner = e.this if isinstance(e, exp.Alias) else e
            if isinstance(inner, exp.Star):
                raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
            if not isinstance(inner, exp.Column):
                raise errors.feature_not_supported(
                    f"non-aggregate SELECT expression not supported with GROUP BY: {inner.sql()}"
                )
            keyname = _column_name(inner)
            if keyname not in group_keys:
                raise errors.SQLError(
                    "42803",
                    f'column "{keyname}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            out_name = names.fresh(alias or keyname)
            project[out_name] = f"$_id.{keyname}"
            out_columns.append((out_name, key_tag[keyname]))

    having = stmt.args.get("having")
    having_match = (
        _join_having_to_match(having.this, resolve, accumulators, agg_fields, group_keys, key_tag)
        if having is not None
        else None
    )
    pipeline.append({"$group": {"_id": group_id, **accumulators}})
    if reductions:
        pipeline.append({"$addFields": reductions})
    if having_match is not None:
        pipeline.append({"$match": having_match})
    pipeline.append({"$project": project})
    _append_sort_limit(pipeline, stmt, {n for n, _ in out_columns})
    return PipelineSelectPlan(
        base.collection,
        {},
        pipeline,
        out_columns,
        derived=derived,
        residual_where=residual,
        residual_resolve=resolve if residual is not None else None,
        residual_split=residual_split,
    )


def _plan_join_group_window_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> EvaluatedSelectPlan:
    """JOIN + GROUP BY combined with window functions — the join analogue of
    ``_plan_group_window_select``. The $lookup/$unwind/$match/$group/$project
    pipeline produces the grouped rows (aggregates resolved through the join
    resolver), then the evaluated executor runs the windows over them."""
    if where_needs_per_row(stmt):
        # A correlated / EXISTS WHERE would need per-joined-row filtering before the
        # $group, which the window phase (post-group) can't express here.
        raise errors.feature_not_supported(
            "a correlated / EXISTS WHERE combined with JOIN, GROUP BY, and a window "
            "function in one SELECT is not supported"
        )
    stmt = stmt.copy()  # we mutate the tree, replacing aggregates with columns
    base, _amap, resolve, pipeline, derived = _build_join_pipeline(stmt, db, catalog, storage)

    group_node = stmt.args.get("group")
    group_keys: dict[str, str] = {}
    key_tag: dict[str, str] = {}
    field_tags: dict[str, str] = {}
    names = _NameAllocator()
    if group_node is not None:
        for c in group_node.expressions:
            if not isinstance(c, exp.Column):
                raise errors.feature_not_supported(f"GROUP BY expression not supported: {c.sql()}")
            keyname = _column_name(c)
            path, tag = resolve(c)
            group_keys[keyname] = f"${path}"
            key_tag[keyname] = tag
            field_tags[keyname] = tag
            names.fresh(keyname)  # reserve so synthetic agg fields never collide
    group_id = group_keys or None

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    agg_fields: dict[str, str] = {}  # _agg_key -> field name
    agg_field_names: list[str] = []

    def register_agg(node: exp.AggFunc) -> str:
        arr_arg = _array_agg_arg(node)
        if arr_arg is not None:
            fname = names.fresh("array_agg")
            path, _ = resolve(arr_arg)
            accumulators[fname] = {"$push": f"${path}"}
            field_tags[fname] = "json"
            agg_field_names.append(fname)
            return fname
        agg = _join_aggregate_of(node)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported aggregate: {node.sql()}")
        func, arg, distinct = agg
        key = _agg_key(func, arg, resolve, distinct)
        if key in agg_fields:
            return agg_fields[key]
        if distinct and func in _DISTINCT_FUNCS:
            if arg is None:
                raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
            path, tag = resolve(arg)
            fname, tag = _register_distinct_agg(
                func, path, tag, None, names, accumulators, reductions
            )
        else:
            acc, tag = _join_accumulator(func, arg, resolve)
            fname = names.fresh(func)
            accumulators[fname] = acc
        agg_fields[key] = fname
        field_tags[fname] = tag
        agg_field_names.append(fname)
        return fname

    for node in _group_agg_nodes(stmt):
        node.replace(exp.column(register_agg(node)))

    having = stmt.args.get("having")
    having_match = (
        _join_having_to_match(having.this, resolve, accumulators, agg_fields, group_keys, key_tag)
        if having is not None
        else None
    )
    pipeline.append({"$group": {"_id": group_id, **accumulators}})
    if reductions:
        pipeline.append({"$addFields": reductions})
    if having_match is not None:
        pipeline.append({"$match": having_match})
    project: dict[str, Any] = {"_id": 0}
    for keyname in group_keys:
        project[keyname] = f"$_id.{keyname}"
    for fname in agg_field_names:
        project[fname] = f"${fname}"
    pipeline.append({"$project": project})
    return _finish_group_window(stmt, base.collection, {}, pipeline, field_tags, derived)


def _join_having_to_match(
    node: exp.Expression,
    resolve: Resolve,
    accumulators: dict[str, Any],
    agg_fields: dict[str, str],
    group_keys: dict[str, str],
    key_tag: dict[str, str],
) -> dict[str, Any]:
    """HAVING for the JOIN+GROUP path — mirrors ``_having_to_match`` but resolves
    columns / aggregate args through the join resolver."""
    rec = _join_having_to_match
    if isinstance(node, exp.Paren):
        return rec(node.this, resolve, accumulators, agg_fields, group_keys, key_tag)
    if isinstance(node, exp.And):
        return _merge_and(
            [
                rec(node.this, resolve, accumulators, agg_fields, group_keys, key_tag),
                rec(node.expression, resolve, accumulators, agg_fields, group_keys, key_tag),
            ]
        )
    if isinstance(node, exp.Or):
        return {
            "$or": [
                rec(node.this, resolve, accumulators, agg_fields, group_keys, key_tag),
                rec(node.expression, resolve, accumulators, agg_fields, group_keys, key_tag),
            ]
        }

    def field_tag(term: exp.Expression) -> tuple[str, str]:
        if isinstance(term, exp.Column):
            keyname = _column_name(term)
            if keyname not in group_keys:
                raise errors.SQLError(
                    "42803",
                    f'column "{keyname}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            return f"_id.{keyname}", key_tag[keyname]
        agg = _join_aggregate_of(term)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported HAVING term: {term.sql()}")
        func, arg, distinct = agg
        if distinct and func in _DISTINCT_FUNCS:
            raise errors.feature_not_supported(
                f"DISTINCT inside {func}() is not supported in HAVING"
            )
        acc, tag = _join_accumulator(func, arg, resolve)
        key = _agg_key(func, arg, resolve, distinct)
        if key not in agg_fields:
            fname = f"__having_{len(agg_fields)}"
            accumulators[fname] = acc
            agg_fields[key] = fname
        return agg_fields[key], tag

    if isinstance(node, (exp.EQ, exp.NEQ)) or type(node) in _HAVING_CMP:
        left, right = node.this, node.expression
        term, lit, on_left = left, right, True
        if not isinstance(left, (exp.Column, *_AGG_CLASSES.keys())):
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


def _is_simple_projection(node: exp.Expression) -> bool:
    """A SELECT item that lowers to a plain ``$project`` field (no per-row eval)."""
    inner = node.this if isinstance(node, exp.Alias) else node
    return isinstance(inner, (exp.Column, exp.Star, *_JSONB_CLASSES))


def _stmt_needs_evaluation(stmt: exp.Select) -> bool:
    """Whether a SELECT list / ORDER BY needs Python per-row evaluation
    (set-returning or scalar functions, CASE, scalar subqueries) rather than a
    plain ``$project`` / ``$group``. Aggregates and ``array_agg`` are handled by
    the group/find paths, not per-row eval, so they don't count here."""
    for e in stmt.expressions:
        if (
            _is_simple_projection(e)
            or _aggregate_of(e) is not None
            or _array_agg_arg(e) is not None
        ):
            continue
        return True
    order = stmt.args.get("order")
    if order is not None:
        return any(not isinstance(o.this, exp.Column) for o in order.expressions)
    return False


_BOOL_EXPR_TYPES = (
    exp.Is,
    exp.Not,
    exp.And,
    exp.Or,
    exp.In,
    exp.Boolean,
    exp.EQ,
    exp.NEQ,
    exp.GT,
    exp.GTE,
    exp.LT,
    exp.LTE,
    exp.Like,
    exp.ILike,
)


def _infer_scalar_tag(node: exp.Expression, resolve: Resolve) -> str:
    """Best-effort output type tag for a computed SELECT expression."""
    if isinstance(node, exp.Paren):
        return _infer_scalar_tag(node.this, resolve)
    if isinstance(node, (exp.Literal, exp.Boolean, exp.Null, exp.Neg)):
        # A bare literal in the SELECT list (``SELECT 0 AS lvl``) must type from its
        # value, else an int rides the wire as text.
        return _infer_value_tag(_literal(node))
    if isinstance(node, exp.Window):
        func = node.this
        if isinstance(func, (exp.RowNumber, exp.Rank, exp.DenseRank, exp.Count, exp.Ntile)):
            return "int8"
        if isinstance(func, exp.Avg):
            return "float8"
        value_funcs = (
            exp.Sum,
            exp.Min,
            exp.Max,
            exp.Lag,
            exp.Lead,
            exp.FirstValue,
            exp.LastValue,
            exp.NthValue,
        )
        if isinstance(func, value_funcs) and func.this is not None:
            return _infer_scalar_tag(func.this, resolve)
        return "numeric"
    srf = _srf_of(node)
    if srf is not None:
        # jsonb_array_elements → json elements; jsonb_object_keys → text keys;
        # unnest(indkey/indclass) → attnum/opclass oid; generate_subscripts → ord.
        return {"jsonb_array_elements": "json", "jsonb_object_keys": "text"}.get(srf[0], "int4")
    # A boolean-producing expression (IS NOT NULL, comparisons, AND/OR) must type
    # as bool, not text — else its value rides the wire as the string 'f'/'t' and
    # a driver reads ``if row["x"]`` as truthy (SQLAlchemy's duplicates_constraint).
    if isinstance(node, _BOOL_EXPR_TYPES):
        return "bool"
    if isinstance(
        node,
        (
            exp.Add,
            exp.Sub,
            exp.Mul,
            exp.Div,
            exp.Mod,
            exp.Abs,
            exp.Round,
            exp.Ceil,
            exp.Floor,
            exp.Pow,
        ),
    ):
        return "numeric"
    if isinstance(node, (exp.DPipe, exp.Upper, exp.Lower, exp.Trim, exp.Substring, exp.Concat)):
        return "text"
    if isinstance(node, exp.Length):
        return "int4"
    if isinstance(node, (exp.Coalesce, exp.Greatest, exp.Least)):
        # Type from the first operand (its own tag, recursively).
        first = (
            node.this
            if node.this is not None
            else (node.expressions[0] if node.expressions else None)
        )
        return _infer_scalar_tag(first, resolve) if first is not None else "text"
    if isinstance(node, exp.Nullif):
        return _infer_scalar_tag(node.this, resolve)
    if isinstance(node, (exp.Column, *_JSONB_CLASSES)):
        try:
            return _field(node, resolve)[1]
        except errors.SQLError:
            return "text"
    name = None
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        name = node.expression.name
    elif isinstance(node, exp.Anonymous):
        name = node.this if isinstance(node.this, str) else node.name
    if name is not None:
        fname = str(name).rsplit(".", 1)[-1].lower()
        if fname in (
            "json_build_object",
            "jsonb_build_object",
            "json_build_array",
            "jsonb_build_array",
        ):
            return "json"
        if fname in ("jsonb_array_length", "json_array_length"):
            return "int4"
    return "text"


def _build_evaluated_join(
    stmt: exp.Select,
    base: TableDef,
    amap: dict[str, tuple[str, TableDef]],
    resolve: Resolve,
    pipeline: list[dict[str, Any]],
    derived: list[DerivedTable],
) -> EvaluatedSelectPlan:
    out_columns: list[tuple[str, str]] = []
    out_exprs: list[exp.Expression] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for a, (_role, tdef) in amap.items():
                for c in tdef.columns:
                    out_columns.append((names.fresh(c.name), c.type_tag))
                    out_exprs.append(exp.column(c.name, table=a))
            continue
        if isinstance(inner, exp.Column):
            name = alias or _column_name(inner)
        else:
            name = alias or "?column?"
        out_columns.append((names.fresh(name), _infer_scalar_tag(inner, resolve)))
        out_exprs.append(inner)

    order: list[tuple[exp.Expression, int, bool]] = []
    order_node = stmt.args.get("order")
    if order_node is not None:
        for o in order_node.expressions:
            order.append((o.this, -1 if o.args.get("desc") else 1, _nulls_first(o)))
    limit, skip = _limit_skip(stmt)
    # A correlated / EXISTS WHERE wasn't pushed into the pipeline (see
    # ``_build_join_pipeline``); carry it for per-joined-row evaluation.
    where_node = stmt.args.get("where")
    residual = where_node.this if (where_node is not None and where_needs_per_row(stmt)) else None
    don = _distinct_on(stmt)
    return EvaluatedSelectPlan(
        base_collection=base.collection,
        base_filter={},
        pipeline=pipeline,
        out_columns=out_columns,
        out_exprs=out_exprs,
        resolve=resolve,
        order=order,
        distinct=bool(stmt.args.get("distinct")) and not don,
        limit=limit,
        skip=skip,
        derived=derived,
        where=residual,
        distinct_on=don,
    )


def _append_join_tail(
    pipeline: list[dict[str, Any]],
    stmt: exp.Select,
    resolve: Resolve,
    project: dict[str, Any],
    out_columns: list[tuple[str, str]],
) -> None:
    """Project, optionally dedup (DISTINCT), then sort/skip/limit for a join.

    ORDER BY may reference a column that isn't in the SELECT list (legal in
    Postgres for a non-DISTINCT query): such a column is carried as a hidden
    projected field, sorted on, then dropped by a final projection. With
    DISTINCT the ordering must be by a selected output column (Postgres' rule).
    """
    out_names = {n for n, _ in out_columns}
    distinct = bool(stmt.args.get("distinct"))
    order = stmt.args.get("order")
    terms: list[tuple[str, int, bool]] = []
    hidden: list[str] = []
    if order is not None:
        for o in order.expressions:
            direction = -1 if o.args.get("desc") else 1
            name = _column_name(o.this)
            if name in out_names:
                key = name
            elif distinct:
                raise errors.undefined_column(name)
            else:
                path, _ = resolve(o.this)
                key = f"__ord_{len(hidden)}"
                project[key] = f"${path}"
                hidden.append(key)
            terms.append((key, direction, _nulls_first(o)))
    pipeline.append({"$project": project})
    if distinct:
        _append_distinct(pipeline, out_columns)
    _emit_pipeline_sort(pipeline, terms)
    limit, skip = _limit_skip(stmt)
    if skip:
        pipeline.append({"$skip": skip})
    if limit:
        pipeline.append({"$limit": limit})
    if hidden:
        pipeline.append({"$project": {**{n: 1 for n in out_names}, "_id": 0}})


def _append_distinct(pipeline: list[dict[str, Any]], out_columns: list[tuple[str, str]]) -> None:
    """Append a dedup stage: group by every projected column, then re-project.

    Runs after the `$project` that produces the output columns, so it dedups on
    exactly the selected values (SQL ``DISTINCT`` semantics).
    """
    names = [n for n, _ in out_columns]
    group_id = {n: f"${n}" for n in names}
    project: dict[str, Any] = {"_id": 0}
    for n in names:
        project[n] = f"$_id.{n}"
    pipeline.append({"$group": {"_id": group_id}})
    pipeline.append({"$project": project})


def _append_sort_limit(
    pipeline: list[dict[str, Any]], stmt: exp.Expression, valid_names: set[str]
) -> None:
    order = stmt.args.get("order")
    if order is not None:
        terms: list[tuple[str, int, bool]] = []
        for o in order.expressions:
            col = _column_name(o.this)
            if col not in valid_names:
                raise errors.undefined_column(col)
            terms.append((col, -1 if o.args.get("desc") else 1, _nulls_first(o)))
        _emit_pipeline_sort(pipeline, terms)
    limit, skip = _limit_skip(stmt)
    if skip:
        pipeline.append({"$skip": skip})
    if limit:
        pipeline.append({"$limit": limit})


def _normalize_params(sql: str) -> str:
    """Space-pad ``$N`` placeholders so sqlglot doesn't misread ``$1,$2``.

    sqlglot's Postgres tokenizer treats ``$1,$2`` (adjacent, no spaces — what
    psycopg / pg8000 emit) as the start of a dollar-quoted string. A ``$``
    followed by digits is unambiguously a bind parameter (Postgres dollar-quote
    tags can't begin with a digit), so we append a space after each one. String
    literals are skipped so a ``'$1'`` inside data is left untouched.
    """
    if "$" not in sql:
        return sql
    out: list[str] = []
    i, n, in_str = 0, len(sql), False
    while i < n:
        ch = sql[i]
        if in_str:
            out.append(ch)
            if ch == "'":
                if i + 1 < n and sql[i + 1] == "'":  # '' escape
                    out.append("'")
                    i += 2
                    continue
                in_str = False
            i += 1
            continue
        if ch == "'":
            in_str = True
            out.append(ch)
            i += 1
            continue
        if ch == "$" and i + 1 < n and sql[i + 1].isdigit():
            j = i + 1
            while j < n and sql[j].isdigit():
                j += 1
            out.append(sql[i:j])
            out.append(" ")
            i = j
            continue
        out.append(ch)
        i += 1
    return "".join(out)


_RELEASE_SAVEPOINT_RE = re.compile(r"(?i)\brelease\s+savepoint\b")
# sqlglot's Postgres dialect can't parse ``MOVE`` (cursor positioning) at all, so
# a lone MOVE statement is hand-built into the same ``Command`` shape FETCH gets.
_MOVE_RE = re.compile(r"^\s*MOVE\b\s*(?P<tail>.*?)\s*;?\s*$", re.IGNORECASE | re.DOTALL)
#: Placeholder substituted for a ``COMMENT ON … IS NULL`` (comment removal);
#: ``executor.execute_comment`` reads it back as "remove the comment".
UNCOMMENT_SENTINEL = "\x00__secantus_uncomment__"
# Only a whole ``COMMENT ON … IS NULL`` statement — anchored so a query's
# ``WHERE x IS NULL`` is never touched.
_COMMENT_NULL_RE = re.compile(r"(?is)^(\s*COMMENT\s+ON\b.*\bIS\s+)NULL(\s*;?\s*)$")


def parse(sql: str) -> list[exp.Expression]:
    """Parse a (possibly multi-statement) SQL string into AST statements."""
    move = _MOVE_RE.match(sql)
    if move is not None:
        return [exp.Command(this="MOVE", expression=exp.Literal.string(move.group("tail")))]
    # sqlglot parses ``RELEASE x`` but not the equivalent ``RELEASE SAVEPOINT x``
    # (the standard form SQLAlchemy / psycopg emit) — drop the redundant keyword.
    # Savepoint commands are standalone, so this can't touch a string literal.
    sql = _RELEASE_SAVEPOINT_RE.sub("RELEASE", sql)
    # sqlglot can't parse ``COMMENT ON … IS NULL`` (it requires a string
    # expression), so a NULL comment (comment removal) is rewritten to a sentinel
    # the executor reads back as "remove". COMMENT statements are standalone.
    sql = _COMMENT_NULL_RE.sub(lambda m: f"{m.group(1)}'{UNCOMMENT_SENTINEL}'{m.group(2)}", sql)
    try:
        return [s for s in sqlglot.parse(_normalize_params(sql), read="postgres") if s is not None]
    except sqlglot.errors.ParseError as exc:
        raise errors.syntax_error(str(exc).splitlines()[0]) from exc
