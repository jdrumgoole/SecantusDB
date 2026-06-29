"""Per-row evaluation of scalar SQL expressions in a SELECT list / ORDER BY.

The ``find`` / aggregation-pipeline paths cover column projection and joins, but a
few clients — notably SQLAlchemy's reflection and ``psql``'s ``\\d`` — issue
catalog queries whose SELECT list contains *computed* scalars: catalog functions
(``format_type``, ``pg_get_expr``), ``CASE`` expressions, and correlated scalar
subqueries. Those can't be lowered to a Mongo ``$project``, so the join pipeline
produces the joined rows and this module evaluates each output expression in
Python, per row.

It is deliberately read-only and side-effect-free: a subquery reads rows through
the same storage view as the outer query. ``evaluate(node, scope, ctx)`` returns
a plain Python value; ``scope`` resolves a column reference to its value in the
current row (with outer-row fallthrough for correlation).
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from sqlglot import exp

from secantus.paths import get_path
from secantus.sql import errors, typemap

# A scope resolves a column reference node to its value in the current row.
Scope = Callable[[exp.Column], Any]


@dataclass
class ScalarContext:
    """Carries what a correlated subquery needs to read inner-table rows."""

    storage: Any
    catalog: Any
    db: str
    session: Any


# OID -> SQL type name, inverted from the canonical tag tables, for format_type.
_OID_TO_TYPENAME: dict[int, str] = {
    oid: typemap.SQL_TYPE_NAME.get(tag, tag) for tag, oid in typemap.PG_OID.items()
}


def evaluate(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """Evaluate a scalar expression node to a Python value."""
    if isinstance(node, exp.Paren):
        return evaluate(node.this, scope, ctx)
    if isinstance(node, exp.Alias):
        return evaluate(node.this, scope, ctx)
    if isinstance(node, exp.Null):
        return None
    if isinstance(node, exp.Boolean):
        return bool(node.this)
    if isinstance(node, exp.Literal):
        if node.is_string:
            return node.this
        text = node.this
        return float(text) if ("." in text or "e" in text.lower()) else int(text)
    if isinstance(node, exp.Neg):
        v = evaluate(node.this, scope, ctx)
        return None if v is None else -v
    if isinstance(node, exp.Cast):
        return _eval_cast(node, scope, ctx)
    if isinstance(node, exp.Column):
        return scope(node)
    if isinstance(node, exp.Case):
        return _eval_case(node, scope, ctx)
    if isinstance(node, exp.If):
        # CASE WHEN x THEN y (no ELSE) parses to If in some shapes.
        cond = evaluate(node.this, scope, ctx)
        if _truthy(cond):
            return evaluate(node.args["true"], scope, ctx)
        false = node.args.get("false")
        return evaluate(false, scope, ctx) if false is not None else None
    if isinstance(node, exp.Is):
        left = evaluate(node.this, scope, ctx)
        if isinstance(node.expression, exp.Null):
            return left is None
        return left == evaluate(node.expression, scope, ctx)
    if isinstance(node, exp.Not):
        return not _truthy(evaluate(node.this, scope, ctx))
    if isinstance(node, exp.And):
        return _truthy(evaluate(node.this, scope, ctx)) and _truthy(
            evaluate(node.expression, scope, ctx)
        )
    if isinstance(node, exp.Or):
        return _truthy(evaluate(node.this, scope, ctx)) or _truthy(
            evaluate(node.expression, scope, ctx)
        )
    if isinstance(node, (exp.EQ, exp.NEQ, exp.GT, exp.GTE, exp.LT, exp.LTE)):
        return _eval_compare(node, scope, ctx)
    # Schema-qualified function: pg_catalog.format_type(...) -> the call.
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        return _eval_func(node.expression, scope, ctx)
    if isinstance(node, exp.Anonymous):
        return _eval_func(node, scope, ctx)
    if isinstance(node, exp.Func):
        return _eval_typed_func(node, scope, ctx)
    if isinstance(node, (exp.Select, exp.Subquery)):
        return _eval_subquery(node, scope, ctx)
    raise errors.feature_not_supported(f"unsupported scalar expression: {node.sql()}")


def _truthy(value: Any) -> bool:
    """SQL boolean coercion — NULL/unknown is falsy in a predicate context."""
    return bool(value) if value is not None else False


def _eval_compare(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    left = evaluate(node.this, scope, ctx)
    right = evaluate(node.expression, scope, ctx)
    if left is None or right is None:
        return None  # three-valued logic: comparison with NULL is unknown
    if isinstance(node, exp.EQ):
        return left == right
    if isinstance(node, exp.NEQ):
        return left != right
    if isinstance(node, exp.GT):
        return left > right
    if isinstance(node, exp.GTE):
        return left >= right
    if isinstance(node, exp.LT):
        return left < right
    return left <= right


def _eval_case(node: exp.Case, scope: Scope, ctx: ScalarContext) -> Any:
    base = node.args.get("this")
    base_val = evaluate(base, scope, ctx) if base is not None else None
    for branch in node.args.get("ifs", []):
        cond = branch.this
        if base is not None:
            matched = base_val == evaluate(cond, scope, ctx)
        else:
            matched = _truthy(evaluate(cond, scope, ctx))
        if matched:
            return evaluate(branch.args["true"], scope, ctx)
    default = node.args.get("default")
    return evaluate(default, scope, ctx) if default is not None else None


def _eval_cast(node: exp.Cast, scope: Scope, ctx: ScalarContext) -> Any:
    # We don't model regclass/oid identity types; evaluating the inner value is
    # enough for the catalog queries that use casts (the results are compared or
    # discarded, never round-tripped through a real type).
    return evaluate(node.this, scope, ctx)


def _func_name(node: exp.Anonymous) -> str:
    name = node.this if isinstance(node.this, str) else node.name
    return str(name).rsplit(".", 1)[-1].lower()


def _eval_func(node: exp.Anonymous, scope: Scope, ctx: ScalarContext) -> Any:
    name = _func_name(node)
    args = [evaluate(a, scope, ctx) for a in node.expressions]
    return _call_func(name, args)


def _eval_typed_func(node: exp.Func, scope: Scope, ctx: ScalarContext) -> Any:
    name = node.sql_name().lower()
    args = [evaluate(a, scope, ctx) for a in node.expressions if isinstance(a, exp.Expression)]
    return _call_func(name, args)


def _call_func(name: str, args: list[Any]) -> Any:
    if name == "format_type":
        return _format_type(args[0] if args else None, args[1] if len(args) > 1 else None)
    if name in ("pg_get_expr", "pg_get_serial_sequence", "pg_get_constraintdef"):
        # No stored defaults / sequences / constraints in our model.
        return None
    if name == "json_build_object":
        out: dict[str, Any] = {}
        for i in range(0, len(args) - 1, 2):
            out[str(args[i])] = args[i + 1]
        return out
    if name == "coalesce":
        for a in args:
            if a is not None:
                return a
        return None
    raise errors.feature_not_supported(f"function {name}() is not supported in this context")


def _format_type(typid: Any, typmod: Any) -> str | None:
    if typid is None:
        return None
    try:
        oid = int(typid)
    except (TypeError, ValueError):
        return str(typid)
    return _OID_TO_TYPENAME.get(oid, "???")


def _lookup_inner_table(ctx: ScalarContext, table_node: exp.Table) -> Any:
    from secantus.sql import planner

    return planner._lookup_table_def(ctx.catalog, ctx.db, table_node, ctx.storage)


def _eval_subquery(node: exp.Expression, outer: Scope, ctx: ScalarContext) -> Any:
    """Evaluate a scalar subquery: first projected value of the first matching
    inner row, else NULL. Correlation falls through to the outer scope."""
    select = node.this if isinstance(node, exp.Subquery) else node
    if not isinstance(select, exp.Select):
        raise errors.feature_not_supported(f"unsupported subquery: {node.sql()}")
    if select.args.get("joins") or select.args.get("group"):
        raise errors.feature_not_supported("only a simple scalar subquery is supported")
    from_node = next((v for v in select.args.values() if isinstance(v, exp.From)), None)
    if from_node is None:
        # FROM-less scalar subquery — evaluate its single projection directly.
        return evaluate(select.expressions[0], outer, ctx)
    table_node = from_node.this
    tdef = _lookup_inner_table(ctx, table_node)
    if tdef is None:
        raise errors.undefined_table(table_node.name)
    inner_alias = table_node.alias or table_node.name
    rows = ctx.storage.find_matching(ctx.db, tdef.collection, {})
    where = select.args.get("where")
    proj = select.expressions[0]
    for row in rows:
        scope = _sub_scope(inner_alias, tdef, row, outer)
        if where is None or _truthy(evaluate(where.this, scope, ctx)):
            return evaluate(proj, scope, ctx)
    return None


def _sub_scope(inner_alias: str, tdef: Any, row: dict[str, Any], outer: Scope) -> Scope:
    def resolve(node: exp.Column) -> Any:
        alias = node.table or None
        name = node.name
        if alias == inner_alias or (alias is None and tdef.column(name) is not None):
            return get_path(row, tdef.field_for(name))
        return outer(node)  # correlated reference to the enclosing query

    return resolve
