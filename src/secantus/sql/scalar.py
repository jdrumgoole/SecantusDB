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

import json
import math
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from sqlglot import exp

from secantus.paths import get_path
from secantus.sql import errors, typemap

# jsonb navigation (->, ->>, #>, #>>); the scalar (->> / #>>) variants render text.
_JSONB_NAV = (exp.JSONExtract, exp.JSONExtractScalar, exp.JSONBExtract, exp.JSONBExtractScalar)
_JSONB_NAV_SCALAR = (exp.JSONExtractScalar, exp.JSONBExtractScalar)

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
    if isinstance(node, exp.Window):
        # Window values are precomputed over the whole partition before per-row
        # evaluation; the evaluated-select scope resolves the node to its value.
        return scope(node)
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
    if isinstance(node, _JSONB_NAV):
        return _eval_jsonb_nav(node, scope, ctx)
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
    if isinstance(node, exp.Exists):
        return _eval_exists(node, scope, ctx)
    if isinstance(node, exp.In):
        return _eval_in(node, scope, ctx)
    if isinstance(node, exp.Between):
        return _eval_between(node, scope, ctx)
    if isinstance(node, (exp.Like, exp.ILike)):
        return _eval_like(node, scope, ctx)
    if type(node) in _ARITH:
        return _eval_arith(node, scope, ctx)
    if isinstance(node, exp.DPipe):  # || string concatenation
        left, right = evaluate(node.this, scope, ctx), evaluate(node.expression, scope, ctx)
        return None if left is None or right is None else _as_text(left) + _as_text(right)
    typed = _SCALAR_FUNC_NODES.get(type(node))
    if typed is not None:
        return typed(node, scope, ctx)
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


def _as_text(value: Any) -> str:
    """Postgres text rendering of a scalar (for ``||`` / ``concat``)."""
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def _pg_div(left: Any, right: Any) -> Any:
    # Postgres integer division truncates toward zero; ``/`` on mixed/float is real.
    if isinstance(left, int) and isinstance(right, int):
        q = abs(left) // abs(right)
        return -q if (left < 0) ^ (right < 0) else q
    return left / right


def _pg_mod(left: Any, right: Any) -> Any:
    # Postgres mod takes the sign of the dividend (unlike Python ``%``).
    r = math.fmod(left, right)
    return int(r) if isinstance(left, int) and isinstance(right, int) else r


_ARITH: dict[type, Callable[[Any, Any], Any]] = {
    exp.Add: lambda a, b: a + b,
    exp.Sub: lambda a, b: a - b,
    exp.Mul: lambda a, b: a * b,
    exp.Div: _pg_div,
    exp.Mod: _pg_mod,
}


def _eval_arith(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    left, right = evaluate(node.this, scope, ctx), evaluate(node.expression, scope, ctx)
    if left is None or right is None:  # NULL propagates through arithmetic
        return None
    return _ARITH[type(node)](left, right)


def _variadic(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> list[Any]:
    args = ([node.this] if node.this is not None else []) + list(node.expressions)
    return [evaluate(a, scope, ctx) for a in args]


def _unary(fn: Callable[[Any], Any]) -> Callable[[exp.Expression, Scope, ScalarContext], Any]:
    def handler(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
        v = evaluate(node.this, scope, ctx)
        return None if v is None else fn(v)

    return handler


def _eval_round(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    v = evaluate(node.this, scope, ctx)
    if v is None:
        return None
    dec = node.args.get("decimals")
    ndigits = int(evaluate(dec, scope, ctx)) if dec is not None else 0
    return round(v, ndigits)


def _eval_substring(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    v = evaluate(node.this, scope, ctx)
    if v is None:
        return None
    text = _as_text(v)
    start_node, length_node = node.args.get("start"), node.args.get("length")
    start = int(evaluate(start_node, scope, ctx)) if start_node is not None else 1
    begin = max(start - 1, 0)  # SQL substring is 1-based
    if length_node is not None:
        return text[begin : begin + int(evaluate(length_node, scope, ctx))]
    return text[begin:]


def _eval_array_size(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    # array_length(arr, dim) / cardinality(arr). Arrays are stored as native BSON
    # lists (one level deep here), so only dimension 1 has a length; any other
    # dimension is NULL, matching Postgres for a 1-D array.
    v = evaluate(node.this, scope, ctx)
    if not isinstance(v, (list, tuple)):
        return None
    dim_node = node.args.get("expression")
    if dim_node is not None and int(evaluate(dim_node, scope, ctx)) != 1:
        return None
    return len(v)


def _eval_nullif(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    a, b = evaluate(node.this, scope, ctx), evaluate(node.expression, scope, ctx)
    return None if a == b else a


def _eval_coalesce(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    for v in _variadic(node, scope, ctx):
        if v is not None:
            return v
    return None


def _eval_concat(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    # Postgres ``concat`` ignores NULL arguments (renders them as empty).
    return "".join(_as_text(v) for v in _variadic(node, scope, ctx) if v is not None)


def _extremum(pick: Callable[[list[Any]], Any]) -> Callable[..., Any]:
    def handler(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
        vals = [v for v in _variadic(node, scope, ctx) if v is not None]
        return pick(vals) if vals else None

    return handler


# Typed scalar function nodes (sqlglot parses these to dedicated classes whose
# operands live in ``this`` / named args, not ``expressions``).
_SCALAR_FUNC_NODES: dict[type, Callable[[exp.Expression, Scope, ScalarContext], Any]] = {
    exp.Upper: _unary(lambda v: _as_text(v).upper()),
    exp.Lower: _unary(lambda v: _as_text(v).lower()),
    exp.Length: _unary(lambda v: len(_as_text(v))),
    exp.Trim: _unary(lambda v: _as_text(v).strip()),
    exp.Abs: _unary(abs),
    exp.Ceil: _unary(lambda v: math.ceil(v)),
    exp.Floor: _unary(lambda v: math.floor(v)),
    exp.Round: _eval_round,
    exp.Pow: lambda n, s, c: (
        None
        if (b := evaluate(n.this, s, c)) is None or (e := evaluate(n.expression, s, c)) is None
        else b**e
    ),
    exp.Substring: _eval_substring,
    exp.Nullif: _eval_nullif,
    exp.Coalesce: _eval_coalesce,
    exp.Concat: _eval_concat,
    exp.Greatest: _extremum(max),
    exp.Least: _extremum(min),
    exp.ArraySize: _eval_array_size,
}


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
    return _call_func(name, args, ctx)


def _eval_typed_func(node: exp.Func, scope: Scope, ctx: ScalarContext) -> Any:
    name = node.sql_name().lower()
    args = [evaluate(a, scope, ctx) for a in node.expressions if isinstance(a, exp.Expression)]
    return _call_func(name, args, ctx)


def _seq_name(arg: Any) -> str:
    """The bare sequence name from a ``nextval`` / ``currval`` / ``setval`` arg —
    a string (possibly schema-qualified ``public.s``, or quoted), stripped down."""
    text = str(arg).strip().strip('"')
    return text.split(".")[-1].strip('"')


def _sequence_func(name: str, args: list[Any], ctx: ScalarContext | None) -> Any:
    """``nextval`` / ``currval`` / ``setval`` / ``lastval`` — sequence value ops.
    ``nextval`` / ``setval`` advance persisted state via the catalog; ``currval`` /
    ``lastval`` read per-session state; all record the session's currval."""
    if ctx is None:
        raise errors.feature_not_supported(f"{name}() requires an execution context")
    if name == "lastval":
        return ctx.session.lastval()
    if not args:
        raise errors.SQLError("42883", f"{name}() requires an argument")
    seq = _seq_name(args[0])
    if name == "nextval":
        value = ctx.catalog.sequence_nextval(ctx.db, seq)
        ctx.session.record_sequence_value(seq, value)
        return value
    if name == "currval":
        return ctx.session.currval(seq)
    # setval(seq, value [, is_called])
    value = int(args[1])
    is_called = bool(args[2]) if len(args) > 2 else True
    ctx.catalog.sequence_setval(ctx.db, seq, value, is_called)
    if is_called:
        ctx.session.record_sequence_value(seq, value)
    return value


def _call_func(name: str, args: list[Any], ctx: ScalarContext | None = None) -> Any:
    if name == "format_type":
        return _format_type(args[0] if args else None, args[1] if len(args) > 1 else None)
    if name == "pg_get_constraintdef":
        # Render a foreign-key constraint (by oid) the way Postgres does so
        # SQLAlchemy's inspector can reflect it; unknown oid / no ctx → NULL
        # (we store no CHECK constraints or defaults).
        if ctx is not None and args and isinstance(args[0], int):
            from secantus.sql import virtual

            return virtual.constraint_def_for_oid(ctx.db, ctx.catalog, args[0])
        return None
    if name == "pg_get_viewdef":
        # Render a view's stored SELECT text (by pg_class oid) so SQLAlchemy's
        # get_view_definition can reflect it; unknown oid / no ctx → NULL.
        if ctx is not None and args and isinstance(args[0], int):
            from secantus.sql import virtual

            return virtual.viewdef_for_oid(ctx.db, ctx.catalog, args[0])
        return None
    if name in ("nextval", "currval", "setval", "lastval"):
        return _sequence_func(name, args, ctx)
    if name in (
        "pg_get_expr",
        "pg_get_serial_sequence",
        "pg_get_indexdef",
    ):
        # No stored defaults; index defs not rendered.
        return None
    if name in ("json_build_object", "jsonb_build_object"):
        out: dict[str, Any] = {}
        for i in range(0, len(args) - 1, 2):
            out[str(args[i])] = args[i + 1]
        return out
    if name in ("json_build_array", "jsonb_build_array"):
        return list(args)
    if name in ("jsonb_array_length", "json_array_length"):
        v = args[0] if args else None
        if not isinstance(v, list):
            raise errors.SQLError("22023", "cannot get array length of a non-array")
        return len(v)
    if name in ("array_length", "cardinality"):
        v = args[0] if args else None
        if not isinstance(v, (list, tuple)):
            return None
        if name == "array_length" and len(args) > 1 and args[1] != 1:
            return None
        return len(v)
    if name in ("jsonb_typeof", "json_typeof"):
        return _json_typeof(args[0] if args else None)
    if name == "coalesce":
        for a in args:
            if a is not None:
                return a
        return None
    raise errors.feature_not_supported(f"function {name}() is not supported in this context")


def _eval_jsonb_nav(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """Resolve a ->/->>/#>/#>> navigation against the current row value."""
    from secantus.sql.planner import _json_keys

    val = evaluate(node.this, scope, ctx)
    for key in _json_keys(node.expression):
        if isinstance(val, dict):
            val = val.get(key)
        elif isinstance(val, list):
            try:
                val = val[int(key)]
            except (ValueError, IndexError):
                return None
        else:
            return None
    if isinstance(node, _JSONB_NAV_SCALAR):  # ->> / #>> return text
        return _json_as_text(val)
    return val


def _json_as_text(val: Any) -> str | None:
    if val is None:
        return None
    if isinstance(val, str):
        return val
    if isinstance(val, bool):
        return "true" if val else "false"
    if isinstance(val, (dict, list)):
        return json.dumps(val)
    return str(val)


def _json_typeof(value: Any) -> str | None:
    """Postgres ``jsonb_typeof``: the JSON type name of a value, NULL for SQL NULL."""
    if value is None:
        return None
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, (int, float)):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        return "object"
    return None


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


def _subquery_select(node: exp.Expression) -> exp.Expression:
    return node.this if isinstance(node, exp.Subquery) else node


def _inner_row_scopes(select: exp.Expression, outer: Scope, ctx: ScalarContext):
    """Yield a ``Scope`` for each inner-table row that satisfies the subquery's
    WHERE. Correlated references in that WHERE fall through to ``outer``. Shared
    by scalar-subquery / EXISTS / IN evaluation (a generator so EXISTS/IN can
    stop at the first match)."""
    if not isinstance(select, exp.Select):
        raise errors.feature_not_supported(f"unsupported subquery: {select.sql()}")
    if select.args.get("joins") or select.args.get("group"):
        raise errors.feature_not_supported("only a simple subquery is supported")
    where = select.args.get("where")
    from_node = next((v for v in select.args.values() if isinstance(v, exp.From)), None)
    if from_node is None:
        # FROM-less subquery — a single synthetic row over the outer scope.
        if where is None or _truthy(evaluate(where.this, outer, ctx)):
            yield outer
        return
    table_node = from_node.this
    tdef = _lookup_inner_table(ctx, table_node)
    if tdef is None:
        raise errors.undefined_table(table_node.name)
    inner_alias = table_node.alias or table_node.name
    for row in ctx.storage.find_matching(ctx.db, tdef.collection, {}):
        scope = _sub_scope(inner_alias, tdef, row, outer)
        if where is None or _truthy(evaluate(where.this, scope, ctx)):
            yield scope


# Aggregate projections in a scalar subquery reduce the matched inner rows to a
# single value (the common ``= (SELECT max(x) FROM … WHERE …)`` shape).
_SUBQUERY_AGG_REDUCERS: dict[type, Callable[[list[Any]], Any]] = {
    exp.Max: max,
    exp.Min: min,
    exp.Sum: sum,
    exp.Avg: lambda vals: sum(vals) / len(vals),
}


def _eval_subquery(node: exp.Expression, outer: Scope, ctx: ScalarContext) -> Any:
    """Evaluate a scalar subquery to a single value. An aggregate projection
    (``max``/``min``/``sum``/``avg``/``count``) reduces every matching inner row;
    otherwise it's the projection of the first matching row, else NULL.
    Correlation falls through to the outer scope."""
    select = _subquery_select(node)
    proj = select.expressions[0]
    target = proj.this if isinstance(proj, exp.Alias) else proj
    if isinstance(target, exp.Count):
        scopes = list(_inner_row_scopes(select, outer, ctx))
        if isinstance(target.this, exp.Star):
            return len(scopes)
        return sum(1 for s in scopes if evaluate(target.this, s, ctx) is not None)
    reducer = _SUBQUERY_AGG_REDUCERS.get(type(target))
    if reducer is not None:
        vals = [
            v
            for v in (evaluate(target.this, s, ctx) for s in _inner_row_scopes(select, outer, ctx))
            if v is not None
        ]
        return reducer(vals) if vals else None
    for scope in _inner_row_scopes(select, outer, ctx):
        return evaluate(proj, scope, ctx)
    return None


def _eval_exists(node: exp.Exists, outer: Scope, ctx: ScalarContext) -> bool:
    """``EXISTS (subquery)`` — True if any inner row satisfies the (possibly
    correlated) subquery WHERE. ``NOT EXISTS`` is handled by the ``exp.Not``
    branch wrapping this."""
    for _ in _inner_row_scopes(_subquery_select(node.this), outer, ctx):
        return True
    return False


def _eval_in(node: exp.In, outer: Scope, ctx: ScalarContext) -> Any:
    """``x IN (...)`` — a value list or a (possibly correlated) subquery."""
    left = evaluate(node.this, outer, ctx)
    query = node.args.get("query")
    if query is not None:
        select = _subquery_select(query)
        proj = select.expressions[0]
        candidates = [evaluate(proj, scope, ctx) for scope in _inner_row_scopes(select, outer, ctx)]
    else:
        candidates = [evaluate(e, outer, ctx) for e in node.expressions]
    if left is None:
        return None  # NULL IN (...) is unknown
    return any(left == v for v in candidates)


def _eval_between(node: exp.Between, outer: Scope, ctx: ScalarContext) -> Any:
    v = evaluate(node.this, outer, ctx)
    low = evaluate(node.args["low"], outer, ctx)
    high = evaluate(node.args["high"], outer, ctx)
    if v is None or low is None or high is None:
        return None
    return low <= v <= high


def _eval_like(node: exp.Expression, outer: Scope, ctx: ScalarContext) -> Any:
    import re

    from secantus.sql.planner import _like_to_regex

    val = evaluate(node.this, outer, ctx)
    pattern = evaluate(node.expression, outer, ctx)
    if val is None or pattern is None:
        return None
    flags = re.IGNORECASE if isinstance(node, exp.ILike) else 0
    return re.match(_like_to_regex(_as_text(pattern)), _as_text(val), flags) is not None


def _sub_scope(inner_alias: str, tdef: Any, row: dict[str, Any], outer: Scope) -> Scope:
    def resolve(node: exp.Column) -> Any:
        alias = node.table or None
        name = node.name
        if alias == inner_alias or (alias is None and tdef.column(name) is not None):
            return get_path(row, tdef.field_for(name))
        return outer(node)  # correlated reference to the enclosing query

    return resolve
