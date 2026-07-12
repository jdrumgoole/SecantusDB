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

import contextlib
import datetime as _dt
import json
import math
from collections.abc import Callable
from dataclasses import dataclass
from decimal import Decimal
from typing import Any

import bson
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
    if isinstance(node, exp.BitString):  # ``B'1010'`` -> the '0'/'1' string
        return str(node.this)
    if isinstance(node, exp.Neg):
        v = evaluate(node.this, scope, ctx)
        if v is None:
            return None
        if isinstance(v, dict) and "interval" in v:
            from secantus.sql import intervals as _intervals

            return _intervals.neg(v)
        return -v
    if isinstance(node, exp.Cast):
        return _eval_cast(node, scope, ctx)
    if isinstance(node, exp.Column):
        return scope(node)
    if isinstance(node, _JSONB_NAV):
        return _eval_jsonb_nav(node, scope, ctx)
    if isinstance(node, exp.JSONBDeleteAtPath):
        return _eval_jsonb_delete_path(node, scope, ctx)
    if isinstance(node, (exp.ArrayContainsAll, exp.ArrayContainedBy, exp.ArrayOverlaps)):
        result = _eval_range_op(node, scope, ctx)
        if result is not _NOT_RANGE:
            return result
        net_result = _eval_net_op(node, scope, ctx)
        if net_result is not _NOT_NET:
            return net_result
        geo_result = _eval_geo_op(node, scope, ctx)
        if geo_result is not _NOT_GEO:
            return geo_result
        hstore_result = _eval_hstore_op(node, scope, ctx)
        if hstore_result is not _NOT_HSTORE:
            return hstore_result
        array_result = _eval_array_op(node, scope, ctx)
        if array_result is not _NOT_ARRAY:
            return array_result
        jsonb_result = _eval_jsonb_op(node, scope, ctx)
        if jsonb_result is not _NOT_JSONB:
            return jsonb_result
    if isinstance(
        node, (exp.JSONBContains, exp.JSONBContainsAllTopKeys, exp.JSONBContainsAnyTopKeys)
    ):
        hstore_result = _eval_hstore_exists(node, scope, ctx)
        if hstore_result is not _NOT_HSTORE:
            return hstore_result
    if getattr(exp, "Distance", None) is not None and isinstance(node, exp.Distance):
        from secantus.sql import pggeo as _pggeo

        left = evaluate(node.this, scope, ctx)
        right = evaluate(node.expression, scope, ctx)
        if left is None or right is None:
            return None
        return _pggeo.distance(left, right)
    if isinstance(node, (exp.BitwiseLeftShift, exp.BitwiseRightShift)):
        net_result = _eval_net_op(node, scope, ctx)
        if net_result is not _NOT_NET:
            return net_result
        bit_result = _eval_bitwise(node, scope, ctx)
        if bit_result is not _NOT_BIT:
            return bit_result
    if isinstance(node, (exp.BitwiseAnd, exp.BitwiseOr, exp.BitwiseXor, exp.BitwiseNot)):
        bit_result = _eval_bitwise(node, scope, ctx)
        if bit_result is not _NOT_BIT:
            return bit_result
    if getattr(exp, "Getbit", None) is not None and isinstance(node, exp.Getbit):
        from secantus.sql import bitstr as _bitstr

        bits = evaluate(node.this, scope, ctx)
        idx = evaluate(node.expression, scope, ctx)
        return None if bits is None or idx is None else _bitstr.get_bit(str(bits), int(idx))
    if getattr(exp, "Host", None) is not None and isinstance(node, exp.Host):
        from secantus.sql import net as _net

        v = evaluate(node.this, scope, ctx)
        return None if v is None else _net.host(v)
    if getattr(exp, "Uuid", None) is not None and isinstance(node, exp.Uuid):
        from secantus.sql import uuidtype as _uuidtype

        return _uuidtype.generate()
    if getattr(exp, "Adjacent", None) is not None and isinstance(node, exp.Adjacent):
        from secantus.sql import ranges as _ranges

        left = evaluate(node.this, scope, ctx)
        right = evaluate(node.expression, scope, ctx)
        if left is None or right is None:
            return None
        return _ranges.adjacent(left, right)
    if isinstance(node, exp.JSONBPathExists):  # jsonb @? jsonpath
        return _eval_jsonb_path_op(node.this, node.expression, "exists", scope, ctx)
    if getattr(exp, "MatchAgainst", None) is not None and isinstance(node, exp.MatchAgainst):
        # ``@@`` is overloaded: full-text ``tsvector @@ tsquery`` and jsonb
        # ``data @@ jsonpath``. sqlglot parses ``left @@ right`` with the right
        # operand in ``this`` and the left in ``expressions[0]``.
        left_node = node.expressions[0] if node.expressions else None
        right_val = evaluate(node.this, scope, ctx)
        left_val = evaluate(left_node, scope, ctx) if left_node is not None else None
        fts_result = _eval_fts_match(left_val, right_val)
        if fts_result is not _NOT_FTS:
            return fts_result
        # Otherwise it's a jsonb @@ jsonpath predicate.
        return _eval_jsonb_path_op(left_node, node.this, "match", scope, ctx)
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
    if isinstance(node, (exp.RegexpLike, exp.RegexpILike)):
        return _eval_regexp(node, scope, ctx)
    if type(node) in _ARITH:
        return _eval_arith(node, scope, ctx)
    if isinstance(node, exp.DPipe):  # || string concatenation
        left, right = evaluate(node.this, scope, ctx), evaluate(node.expression, scope, ctx)
        if left is None or right is None:
            return None
        # ``bytea || bytea`` concatenates the raw bytes; anything else is text.
        if isinstance(left, (bytes, bytearray)) and isinstance(right, (bytes, bytearray)):
            return bytes(left) + bytes(right)
        # ``hstore || hstore`` merges (right wins).
        from secantus.sql import hstore as _hstore

        if _hstore.is_hstore(left) or _hstore.is_hstore(right):
            return _hstore.merge(_hstore.parse(left), _hstore.parse(right))
        return _as_text(left) + _as_text(right)
    if isinstance(node, exp.Bracket):
        return _eval_bracket(node, scope, ctx)
    if isinstance(node, exp.Array):  # ARRAY[...] constructor -> a Python list
        return [evaluate(e, scope, ctx) for e in node.expressions]
    if isinstance(node, exp.Interval):  # interval '1 day' (added to / subtracted
        return _eval_interval(node, scope, ctx)  # from a date via _Interval.__radd__)
    typed = _SCALAR_FUNC_NODES.get(type(node))
    if typed is not None:
        return typed(node, scope, ctx)
    # Schema-qualified function: pg_catalog.format_type(...) -> the call.
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        return _eval_func(node.expression, scope, ctx)
    # Composite field access: ``(col).field`` -> Dot(Paren(col), Identifier). The
    # inner expression resolves to a subdocument; return the named field (NULL for
    # a missing field or a NULL composite).
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Identifier):
        base = evaluate(node.this, scope, ctx)
        if base is None:
            return None
        if isinstance(base, dict):
            return base.get(node.expression.name)
        raise errors.feature_not_supported(f"field access on a non-composite value: {node.sql()}")
    if isinstance(node, exp.Anonymous):
        return _eval_func(node, scope, ctx)
    if isinstance(node, exp.Func):
        return _eval_typed_func(node, scope, ctx)
    if isinstance(node, (exp.Select, exp.Subquery)):
        return _eval_subquery(node, scope, ctx)
    raise errors.feature_not_supported(f"unsupported scalar expression: {node.sql()}")


def _eval_bracket(node: exp.Bracket, scope: Scope, ctx: ScalarContext) -> Any:
    """Postgres array subscript / slice: ``arr[i]`` (1-based element, NULL when out
    of range) and ``arr[lo:hi]`` (1-based inclusive slice, clamped).

    sqlglot's Postgres dialect folds a compile-time-constant single index to
    0-based (``arr[1]`` → literal ``0``) but leaves a runtime (column-bearing)
    index as the raw 1-based value; slice bounds always stay 1-based. We mirror
    that: a column-bearing single index is decremented, a constant one is used
    as-is, and a negative resulting index is out of range (no Python wraparound)."""
    base = evaluate(node.this, scope, ctx)
    if base is None or not node.expressions:
        return None
    idx_node = node.expressions[0]
    if isinstance(idx_node, exp.Slice):
        if not isinstance(base, (list, tuple)):
            return None
        lo = idx_node.this
        hi = idx_node.args.get("expression")
        lo_i = max(int(evaluate(lo, scope, ctx)), 1) if lo is not None else 1
        hi_i = int(evaluate(hi, scope, ctx)) if hi is not None else len(base)
        return list(base[lo_i - 1 : hi_i])
    if not isinstance(base, (list, tuple)):
        return None
    val = evaluate(idx_node, scope, ctx)
    if val is None:
        return None
    i = int(val)
    if any(True for _ in idx_node.find_all(exp.Column)):
        i -= 1  # runtime 1-based index -> 0-based
    if i < 0 or i >= len(base):
        return None  # out of range -> NULL (no negative wraparound)
    return base[i]


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


def _unwrap_decimal(v: Any) -> Any:
    """A stored ``numeric`` / ``money`` value is a BSON ``Decimal128``, which has no
    Python arithmetic / comparison operators — unwrap it to a ``Decimal`` so the
    scalar evaluator can compute with it."""
    return v.to_decimal() if isinstance(v, bson.Decimal128) else v


def _eval_arith(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    left, right = evaluate(node.this, scope, ctx), evaluate(node.expression, scope, ctx)
    if left is None or right is None:  # NULL propagates through arithmetic
        return None
    left, right = _unwrap_decimal(left), _unwrap_decimal(right)
    # ``*`` / ``+`` / ``-`` are overloaded for ranges (intersection / union /
    # difference) when both operands are range subdocuments.
    if _is_range_value(left) and _is_range_value(right):
        from secantus.sql import ranges as _ranges

        if isinstance(node, exp.Mul):
            return _ranges.intersect(left, right)
        if isinstance(node, exp.Add):
            return _ranges.union(left, right)
        if isinstance(node, exp.Sub):
            return _ranges.difference(left, right)
    date_result = _eval_date_arith(node, left, right)
    if date_result is not _NOT_DATE:
        return date_result
    interval_result = _eval_interval_arith(node, left, right)
    if interval_result is not _NOT_INTERVAL:
        return interval_result
    return _ARITH[type(node)](left, right)


_NOT_DATE = object()


def _eval_date_arith(node: exp.Expression, left: Any, right: Any) -> Any:
    """Arithmetic on ``date`` / ``time`` values: ``date - date -> int`` (days),
    ``date ± int -> date``, ``date ± interval -> timestamp``, and
    ``time - time -> interval``. Returns ``_NOT_DATE`` when neither operand is a
    date / time so the caller falls through to interval / numeric arithmetic."""
    from secantus.sql import datetimes as _datetimes
    from secantus.sql import intervals as _intervals

    ld, rd = _datetimes.is_date_value(left), _datetimes.is_date_value(right)
    lt_, rt_ = _datetimes.is_time_value(left), _datetimes.is_time_value(right)
    li, ri = _intervals.is_interval(left), _intervals.is_interval(right)

    def _is_int(v: Any) -> bool:
        return isinstance(v, int) and not isinstance(v, bool)

    if isinstance(node, exp.Add):
        if ld and _is_int(right):
            return _datetimes.date_add_days(left, right)
        if rd and _is_int(left):
            return _datetimes.date_add_days(right, left)
        if ld and ri:
            base = _dt.datetime.combine(_datetimes.to_date_obj(left), _dt.time())
            return _intervals.to_date(base, right, 1)
        if rd and li:
            base = _dt.datetime.combine(_datetimes.to_date_obj(right), _dt.time())
            return _intervals.to_date(base, left, 1)
    elif isinstance(node, exp.Sub):
        if ld and rd:
            return _datetimes.date_sub_date(left, right)
        if ld and _is_int(right):
            return _datetimes.date_add_days(left, -right)
        if ld and ri:
            base = _dt.datetime.combine(_datetimes.to_date_obj(left), _dt.time())
            return _intervals.to_date(base, right, -1)
        if lt_ and rt_:
            return _time_sub_time(left, right)
    return _NOT_DATE


def _time_sub_time(a: Any, b: Any) -> dict:
    from secantus.sql import datetimes as _datetimes
    from secantus.sql import intervals as _intervals

    ta, tb = _datetimes.to_time_obj(a), _datetimes.to_time_obj(b)

    def _micros(t: _dt.time) -> int:
        return ((t.hour * 3600 + t.minute * 60 + t.second) * 1_000_000) + t.microsecond

    return _intervals.make(0, 0, _micros(ta) - _micros(tb))


_NOT_INTERVAL = object()


def _eval_interval_arith(node: exp.Expression, left: Any, right: Any) -> Any:
    """Arithmetic overloaded for intervals: ``date ± interval``, ``interval ±
    interval``, ``interval * number`` (either order), ``interval / number``, and
    ``timestamp - timestamp -> interval``. Returns ``_NOT_INTERVAL`` when neither
    operand is an interval / date pair so the caller falls back to numeric arith."""
    from secantus.sql import intervals as _intervals

    li, ri = _intervals.is_interval(left), _intervals.is_interval(right)
    if isinstance(node, exp.Add):
        if li and ri:
            return _intervals.add(left, right)
        if ri and isinstance(left, (_dt.date, _dt.datetime)):
            return _intervals.to_date(left, right, 1)
        if li and isinstance(right, (_dt.date, _dt.datetime)):
            return _intervals.to_date(right, left, 1)
    elif isinstance(node, exp.Sub):
        if li and ri:
            return _intervals.sub(left, right)
        if li and isinstance(right, (_dt.date, _dt.datetime)):
            return _NOT_INTERVAL  # interval - timestamp is undefined
        if ri and isinstance(left, (_dt.date, _dt.datetime)):
            return _intervals.to_date(left, right, -1)
        if isinstance(left, (_dt.date, _dt.datetime)) and isinstance(
            right, (_dt.date, _dt.datetime)
        ):
            return _intervals.diff(left, right)
    elif isinstance(node, exp.Mul):
        if li and isinstance(right, (int, float)):
            return _intervals.mul(left, right)
        if ri and isinstance(left, (int, float)):
            return _intervals.mul(right, left)
    elif isinstance(node, exp.Div) and li and isinstance(right, (int, float)) and right != 0:
        return _intervals.mul(left, 1.0 / right)
    return _NOT_INTERVAL


def _is_range_value(v: Any) -> bool:
    return isinstance(v, dict) and ("lower" in v or "empty" in v)


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
    # ``array_length(arr, dim)`` — the length along ``dim`` (1-based). A
    # multi-dimensional array reports each dimension's length; a dimension beyond
    # the array's rank is NULL, matching Postgres.
    v = evaluate(node.this, scope, ctx)
    if not isinstance(v, (list, tuple)):
        return None
    dims = _array_dim_lengths(v)
    dim_node = node.args.get("expression")
    dim = int(evaluate(dim_node, scope, ctx)) if dim_node is not None else 1
    if dim < 1 or dim > len(dims):
        return None
    return dims[dim - 1]


def _as_list(v: Any) -> list:
    """Coerce an array operand to a list; a NULL array is the empty array (matches
    Postgres, where ``array_append(NULL, x)`` -> ``{x}``)."""
    if v is None:
        return []
    return list(v) if isinstance(v, (list, tuple)) else [v]


def _eval_array_append(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    arr = evaluate(node.this, scope, ctx)
    elem = evaluate(node.args.get("expression"), scope, ctx)
    return _as_list(arr) + [elem]


def _eval_array_prepend(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    # sqlglot normalizes both array_append(arr, e) and array_prepend(e, arr) to
    # this=arr / expression=e; the node type is what distinguishes them.
    arr = evaluate(node.this, scope, ctx)
    elem = evaluate(node.args.get("expression"), scope, ctx)
    return [elem] + _as_list(arr)


def _eval_array_cat(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    out = _as_list(evaluate(node.this, scope, ctx))
    for e in node.args.get("expressions") or []:
        out = out + _as_list(evaluate(e, scope, ctx))
    return out


def _eval_array_position(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    arr = evaluate(node.this, scope, ctx)
    if not isinstance(arr, (list, tuple)):
        return None
    elem = evaluate(node.args.get("expression"), scope, ctx)
    for i, v in enumerate(arr, start=1):  # 1-based, first match
        if v == elem:
            return i
    return None


def _eval_array_remove(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    arr = evaluate(node.this, scope, ctx)
    if arr is None:
        return None
    elem = evaluate(node.args.get("expression"), scope, ctx)
    return [v for v in _as_list(arr) if v != elem]


def _eval_array_to_string(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    arr = evaluate(node.this, scope, ctx)
    if arr is None:
        return None
    delim = _as_text(evaluate(node.args.get("expression"), scope, ctx))
    null_node = node.args.get("null")
    null_str = None if null_node is None else _as_text(evaluate(null_node, scope, ctx))
    parts = []
    for v in _as_list(arr):
        if v is None:
            if null_str is not None:
                parts.append(null_str)  # NULL elements omitted unless a null_string is given
        else:
            parts.append(_as_text(v))
    return delim.join(parts)


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


def _re_compile(pattern: str, flags_str: str) -> Any:
    """Compile a POSIX regex with Postgres flag letters (``i`` case-insensitive,
    ``m``/``n`` newline-sensitive, ``s`` dot-all, ``x`` extended)."""
    import re

    fs = flags_str or ""
    f = 0
    if "i" in fs:
        f |= re.IGNORECASE
    if "m" in fs or "n" in fs:
        f |= re.MULTILINE
    if "s" in fs:
        f |= re.DOTALL
    if "x" in fs:
        f |= re.VERBOSE
    return re.compile(pattern, f)


def _pg_replacement(repl: str) -> str:
    """Translate a Postgres ``regexp_replace`` replacement into Python's ``re.sub``
    syntax: ``\\&`` (whole match) becomes ``\\g<0>``; ``\\1``–``\\9`` pass through."""
    return repl.replace(r"\&", r"\g<0>")


def _eval_regexp_replace(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    src = evaluate(node.this, scope, ctx)
    pattern = evaluate(node.expression, scope, ctx)
    repl = evaluate(node.args.get("replacement"), scope, ctx)
    if src is None or pattern is None or repl is None:
        return None
    mods = node.args.get("modifiers")
    flags_str = _as_text(evaluate(mods, scope, ctx)) if mods is not None else ""
    rx = _re_compile(_as_text(pattern), flags_str)
    return rx.sub(
        _pg_replacement(_as_text(repl)), _as_text(src), count=0 if "g" in flags_str else 1
    )


def _eval_split_part(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    src = evaluate(node.this, scope, ctx)
    delim = evaluate(node.args.get("delimiter"), scope, ctx)
    idx = evaluate(node.args.get("part_index"), scope, ctx)
    if src is None or delim is None or idx is None:
        return None
    parts = _as_text(src).split(_as_text(delim))
    n = int(idx)
    if n < 0:  # Postgres 14+: count from the end
        n = len(parts) + n + 1
    return parts[n - 1] if 1 <= n <= len(parts) else ""


def _eval_translate(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    src = evaluate(node.this, scope, ctx)
    frm = evaluate(node.args.get("from_"), scope, ctx)
    to = evaluate(node.args.get("to"), scope, ctx)
    if src is None or frm is None or to is None:
        return None
    frm, to = _as_text(frm), _as_text(to)
    # Each char in `frm` maps to the same-position char in `to`; chars in `frm`
    # beyond `to`'s length are deleted.
    table = {ord(ch): (to[i] if i < len(to) else None) for i, ch in enumerate(frm)}
    return _as_text(src).translate(table)


def _eval_regexp_count(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    src = evaluate(node.this, scope, ctx)
    pattern = evaluate(node.expression, scope, ctx)
    if src is None or pattern is None:
        return None
    return len(_re_compile(_as_text(pattern), "").findall(_as_text(src)))


def _eval_pad(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``lpad(str, len [, fill])`` / ``rpad(...)`` — pad (or truncate) ``str`` to
    ``len`` characters using ``fill`` (default a space); ``is_left`` selects lpad."""
    src = evaluate(node.this, scope, ctx)
    length = evaluate(node.expression, scope, ctx)
    if src is None or length is None:
        return None
    text, n = _as_text(src), int(length)
    if n <= 0:
        return ""
    fill_node = node.args.get("fill_pattern")
    fill = _as_text(evaluate(fill_node, scope, ctx)) if fill_node is not None else " "
    if len(text) >= n:
        return text[:n]
    if not fill:
        return text
    pad = (fill * (n // len(fill) + 1))[: n - len(text)]
    return (pad + text) if node.args.get("is_left") else (text + pad)


def _eval_left(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    src = evaluate(node.this, scope, ctx)
    n = evaluate(node.expression, scope, ctx)
    if src is None or n is None:
        return None
    return _as_text(src)[: int(n)]  # negative n drops the last |n| chars (Postgres)


def _eval_right(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    src = evaluate(node.this, scope, ctx)
    n = evaluate(node.expression, scope, ctx)
    if src is None or n is None:
        return None
    text, i = _as_text(src), int(n)
    return "" if i == 0 else text[-i:]  # negative i drops the first |i| chars


def _eval_repeat(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    src = evaluate(node.this, scope, ctx)
    times = evaluate(node.args.get("times"), scope, ctx)
    if src is None or times is None:
        return None
    return _as_text(src) * max(int(times), 0)


def _eval_ascii(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    src = evaluate(node.this, scope, ctx)
    if src is None:
        return None
    text = _as_text(src)
    return ord(text[0]) if text else 0


def _eval_chr(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    args = node.expressions or ([node.this] if node.this is not None else [])
    code = evaluate(args[0], scope, ctx) if args else None
    return None if code is None else chr(int(code))


def _eval_str_position(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``position(sub IN str)`` / ``strpos(str, sub)`` -> 1-based index, 0 if absent."""
    src = evaluate(node.this, scope, ctx)
    sub = evaluate(node.args.get("substr"), scope, ctx)
    if src is None or sub is None:
        return None
    return _as_text(src).find(_as_text(sub)) + 1


def _eval_overlay(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``overlay(str placing rep from start [for len])`` — replace ``len`` chars
    (default ``len(rep)``) starting at 1-based ``start`` with ``rep``."""
    src = evaluate(node.this, scope, ctx)
    rep = evaluate(node.expression, scope, ctx)
    start = evaluate(node.args.get("from_"), scope, ctx)
    if src is None or rep is None or start is None:
        return None
    text, rep_text, i = _as_text(src), _as_text(rep), int(start)
    for_node = node.args.get("for_")
    span = int(evaluate(for_node, scope, ctx)) if for_node is not None else len(rep_text)
    return text[: i - 1] + rep_text + text[i - 1 + span :]


def _eval_trunc(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``trunc(x [, n])`` — truncate toward zero to ``n`` decimal places (0 default)."""
    v = evaluate(node.this, scope, ctx)
    if v is None:
        return None
    dec = node.args.get("decimals")
    n = int(evaluate(dec, scope, ctx)) if dec is not None else 0
    if n == 0:
        return math.trunc(v)
    factor = 10.0**n
    return math.trunc(v * factor) / factor


def _eval_log(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``log(x)`` is base-10 in Postgres; ``log(b, x)`` is log base ``b`` (this=b)."""
    a = evaluate(node.this, scope, ctx)
    if a is None:
        return None
    expr = node.args.get("expression")
    if expr is not None:
        x = evaluate(expr, scope, ctx)
        if x is None:
            return None
        # Use the exact base-10 / base-2 routines when applicable so that, e.g.,
        # log(10, 1000) is 3.0 rather than 2.9999999999999996.
        if a == 10:
            return math.log10(x)
        if a == 2:
            return math.log2(x)
        return math.log(x, a)
    return math.log10(a)


def _eval_pi(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    return math.pi


def _sign(v: Any) -> Any:
    """-1 / 0 / 1 with the operand's numeric kind preserved (float stays float)."""
    s = (v > 0) - (v < 0)
    return float(s) if isinstance(v, float) else s


def _cbrt(v: Any) -> float:
    """Real cube root — Python's ``** (1/3)`` goes complex for negatives."""
    return math.copysign(abs(v) ** (1.0 / 3.0), v)


# -- date / time ------------------------------------------------------------- #


def _eval_interval(node: exp.Interval, scope: Scope, ctx: ScalarContext) -> dict:
    """``interval '<n> <unit>'`` / ``interval '<n>' <unit>`` / a compound literal
    (``'1 year 2 months'``) -> an interval subdocument (see ``secantus.sql.intervals``)."""
    from secantus.sql import intervals as _intervals

    raw = evaluate(node.this, scope, ctx) if node.this is not None else None
    text = _as_text(raw).strip() if raw is not None else ""
    unit = node.args.get("unit")
    unit_name = unit.name if unit is not None else None
    if unit_name:
        return _intervals.from_unit(float(text), unit_name)
    return _intervals.parse(text)


def _as_datetime(v: Any) -> _dt.datetime | _dt.date:
    if isinstance(v, (_dt.datetime, _dt.date)):
        return v
    from secantus.sql.datetimes import parse_iso_datetime

    text = _as_text(v)
    return parse_iso_datetime(text)


def _eval_extract(node: exp.Extract, scope: Scope, ctx: ScalarContext) -> Any:
    """``extract(field FROM ts)`` / ``date_part('field', ts)`` -> a numeric field."""
    src = evaluate(node.expression, scope, ctx)
    if src is None:
        return None
    field = node.this.name.lower()
    if isinstance(src, dict) and "interval" in src:
        from secantus.sql import intervals as _intervals

        return _intervals.extract_field(field, src)
    ts = _as_datetime(src)
    if field in ("year", "years"):
        return ts.year
    if field in ("month", "months"):
        return ts.month
    if field in ("day", "days"):
        return ts.day
    if field == "quarter":
        return (ts.month - 1) // 3 + 1
    if field in ("dow",):  # Postgres: Sunday = 0 .. Saturday = 6
        return (ts.weekday() + 1) % 7
    if field in ("isodow",):  # Monday = 1 .. Sunday = 7
        return ts.isoweekday()
    if field in ("doy",):
        return ts.timetuple().tm_yday
    if field in ("week",):
        return ts.isocalendar()[1]
    if field in ("hour", "hours"):
        return getattr(ts, "hour", 0)
    if field in ("minute", "minutes"):
        return getattr(ts, "minute", 0)
    if field in ("second", "seconds"):
        return getattr(ts, "second", 0)
    if field == "epoch":
        base = ts if isinstance(ts, _dt.datetime) else _dt.datetime(ts.year, ts.month, ts.day)
        if base.tzinfo is None:
            base = base.replace(tzinfo=_dt.timezone.utc)
        return base.timestamp()
    raise errors.feature_not_supported(f"unsupported extract field: {field}")


_DATE_TRUNC_UNITS = ("year", "quarter", "month", "week", "day", "hour", "minute", "second")


def _eval_date_trunc(node: exp.TimestampTrunc, scope: Scope, ctx: ScalarContext) -> Any:
    """``date_trunc('unit', ts)`` -> ts zeroed below ``unit`` (week -> Monday).

    ``date_trunc('unit', interval)`` truncates an interval value instead, zeroing
    every component finer than ``unit`` (matching Postgres)."""
    from secantus.sql import intervals as _intervals

    src = evaluate(node.this, scope, ctx)
    if src is None:
        return None
    unit0 = node.args["unit"].name.lower().rstrip("s")
    if _intervals.is_interval(src):
        return _date_trunc_interval(unit0, src, _intervals)
    ts = _as_datetime(src)
    if not isinstance(ts, _dt.datetime):
        ts = _dt.datetime(ts.year, ts.month, ts.day, tzinfo=_dt.timezone.utc)
    unit = unit0
    y, mo, d, h, mi, s = ts.year, ts.month, ts.day, ts.hour, ts.minute, ts.second
    tz = ts.tzinfo
    if unit == "year":
        return _dt.datetime(y, 1, 1, tzinfo=tz)
    if unit == "quarter":
        return _dt.datetime(y, ((mo - 1) // 3) * 3 + 1, 1, tzinfo=tz)
    if unit == "month":
        return _dt.datetime(y, mo, 1, tzinfo=tz)
    if unit == "week":
        monday = ts - _dt.timedelta(days=ts.weekday())
        return _dt.datetime(monday.year, monday.month, monday.day, tzinfo=tz)
    if unit == "day":
        return _dt.datetime(y, mo, d, tzinfo=tz)
    if unit == "hour":
        return _dt.datetime(y, mo, d, h, tzinfo=tz)
    if unit == "minute":
        return _dt.datetime(y, mo, d, h, mi, tzinfo=tz)
    if unit == "second":
        return _dt.datetime(y, mo, d, h, mi, s, tzinfo=tz)
    raise errors.feature_not_supported(f"unsupported date_trunc unit: {unit}")


_MICROS_PER = {
    "microsecond": 1,
    "millisecond": 1_000,
    "second": 1_000_000,
    "minute": 60 * 1_000_000,
    "hour": 3600 * 1_000_000,
}


def _date_trunc_interval(unit: str, src: Any, _intervals: Any) -> Any:
    """``date_trunc(unit, interval)`` — zero every interval component finer than
    ``unit``. Postgres orders components years > months > days > time; ``week``
    is not a valid unit for an interval."""
    iv = src["interval"]
    months = int(iv.get("months", 0))
    days = int(iv.get("days", 0))
    micros = int(iv.get("micros", 0))
    if unit in ("millennium", "century", "decade", "year"):
        step = {"millennium": 12000, "century": 1200, "decade": 120, "year": 12}[unit]
        return _intervals.make((months // step) * step, 0, 0)
    if unit == "quarter":
        return _intervals.make((months // 3) * 3, 0, 0)
    if unit == "month":
        return _intervals.make(months, 0, 0)
    if unit == "day":
        return _intervals.make(months, days, 0)
    if unit in _MICROS_PER:
        per = _MICROS_PER[unit]
        return _intervals.make(months, days, (micros // per) * per)
    raise errors.feature_not_supported(f'unit "{unit}" not supported for interval date_trunc')


# sqlglot's Postgres dialect already normalises the standard ``to_char`` tokens
# (YYYY / MM / DD / HH24 / MI / SS …) to strftime directives; only the word-form
# tokens are left as literals, so we map just those (longest-first) and then
# strftime once. Existing ``%X`` directives are copied through untouched.
_PG_WORD_TOKENS = [
    ("MONTH", "%B"),
    ("MON", "%b"),
    ("DAY", "%A"),
    ("DY", "%a"),
    ("AM", "%p"),
    ("PM", "%p"),
]


def _eval_to_char(node: exp.TimeToStr, scope: Scope, ctx: ScalarContext) -> Any:
    """``to_char(ts, 'YYYY-MM-DD HH24:MI:SS')`` (timestamps) or
    ``to_char(1234.5, '999,999.99')`` (numbers) -> a formatted string."""
    src = evaluate(node.this, scope, ctx)
    fmt_node = node.args.get("format")
    if src is None or fmt_node is None:
        return None
    fmt = _as_text(
        evaluate(fmt_node, scope, ctx) if isinstance(fmt_node, exp.Expression) else fmt_node
    )
    # A numeric source with a numeric template -> the numeric formatter.
    if isinstance(src, (int, float, Decimal, bson.Decimal128)) and not isinstance(src, bool):
        from secantus.sql import numformat as _numformat

        num = src.to_decimal() if isinstance(src, bson.Decimal128) else src
        return _numformat.to_char_numeric(num, fmt)
    ts = _as_datetime(src)
    if not isinstance(ts, _dt.datetime):
        ts = _dt.datetime(ts.year, ts.month, ts.day)
    out, i = [], 0
    up = fmt.upper()
    while i < len(fmt):
        if fmt[i] == "%" and i + 1 < len(fmt):
            out.append(fmt[i : i + 2])  # already a strftime directive
            i += 2
            continue
        for pat, directive in _PG_WORD_TOKENS:
            if up.startswith(pat, i):
                out.append(directive)
                i += len(pat)
                break
        else:
            out.append(fmt[i])
            i += 1
    return ts.strftime("".join(out))


def _utcnow(ctx: ScalarContext | None) -> _dt.datetime:
    return _dt.datetime.now(_dt.timezone.utc)


def _fmt_current_time(ctx: ScalarContext | None) -> str:
    """``current_time`` -> a ``timetz`` string at UTC."""
    from secantus.sql import datetimes as _datetimes

    return _datetimes.parse_timetz(_utcnow(ctx).strftime("%H:%M:%S") + "+00:00")


def _eval_make_interval(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> dict:
    """``make_interval(years, months, weeks, days, hours, mins, secs)`` — parsed to
    a dedicated node whose components live in named args."""
    from secantus.sql import intervals as _intervals

    def arg(name: str) -> float:
        a = node.args.get(name)
        v = evaluate(a, scope, ctx) if a is not None else None
        return float(v) if v is not None else 0.0

    months = int(arg("year")) * 12 + int(arg("month"))
    days = int(arg("week")) * 7 + int(arg("day"))
    micros = round(
        (arg("hour") * 3600 + arg("minute") * 60 + arg("second")) * _intervals.MICROS_PER_SECOND
    )
    return _intervals.make(months, days, micros)


def _eval_justify(fn: str) -> Callable[[exp.Expression, Scope, ScalarContext], Any]:
    def handler(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
        from secantus.sql import intervals as _intervals

        v = evaluate(node.this, scope, ctx)
        return None if v is None else getattr(_intervals, fn)(v)

    return handler


# Typed scalar function nodes (sqlglot parses these to dedicated classes whose
# operands live in ``this`` / named args, not ``expressions``).
def _eval_xmlelement(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``xmlelement(NAME tag [, xmlattributes(v AS k, …)], content…)`` — build an
    XML element. The name is in ``this``; the first ``xmlattributes(...)`` in
    ``expressions`` supplies attributes and the rest are content."""
    from secantus.sql import xmltype as _xmltype

    name = node.this.name if isinstance(node.this, exp.Identifier) else _as_text(node.this)
    attributes: list[tuple[str, Any]] = []
    content: list[Any] = []
    for e in node.expressions:
        if isinstance(e, exp.Anonymous) and str(e.this).lower() == "xmlattributes":
            for a in e.expressions:
                key = a.alias if isinstance(a, exp.Alias) else _as_text(a)
                inner = a.this if isinstance(a, exp.Alias) else a
                attributes.append((key, evaluate(inner, scope, ctx)))
        else:
            content.append(evaluate(e, scope, ctx))
    return _xmltype.element(name, attributes, content)


def _eval_encode(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``encode(bytea, fmt)`` — the dedicated ``exp.Encode`` node carries the data
    in ``this`` and the format in ``charset``."""
    from secantus.sql import bytea as _bytea

    data = evaluate(node.this, scope, ctx)
    fmt = node.args.get("charset")
    if data is None or fmt is None:
        return None
    return _bytea.encode(data, _as_text(evaluate(fmt, scope, ctx)))


def _eval_decode(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``decode(text, fmt)`` — the dedicated ``exp.Decode`` node carries the text
    in ``this`` and the format in ``charset``."""
    from secantus.sql import bytea as _bytea

    text = evaluate(node.this, scope, ctx)
    fmt = node.args.get("charset")
    if text is None or fmt is None:
        return None
    return _bytea.decode(text, _as_text(evaluate(fmt, scope, ctx)))


_SCALAR_FUNC_NODES: dict[type, Callable[[exp.Expression, Scope, ScalarContext], Any]] = {
    # ``upper`` / ``lower`` are overloaded: a range operand yields its bound, any
    # other operand is the string case-shift.
    exp.Upper: _unary(lambda v: v.get("upper") if isinstance(v, dict) else _as_text(v).upper()),
    exp.Lower: _unary(lambda v: v.get("lower") if isinstance(v, dict) else _as_text(v).lower()),
    # ``length()`` — a bytea's byte count, else the string's character length.
    exp.Length: _unary(lambda v: len(v) if isinstance(v, (bytes, bytearray)) else len(_as_text(v))),
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
    exp.ArrayAppend: _eval_array_append,
    exp.ArrayPrepend: _eval_array_prepend,
    exp.ArrayConcat: _eval_array_cat,
    exp.ArrayPosition: _eval_array_position,
    exp.ArrayRemove: _eval_array_remove,
    exp.ArrayToString: _eval_array_to_string,
    exp.RegexpReplace: _eval_regexp_replace,
    exp.SplitPart: _eval_split_part,
}
# Node names vary across sqlglot versions; look them up by attribute so a missing
# class doesn't break import.
for _cls_name, _handler in (
    ("Translate", _eval_translate),
    ("RegexpCount", _eval_regexp_count),
    ("Trunc", _eval_trunc),
    ("Log", _eval_log),
    ("Pi", _eval_pi),
    ("Sqrt", _unary(math.sqrt)),
    ("Cbrt", _unary(_cbrt)),
    ("Sign", _unary(_sign)),
    ("Ln", _unary(math.log)),
    ("Exp", _unary(math.exp)),
    ("Degrees", _unary(math.degrees)),
    ("Radians", _unary(math.radians)),
    ("Factorial", _unary(lambda v: math.factorial(int(v)))),
    ("Extract", _eval_extract),
    ("TimestampTrunc", _eval_date_trunc),
    ("TimeToStr", _eval_to_char),
    ("CurrentTimestamp", lambda n, s, c: _utcnow(c)),
    ("CurrentDate", lambda n, s, c: _utcnow(c).date()),
    ("CurrentTime", lambda n, s, c: _fmt_current_time(c)),
    ("Pad", _eval_pad),
    ("Left", _eval_left),
    ("Right", _eval_right),
    ("Repeat", _eval_repeat),
    ("Reverse", _unary(lambda v: _as_text(v)[::-1])),
    ("Initcap", _unary(lambda v: _as_text(v).title())),
    ("Ascii", _eval_ascii),
    ("Chr", _eval_chr),
    ("StrPosition", _eval_str_position),
    ("Overlay", _eval_overlay),
    # ``bit_length`` — a bytea's byte count x8, else a bit string's bit count.
    (
        "BitLength",
        _unary(lambda v: 8 * len(v) if isinstance(v, (bytes, bytearray)) else len(_as_text(v))),
    ),
    # Interval functions with dedicated sqlglot nodes.
    ("MakeInterval", _eval_make_interval),
    ("JustifyDays", _eval_justify("justify_days")),
    ("JustifyHours", _eval_justify("justify_hours")),
    ("JustifyInterval", _eval_justify("justify_interval")),
    # bytea encode / decode carry their format in ``charset``, not ``expressions``.
    ("Encode", _eval_encode),
    ("Decode", _eval_decode),
    # ``xmlelement`` has a dedicated node (name in ``this``, args in ``expressions``).
    ("XMLElement", _eval_xmlelement),
):
    _cls = getattr(exp, _cls_name, None)
    if _cls is not None:
        _SCALAR_FUNC_NODES[_cls] = _handler


def _eval_compare(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    left = evaluate(node.this, scope, ctx)
    right = evaluate(node.expression, scope, ctx)
    if left is None or right is None:
        return None  # three-valued logic: comparison with NULL is unknown
    left, right = _unwrap_decimal(left), _unwrap_decimal(right)
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


def _is_bit_expr(node: exp.Expression) -> bool:
    """Whether ``node`` statically denotes a bit string — a ``B'…'`` literal or a
    cast to ``bit`` / ``varbit`` (through parens). Used so ``b'1010'::int`` reads
    the operand as a bit string while ``'1010'::int`` reads it as decimal text."""
    if isinstance(node, exp.Paren):
        return _is_bit_expr(node.this)
    if isinstance(node, exp.BitString):
        return True
    if isinstance(node, exp.Cast) and node.to is not None:
        return typemap.type_tag_for_sql(node.to) in typemap._BIT_TAGS
    return False


def _eval_cast(node: exp.Cast, scope: Scope, ctx: ScalarContext) -> Any:
    value = evaluate(node.this, scope, ctx)
    # ``'int4'::regtype`` — normalize the type name to its canonical pretty
    # spelling so it compares equal to what ``pg_typeof`` prints.
    if (
        value is not None
        and isinstance(node.to, exp.ObjectIdentifier)
        and str(node.to.this).upper() == "REGTYPE"
    ):
        return typemap.normalize_regtype(str(value))
    # ``'[1,10)'::int4range`` — parse a range text literal into its subdocument.
    to = node.to.sql(dialect="postgres").lower().strip() if node.to is not None else ""
    # ``'1 day'::interval`` — parse an interval literal (a subdoc passes through).
    to_tag_early = typemap.type_tag_for_sql(node.to) if node.to is not None else None
    if value is not None and to_tag_early == "interval":
        from secantus.sql import intervals as _intervals

        return value if _intervals.is_interval(value) else _intervals.parse(str(value))
    if value is not None and to_tag_early == "uuid":
        from secantus.sql import uuidtype as _uuidtype

        return _uuidtype.normalize(value)
    if value is not None and to_tag_early == "money":
        from secantus.sql import numformat as _numformat

        return _numformat.parse_money(value)
    if value is not None and to_tag_early in typemap._GEO_TAGS:
        from secantus.sql import pggeo as _pggeo

        return _pggeo.canonical(value, to_tag_early)
    if value is not None and to_tag_early == "bytea":
        from secantus.sql import bytea as _bytea

        return _bytea.parse(value)
    if value is not None and to_tag_early == "hstore":
        from secantus.sql import hstore as _hstore

        return _hstore.parse(value)
    if value is not None and to_tag_early == "xml":
        from secantus.sql import xmltype as _xmltype

        return _xmltype.parse(value)
    if value is not None and to_tag_early in ("date", "time", "timetz"):
        from secantus.sql import datetimes as _datetimes

        if to_tag_early == "date":
            return _datetimes.parse_date(value)
        if to_tag_early == "time":
            return _datetimes.parse_time(value)
        return _datetimes.parse_timetz(value)
    # ``timestamp '2020-01-31'`` / ``timestamptz '…'`` -> a real datetime so
    # interval and date arithmetic land on a temporal value rather than a bare
    # string. ``timestamp`` is naive (any offset dropped); ``timestamptz`` keeps
    # the parsed instant. Casting an existing datetime to ``timestamp`` strips tz.
    if value is not None and to_tag_early in ("timestamp", "timestamptz"):
        if isinstance(value, _dt.datetime):
            if to_tag_early == "timestamp" and value.tzinfo is not None:
                return value.replace(tzinfo=None)
            return value
        if not isinstance(value, _dt.date):
            try:
                return typemap.coerce(value, to_tag_early)
            except ValueError:
                return value
    if (
        value is not None
        and (
            to in typemap._RANGE_TAGS
            or to in typemap._MULTIRANGE_TAGS
            or to in typemap._FTS_TAGS
            or to in typemap._NET_TAGS
        )
        and not isinstance(value, dict)
    ):
        return typemap.coerce(value, to)
    # Bit-string casts: ``::bit(n)`` / ``::varbit`` (from a '0'/'1' string or an
    # integer) and ``bit::int``.
    to_tag = typemap.type_tag_for_sql(node.to) if node.to is not None else None
    if value is not None and to_tag in typemap._BIT_TAGS:
        from secantus.sql import bitstr as _bitstr

        length = _bit_cast_length(node.to)
        if isinstance(value, int) and not isinstance(value, bool):
            return _bitstr.from_int(
                value, length if length is not None else max(int(value).bit_length(), 1)
            )
        return _bitstr.normalize(value, length=length, varying=(to_tag == "varbit"))
    if (
        value is not None
        and _is_bit_expr(node.this)
        and to_tag in ("int2", "int4", "int8", "numeric", "float4", "float8")
    ):
        from secantus.sql import bitstr as _bitstr

        n = _bitstr.to_int(str(value))
        return float(n) if to_tag in ("float4", "float8") else n
    # Otherwise we don't model regclass/oid identity types; evaluating the inner
    # value is enough for the catalog queries that use casts (compared / discarded,
    # never round-tripped through a real type).
    return value


def _bit_cast_length(datatype: exp.DataType | None) -> int | None:
    """The declared length of a ``bit(n)`` / ``varbit(n)`` cast target, or None."""
    if datatype is None:
        return None
    params = datatype.args.get("expressions") or []
    for p in params:
        lit = p.this if isinstance(p, exp.DataTypeParam) else p
        if isinstance(lit, exp.Literal) and not lit.is_string:
            try:
                return int(lit.this)
            except (TypeError, ValueError):
                return None
    return None


def _func_name(node: exp.Anonymous) -> str:
    name = node.this if isinstance(node.this, str) else node.name
    return str(name).rsplit(".", 1)[-1].lower()


def _eval_func(node: exp.Anonymous, scope: Scope, ctx: ScalarContext) -> Any:
    name = _func_name(node)
    if name == "xmlforest":
        # ``xmlforest(value AS name, …)`` needs the per-arg aliases, which are lost
        # once the args are flattened to values.
        from secantus.sql import xmltype as _xmltype

        pairs: list[tuple[str, Any]] = []
        for e in node.expressions:
            label = e.alias if isinstance(e, exp.Alias) else _column_name_of(e)
            inner = e.this if isinstance(e, exp.Alias) else e
            pairs.append((label, evaluate(inner, scope, ctx)))
        return _xmltype.forest(pairs)
    args = [evaluate(a, scope, ctx) for a in node.expressions]
    return _call_func(name, args, ctx)


def _column_name_of(node: exp.Expression) -> str:
    """The implicit element name for an unaliased ``xmlforest`` operand — its column
    name (Postgres uses the column name when no ``AS`` is given)."""
    return node.name if isinstance(node, exp.Column) else _as_text(node)


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


def _has_table_privilege(args: list[Any], ctx: ScalarContext | None) -> Any:
    """``has_table_privilege([user,] table, privilege)`` — reflects the table-level
    grants recorded by GRANT/REVOKE. Two-arg form checks the current user (the
    SET ROLE override, else the session user); the three-arg form checks a named
    user. The privilege may carry a trailing ``WITH GRANT OPTION`` (ignored).
    Returns a bool, or NULL on a NULL argument."""
    if ctx is None or ctx.catalog is None:
        raise errors.feature_not_supported("has_table_privilege() requires an execution context")
    if len(args) >= 3:
        user, table, privilege = args[0], args[1], args[2]
    elif len(args) == 2:
        user, table, privilege = ctx.session.effective_user, args[0], args[1]
    else:
        raise errors.SQLError("42883", "has_table_privilege() requires (table, privilege)")
    if table is None or privilege is None or user is None:
        return None
    priv = _as_text(privilege).split("WITH")[0].strip().upper()
    grantees = {_as_text(user), "PUBLIC", "public"}
    return ctx.catalog.has_table_privilege(ctx.db, _as_text(table), grantees, priv)


def _has_column_privilege(args: list[Any], ctx: ScalarContext | None) -> Any:
    """``has_column_privilege([user,] table, column, privilege)`` — reflects the
    column-level grants recorded by GRANT (a whole-table grant satisfies it too).
    Two-arg-less forms mirror ``has_table_privilege``; NULL argument → NULL."""
    if ctx is None or ctx.catalog is None:
        raise errors.feature_not_supported("has_column_privilege() requires an execution context")
    if len(args) >= 4:
        user, table, column, privilege = args[0], args[1], args[2], args[3]
    elif len(args) == 3:
        user, table, column, privilege = ctx.session.effective_user, args[0], args[1], args[2]
    else:
        raise errors.SQLError("42883", "has_column_privilege() requires (table, column, privilege)")
    if user is None or table is None or column is None or privilege is None:
        return None
    priv = _as_text(privilege).split("WITH")[0].strip().upper()
    grantees = {_as_text(user), "PUBLIC", "public"}
    return ctx.catalog.has_column_privilege(
        ctx.db, _as_text(table), grantees, _as_text(column), priv
    )


def _advisory_key(args: list[Any]) -> tuple[int, int, int]:
    """Split advisory-lock arguments into the ``(classid, objid, objsubid)``
    triple ``pg_locks`` reports. A single ``bigint`` key splits into two signed
    32-bit halves (objsubid 1); a ``(int4, int4)`` pair maps straight through
    (objsubid 2) — matching real Postgres."""
    if len(args) >= 2 and args[1] is not None:
        return (int(args[0]), int(args[1]), 2)
    k = int(args[0])
    hi = (k >> 32) & 0xFFFFFFFF
    lo = k & 0xFFFFFFFF
    classid = hi - 0x100000000 if hi >= 0x80000000 else hi
    objid = lo - 0x100000000 if lo >= 0x80000000 else lo
    return (classid, objid, 1)


def _advisory_lock(name: str, args: list[Any], ctx: ScalarContext | None) -> Any:
    """The ``pg_advisory_lock`` family (#135). Single-node: a lock is always
    granted immediately, so acquisition is a no-op that just records what the
    session holds (for ``pg_advisory_unlock`` truthfulness + ``pg_locks``
    reflection). ``pg_try_*`` always succeed (``True``); ``pg_advisory_unlock*``
    return whether a matching session-level lock was held; the void-returning
    forms return ``None``."""
    if ctx is None:
        return None  # embedded / no session — nothing to track
    session = ctx.session
    if name == "pg_advisory_unlock_all":
        session.advisory_unlock_all()
        return None
    if not args or args[0] is None:
        return None
    key = _advisory_key(args)
    shared = name.endswith("_shared")
    base = name[: -len("_shared")] if shared else name
    if base == "pg_advisory_unlock":
        return session.advisory_lock_release(key, shared=shared)
    xact = "xact" in base
    session.advisory_lock_acquire(key, shared=shared, xact=xact)
    if base.startswith("pg_try_"):
        return True  # single-node: always acquirable
    return None  # pg_advisory_lock* return void


def _call_func(name: str, args: list[Any], ctx: ScalarContext | None = None) -> Any:
    if name == "has_column_privilege":
        return _has_column_privilege(args, ctx)
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
    if name in ("pg_get_functiondef", "pg_get_function_arguments", "pg_get_function_result"):
        # Reconstruct a user function's definition / argument list / result type
        # (by pg_proc oid) so psql's \df and SQLAlchemy reflect it. (#130)
        if ctx is not None and args and isinstance(args[0], int):
            from secantus.sql import virtual

            fn = {
                "pg_get_functiondef": virtual.functiondef_for_oid,
                "pg_get_function_arguments": virtual.function_arguments_for_oid,
                "pg_get_function_result": virtual.function_result_for_oid,
            }[name]
            return fn(ctx.db, ctx.catalog, args[0])
        return None
    if name == "has_table_privilege":
        return _has_table_privilege(args, ctx)
    if name in ("nextval", "currval", "setval", "lastval"):
        return _sequence_func(name, args, ctx)
    if name == "pg_get_indexdef":
        # Render an index's CREATE INDEX text (by pg_index/pg_class oid) so psql's
        # \d and SQLAlchemy reflect it; unknown oid / no ctx → NULL. (#134)
        if ctx is not None and args and isinstance(args[0], int):
            from secantus.sql import virtual

            return virtual.indexdef_for_oid(ctx.db, ctx.storage, ctx.catalog, args[0])
        return None
    if name in ("pg_get_expr", "pg_get_serial_sequence"):
        # No stored defaults / serial-sequence resolution.
        return None
    if name == "regexp_matches":
        # Postgres regexp_matches is set-returning; in a scalar context we return
        # the first match's capture groups as a text[] (whole match if no groups),
        # or NULL when there is no match. (One match is the common non-'g' case.)
        src = args[0] if args else None
        pattern = args[1] if len(args) > 1 else None
        if src is None or pattern is None:
            return None
        flags = _as_text(args[2]) if len(args) > 2 else ""
        m = _re_compile(_as_text(pattern), flags).search(_as_text(src))
        if m is None:
            return None
        return list(m.groups()) if m.groups() else [m.group(0)]
    if name in ("json_build_object", "jsonb_build_object"):
        out: dict[str, Any] = {}
        for i in range(0, len(args) - 1, 2):
            out[str(args[i])] = args[i + 1]
        return out
    if name in ("json_build_array", "jsonb_build_array"):
        return list(args)
    if name in ("to_jsonb", "to_json", "row_to_json"):
        # Values are already stored as native Python (dict / list / scalar) that
        # renders as json on the wire, so the conversion is the identity — a
        # composite / ROW(...) argument arrives as a subdocument, a scalar as itself.
        return _as_json_value(args[0]) if args else None
    if name in ("jsonb_array_length", "json_array_length"):
        v = args[0] if args else None
        if not isinstance(v, list):
            raise errors.SQLError("22023", "cannot get array length of a non-array")
        return len(v)
    if name in ("array_length", "cardinality", "array_ndims", "array_upper", "array_lower"):
        v = args[0] if args else None
        if not isinstance(v, (list, tuple)):
            return None
        dims = _array_dim_lengths(v)
        if name == "cardinality":
            n = 1
            for d in dims:
                n *= d
            return n if dims else 0
        if name == "array_ndims":
            return len(dims) or None
        # array_length / array_upper / array_lower take a 1-based dimension.
        dim = int(args[1]) if len(args) > 1 and args[1] is not None else 1
        if dim < 1 or dim > len(dims):
            return None
        if name == "array_lower":
            return 1  # Postgres arrays are 1-based by default
        return dims[dim - 1]  # array_length == array_upper (lower is 1)
    if name == "array_dims":
        v = args[0] if args else None
        if not isinstance(v, (list, tuple)):
            return None
        dims = _array_dim_lengths(v)
        return "".join(f"[1:{d}]" for d in dims) if dims else None
    if name in ("jsonb_typeof", "json_typeof"):
        return _json_typeof(args[0] if args else None)
    if name in ("jsonb_set", "jsonb_set_lax"):
        target, path, value = args[0], _pg_text_path(args[1]), _as_json_value(args[2])
        create = args[3] if len(args) > 3 else True
        return _jsonb_set(target, path, value, create=bool(create), insert=False)
    if name == "jsonb_insert":
        target, path, value = args[0], _pg_text_path(args[1]), _as_json_value(args[2])
        after = bool(args[3]) if len(args) > 3 else False
        return _jsonb_set(target, path, value, create=True, insert=True, insert_after=after)
    if name in ("jsonb_strip_nulls", "json_strip_nulls"):
        return _jsonb_strip_nulls(args[0] if args else None)
    if name in ("jsonb_pretty",):
        v = args[0] if args else None
        return None if v is None else json.dumps(v, indent=4, default=str)
    if name == "coalesce":
        for a in args:
            if a is not None:
                return a
        return None
    if name in ("gcd", "lcm"):
        a = args[0] if args else None
        b = args[1] if len(args) > 1 else None
        if a is None or b is None:
            return None
        return math.gcd(int(a), int(b)) if name == "gcd" else math.lcm(int(a), int(b))
    if name == "log10":
        v = args[0] if args else None
        return None if v is None else math.log10(v)
    if name in typemap._RANGE_TAGS:
        # ``int4range(lo, hi [, bounds])`` etc. -> a range subdocument.
        from secantus.sql import ranges as _ranges

        lo = args[0] if args else None
        hi = args[1] if len(args) > 1 else None
        bounds = _as_text(args[2]) if len(args) > 2 else "[)"
        return _ranges.make_range(lo, hi, bounds, name)
    if name in typemap._MULTIRANGE_TAGS:
        # ``int4multirange(r1, r2, …)`` -> a coalesced multirange subdocument.
        from secantus.sql import ranges as _ranges

        return _ranges.make_multirange([a for a in args if a is not None])
    if name == "range_merge":
        from secantus.sql import ranges as _ranges

        a = args[0] if args else None
        b = args[1] if len(args) > 1 else None
        if a is None or b is None:
            return None
        return _ranges.merge(a, b)
    if name in (
        "to_tsvector",
        "to_tsquery",
        "plainto_tsquery",
        "phraseto_tsquery",
        "websearch_to_tsquery",
    ):
        from secantus.sql import fts as _fts

        # A two-argument form passes the text-search config first; we ignore it
        # (the config is fixed) and read the last argument as the document / query.
        text = args[-1] if args else None
        if text is None:
            return None
        if name == "to_tsvector":
            return _fts.to_tsvector(_as_text(text))
        if name == "plainto_tsquery":
            return _fts.plainto_tsquery(_as_text(text))
        if name == "phraseto_tsquery":
            return _fts.phraseto_tsquery(_as_text(text))
        if name == "websearch_to_tsquery":
            return _fts.websearch_to_tsquery(_as_text(text))
        return _fts.to_tsquery(_as_text(text))
    if name == "ts_headline":
        from secantus.sql import fts as _fts

        # ts_headline([config,] document, query [, options]) — ignore config /
        # options; the query is the tsquery arg and the document is the text arg
        # immediately before it.
        q_idx = next((i for i, a in enumerate(args) if _fts.is_tsquery(a)), None)
        if q_idx is None or q_idx == 0:
            return _as_text(args[0]) if args else None
        return _fts.ts_headline(_as_text(args[q_idx - 1]), args[q_idx])
    if name in ("ts_rank", "ts_rank_cd"):
        from secantus.sql import fts as _fts

        vec = args[0] if args else None
        query = args[1] if len(args) > 1 else None
        if vec is None or query is None:
            return None
        return _fts.ts_rank(vec, query)
    if name in (
        "host",
        "masklen",
        "network",
        "netmask",
        "broadcast",
        "family",
        "abbrev",
        "hostmask",
    ):
        from secantus.sql import net as _net

        v = args[0] if args else None
        if v is None:
            return None
        if name == "host":
            return _net.host(v)
        if name == "masklen":
            return _net.masklen(v)
        if name == "network":
            return _net.network(v)
        if name == "netmask":
            return _net.netmask(v)
        if name == "broadcast":
            return _net.broadcast(v)
        if name == "family":
            return _net.family(v)
        if name == "hostmask":
            return _net.netmask(v)  # close enough for the common /24 cases
        # abbrev: render the inet abbreviated form (drops a full-host mask).
        return _net.render_inet(v)
    if name in ("get_byte", "set_byte"):
        from secantus.sql import bytea as _bytea

        v = args[0] if args else None
        if v is None or args[1] is None:
            return None
        if name == "get_byte":
            return _bytea.get_byte(v, int(args[1]))
        return _bytea.set_byte(v, int(args[1]), int(args[2]))
    if name in ("bit_length", "octet_length") and args and isinstance(args[0], (bytes, bytearray)):
        # bytea overloads: octet_length -> byte count; bit_length -> 8x that.
        return len(args[0]) if name == "octet_length" else 8 * len(args[0])
    if name in ("set_bit", "bit_length", "octet_length"):
        from secantus.sql import bitstr as _bitstr

        v = args[0] if args else None
        if v is None:
            return None
        bits = str(v)
        if name == "bit_length":
            return _bitstr.bit_length(bits)
        if name == "octet_length":
            return _bitstr.octet_length(bits)
        # set_bit(bits, n, newvalue)
        return _bitstr.set_bit(bits, int(args[1]), int(args[2]))
    if name == "get_bit":
        from secantus.sql import bitstr as _bitstr

        v = args[0] if args else None
        return None if v is None or args[1] is None else _bitstr.get_bit(str(v), int(args[1]))
    if name in (
        "hstore",
        "akeys",
        "avals",
        "hstore_to_json",
        "hstore_to_jsonb",
        "delete",
        "defined",
    ):
        from secantus.sql import hstore as _hstore

        if name == "hstore":
            if len(args) == 2 and isinstance(args[0], (list, tuple)):
                return _hstore.from_arrays(args[0], args[1])
            if len(args) == 2:
                return _hstore.from_pair(args[0], args[1])
        v = args[0] if args else None
        if v is None:
            return None
        if name == "akeys":
            return _hstore.akeys(v)
        if name == "avals":
            return _hstore.avals(v)
        if name in ("hstore_to_json", "hstore_to_jsonb"):
            return _hstore.to_json(v)
        if name == "delete":
            return _hstore.delete(v, args[1])
        if name == "defined":
            return _hstore.defined(v, args[1])
    if name in ("xml_is_well_formed", "xml_is_well_formed_document", "xpath", "xmlconcat"):
        from secantus.sql import xmltype as _xmltype

        if name in ("xml_is_well_formed", "xml_is_well_formed_document"):
            return None if not args else _xmltype.is_well_formed(args[0])
        if name == "xmlconcat":
            return _xmltype.concat(*args)
        # xpath(expr, xml [, nsarray]) -> a text array of matched nodes.
        if len(args) < 2 or args[0] is None or args[1] is None:
            return None
        return _xmltype.xpath(_as_text(args[0]), args[1])
    if name.startswith("pg_advisory_") or name.startswith("pg_try_advisory_"):
        return _advisory_lock(name, args, ctx)
    if name == "pg_notify":
        # ``pg_notify(channel, payload)`` — the function form of NOTIFY.
        if ctx is not None and args and args[0] is not None:
            session = ctx.session
            channel, payload = str(args[0]), (_as_text(args[1]) if len(args) > 1 else "")
            hub = getattr(session, "notify_hub", None)
            if hub is not None:
                if session.txn_handle is not None:
                    session.pending_notifies.append((channel, payload))
                else:
                    hub.notify(channel, payload, session.backend_pid)
        return None
    if name in ("gen_random_uuid", "uuid_generate_v4", "uuid_generate_v1"):
        from secantus.sql import uuidtype as _uuidtype

        return _uuidtype.generate()
    if name in ("justify_days", "justify_hours", "justify_interval"):
        from secantus.sql import intervals as _intervals

        v = args[0] if args else None
        if v is None:
            return None
        return getattr(_intervals, name)(v)
    if name == "make_interval":
        from secantus.sql import intervals as _intervals

        # Positional: (years, months, weeks, days, hours, mins, secs).
        vals = [int(a) if a is not None else 0 for a in args[:6]] + [
            float(args[6]) if len(args) > 6 and args[6] is not None else 0.0
        ]
        years, months, weeks, days, hours, mins, secs = (vals + [0] * 7)[:7]
        return _intervals.make(
            years * 12 + months,
            weeks * 7 + days,
            round(((hours * 3600) + (mins * 60) + secs) * _intervals.MICROS_PER_SECOND),
        )
    if name == "age":
        from secantus.sql import intervals as _intervals

        if not args or args[0] is None:
            return None
        if len(args) >= 2 and args[1] is not None:
            return _intervals.age(args[0], args[1])
        return _intervals.age(_dt.datetime.now(_dt.timezone.utc).date(), args[0])
    if name == "isempty":
        from secantus.sql import ranges as _ranges

        v = args[0] if args else None
        return None if v is None else _ranges.is_empty(v)
    if name in ("lower", "upper") and args and isinstance(args[0], dict):
        # lower()/upper() on a range value (the string-function overloads are the
        # exp.Lower/exp.Upper nodes; a dict operand here is a range subdocument).
        from secantus.sql import ranges as _ranges

        return _ranges.lower_bound(args[0]) if name == "lower" else _ranges.upper_bound(args[0])
    if name in (
        "jsonb_path_query",
        "jsonb_path_query_array",
        "jsonb_path_exists",
        "jsonb_path_match",
    ):
        from secantus.sql import jsonpath as _jsonpath

        doc = args[0] if args else None
        path = args[1] if len(args) > 1 else None
        if doc is None or path is None:
            return None
        try:
            if name == "jsonb_path_exists":
                return _jsonpath.exists(doc, _as_text(path))
            if name == "jsonb_path_match":
                return _jsonpath.match(doc, _as_text(path))
            matches = _jsonpath.query(doc, _as_text(path))
        except _jsonpath.JsonPathError as e:
            raise errors.feature_not_supported(f"unsupported jsonpath: {e}") from e
        if name == "jsonb_path_query_array":
            return matches
        # jsonb_path_query is set-returning; in a scalar context return the first
        # match (NULL when the path matches nothing).
        return matches[0] if matches else None
    if ctx is not None and getattr(ctx, "catalog", None) is not None:
        udf = ctx.catalog.get_function(ctx.db, name, len(args))
        if udf is not None:
            return _invoke_udf(udf, args, ctx)
    raise errors.feature_not_supported(f"function {name}() is not supported in this context")


_UDF_MISSING = object()


def _invoke_udf(func: dict, args: list[Any], ctx: ScalarContext) -> Any:
    """Evaluate a ``LANGUAGE sql`` user-defined function: bind the call arguments
    to the body's parameters (named columns and/or positional ``$N``) and reduce
    the single-statement body to its scalar result via the subquery machinery."""
    from secantus.sql import planner as _planner

    if func.get("language") == "plpgsql":
        from secantus.sql import plpgsql

        return plpgsql.invoke(func, args, ctx)

    body = _planner.parse(func["body"])[0]
    if not isinstance(body, exp.Select):
        raise errors.feature_not_supported("a SQL function body must be a SELECT")
    body = body.copy()
    # Positional $1..$N placeholders -> literal argument nodes.
    for p in list(body.find_all(exp.Parameter)):
        try:
            idx = int(p.name) - 1
        except (TypeError, ValueError):
            continue
        if 0 <= idx < len(args):
            p.replace(_planner._value_to_node(args[idx]))
    # Named parameters resolve as columns in the body's scope.
    params = func.get("params") or []
    name_to_val = {
        str(n).lower(): args[i] for i, n in enumerate(params) if n is not None and i < len(args)
    }

    def _param_scope(col: exp.Column) -> Any:
        val = name_to_val.get(col.name.lower(), _UDF_MISSING)
        if val is _UDF_MISSING:
            raise errors.SQLError("42703", f'column "{col.name}" does not exist')
        return val

    result = _eval_subquery(body, _param_scope, ctx)
    return_tag = func.get("return_tag")
    if return_tag and result is not None:
        with contextlib.suppress(errors.SQLError, ValueError, TypeError):
            result = typemap.coerce(result, return_tag)
    return result


def _eval_jsonb_nav(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """Resolve a ->/->>/#>/#>> navigation against the current row value."""
    from secantus.sql.planner import _json_keys

    val = evaluate(node.this, scope, ctx)
    keys = _json_keys(node.expression)
    # ``hstore -> key`` is a flat string lookup (returns text / NULL), not a JSON
    # descent — disambiguated on the hstore tag.
    from secantus.sql import hstore as _hstore

    if _hstore.is_hstore(val):
        return _hstore.lookup(val, keys[0]) if keys else None
    for key in keys:
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


def _pg_text_path(value: Any) -> list[str]:
    """Parse a jsonb function's ``path`` argument — a Postgres ``text[]`` given
    either as a Python list or a ``'{a,b}'`` string literal — into a key list."""
    if isinstance(value, (list, tuple)):
        return [str(v) for v in value]
    from secantus.sql import typemap

    return [str(v) for v in typemap._parse_pg_array_literal(str(value))]


def _as_json_value(value: Any) -> Any:
    """Coerce a ``jsonb`` value argument: a string is parsed as JSON (so ``'5'`` ->
    5, ``'{"k":1}'`` -> dict) when it parses, else used verbatim."""
    if isinstance(value, str):
        try:
            return json.loads(value)
        except (ValueError, TypeError):
            return value
    return value


def _jsonb_set(
    target: Any,
    keys: list[str],
    value: Any,
    *,
    create: bool,
    insert: bool = False,
    insert_after: bool = False,
) -> Any:
    """``jsonb_set`` / ``jsonb_insert`` core: return a copy of ``target`` with
    ``value`` set (or inserted) at the ``keys`` path. ``create`` adds missing
    object keys; ``insert`` only adds a new element (object key must be absent, or
    array position ``insert_after``-adjusted), leaving an existing value alone."""
    import copy

    if target is None or not keys:
        return target
    root = copy.deepcopy(target)
    node = root
    for key in keys[:-1]:  # walk to the parent of the leaf
        if isinstance(node, dict):
            if key not in node:
                if not create:
                    return root
                node[key] = {}
            node = node[key]
        elif isinstance(node, list):
            idx = _list_index(node, key)
            if idx is None or not 0 <= idx < len(node):
                return root
            node = node[idx]
        else:
            return root
    leaf = keys[-1]
    if isinstance(node, dict):
        if insert and leaf in node:
            return root  # jsonb_insert leaves an existing key untouched
        if leaf not in node and not create and not insert:
            return root
        node[leaf] = value
    elif isinstance(node, list):
        idx = _list_index(node, leaf)
        if idx is None:
            return root
        if insert:
            pos = idx + 1 if insert_after else idx
            node.insert(max(0, min(pos, len(node))), value)
        elif 0 <= idx < len(node):
            node[idx] = value
    return root


def _list_index(arr: list, key: str) -> int | None:
    """A jsonb array subscript: a signed int (negative counts from the end)."""
    try:
        idx = int(key)
    except (ValueError, TypeError):
        return None
    return idx + len(arr) if idx < 0 else idx


def _jsonb_strip_nulls(value: Any) -> Any:
    """Recursively drop object members whose value is JSON null (arrays keep their
    null elements, matching Postgres ``jsonb_strip_nulls``)."""
    if isinstance(value, dict):
        return {k: _jsonb_strip_nulls(v) for k, v in value.items() if v is not None}
    if isinstance(value, list):
        return [_jsonb_strip_nulls(v) for v in value]
    return value


_NOT_RANGE = object()
_NOT_FTS = object()
_NOT_NET = object()
_NOT_BIT = object()
_NOT_GEO = object()
_NOT_HSTORE = object()
_NOT_ARRAY = object()
_NOT_JSONB = object()


def _eval_hstore_op(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``@>`` (contains) / ``<@`` (contained by) on hstore operands. Returns
    ``_NOT_HSTORE`` when neither operand is an hstore so the caller can fall back to
    the range / geo / jsonb handling of the same operator."""
    from secantus.sql import hstore as _hstore

    left = evaluate(node.this, scope, ctx)
    right = evaluate(node.expression, scope, ctx)
    if not (_hstore.is_hstore(left) or _hstore.is_hstore(right)):
        return _NOT_HSTORE
    if left is None or right is None:
        return None
    # The non-hstore side of @>/<@ is a text literal hstore (``'a=>1'``) — parse it.
    left = _hstore.parse(left) if isinstance(left, str) else left
    right = _hstore.parse(right) if isinstance(right, str) else right
    if isinstance(node, exp.ArrayContainsAll):  # a @> b
        return _hstore.contains(left, right)
    return _hstore.contained_by(left, right)  # a <@ b


def _eval_hstore_exists(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``?`` (key exists) / ``?&`` (all keys) / ``?|`` (any keys) on an hstore.
    Returns ``_NOT_HSTORE`` when the left operand is not an hstore (so jsonb's own
    key-exists handling applies)."""
    from secantus.sql import hstore as _hstore

    left = evaluate(node.this, scope, ctx)
    if not _hstore.is_hstore(left):
        return _NOT_HSTORE
    right = evaluate(node.expression, scope, ctx)
    if right is None:
        return None
    if isinstance(node, exp.JSONBContains):  # ? single key
        return _hstore.exists(left, right)
    keys = right if isinstance(right, (list, tuple)) else [right]
    if isinstance(node, exp.JSONBContainsAllTopKeys):  # ?&
        return _hstore.exists_all(left, keys)
    return _hstore.exists_any(left, keys)  # ?|


def _eval_geo_op(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``@>`` (contains) / ``<@`` (contained by) / ``&&`` (overlaps) on geometric
    operands. Returns ``_NOT_GEO`` when neither operand looks like a geometry so the
    caller can fall back to the jsonb / array handling of the same operator."""
    from secantus.sql import pggeo as _pggeo

    left = evaluate(node.this, scope, ctx)
    right = evaluate(node.expression, scope, ctx)
    if not (_pggeo.is_geo_text(left) or _pggeo.is_geo_text(right)):
        return _NOT_GEO
    if left is None or right is None:
        return None
    if isinstance(node, exp.ArrayContainsAll):  # a @> b
        return _pggeo.contains(left, right)
    if isinstance(node, exp.ArrayContainedBy):  # a <@ b
        return _pggeo.contains(right, left)
    return _pggeo.overlaps(left, right)  # a && b


def _eval_bitwise(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """Bitwise operators overloaded across bit strings and integers: ``&`` / ``|`` /
    ``#`` (xor) / ``~`` (not) / ``<<`` / ``>>``. Returns ``_NOT_BIT`` when the (net
    check having already run) operands are neither bit strings nor integers, so a
    ``<<`` / ``>>`` net value can still be handled upstream. NULL operands -> None."""
    from secantus.sql import bitstr as _bitstr

    left = evaluate(node.this, scope, ctx)
    if isinstance(node, exp.BitwiseNot):  # unary ~
        if left is None:
            return None
        if _bitstr.is_bit_value(left):
            return _bitstr.bnot(left)
        if isinstance(left, int) and not isinstance(left, bool):
            return ~left
        return _NOT_BIT
    right = evaluate(node.expression, scope, ctx)
    left_bit, right_bit = _bitstr.is_bit_value(left), _bitstr.is_bit_value(right)
    if left_bit or right_bit:
        if left is None or right is None:
            return None
        a, b = str(left), str(right)
        if isinstance(node, exp.BitwiseAnd):
            return _bitstr.band(a, b)
        if isinstance(node, exp.BitwiseOr):
            return _bitstr.bor(a, b)
        if isinstance(node, exp.BitwiseXor):
            return _bitstr.bxor(a, b)
        if isinstance(node, exp.BitwiseLeftShift):
            return _bitstr.shift_left(a, int(right))
        if isinstance(node, exp.BitwiseRightShift):
            return _bitstr.shift_right(a, int(right))
        return _NOT_BIT
    if isinstance(left, bool) or isinstance(right, bool):
        return _NOT_BIT
    if isinstance(left, int) and isinstance(right, int):
        if left is None or right is None:
            return None
        if isinstance(node, exp.BitwiseAnd):
            return left & right
        if isinstance(node, exp.BitwiseOr):
            return left | right
        if isinstance(node, exp.BitwiseXor):
            return left ^ right
        if isinstance(node, exp.BitwiseLeftShift):
            return left << right
        if isinstance(node, exp.BitwiseRightShift):
            return left >> right
    return _NOT_BIT


def _is_net_value(v: Any) -> bool:
    """Does ``v`` look like a stored ``inet`` / ``cidr`` value (an ``addr/masklen``
    string)?"""
    if not isinstance(v, str) or "/" not in v:
        return False
    import ipaddress

    try:
        ipaddress.ip_interface(v)
    except ValueError:
        return False
    return True


def _eval_net_op(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``<<`` (contained by) / ``>>`` (contains) / ``&&`` (overlaps) on network
    operands. Returns ``_NOT_NET`` when neither operand is a network value (so the
    caller falls back to the range / bit-shift path)."""
    left = evaluate(node.this, scope, ctx)
    right = evaluate(node.expression, scope, ctx)
    if not (_is_net_value(left) or _is_net_value(right)):
        return _NOT_NET
    if left is None or right is None:
        return None
    from secantus.sql import net as _net

    if isinstance(node, exp.BitwiseLeftShift):  # a << b : a contained within b
        return _net.contains(right, left)
    if isinstance(node, exp.BitwiseRightShift):  # a >> b : a contains b
        return _net.contains(left, right)
    return _net.overlaps(left, right)  # a && b


def _eval_fts_match(left: Any, right: Any) -> Any:
    """``@@`` on full-text operands: ``tsvector @@ tsquery`` (either order). Returns
    ``_NOT_FTS`` when neither operand is a tsvector / tsquery so the caller can fall
    back to the jsonb ``@@`` path predicate. NULL on either side yields None."""
    from secantus.sql import fts as _fts

    left_v, left_q = _fts.is_tsvector(left), _fts.is_tsquery(left)
    right_v, right_q = _fts.is_tsvector(right), _fts.is_tsquery(right)
    if not (left_v or left_q or right_v or right_q):
        return _NOT_FTS
    if left is None or right is None:
        return None
    if left_v and right_q:
        return _fts.matches(left, right)
    if left_q and right_v:
        return _fts.matches(right, left)
    return None


def _eval_range_op(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``@>`` / ``<@`` / ``&&`` on range operands. Returns ``_NOT_RANGE`` when
    neither operand is a range subdocument (so the caller falls back to the jsonb /
    array handling of the same operator)."""
    from secantus.sql import ranges as _ranges

    left = evaluate(node.this, scope, ctx)
    right = evaluate(node.expression, scope, ctx)
    left_r = _ranges._is_range(left) or _ranges.is_multirange(left)
    right_r = _ranges._is_range(right) or _ranges.is_multirange(right)
    if not (left_r or right_r):
        return _NOT_RANGE
    if left is None or right is None:
        return None
    if isinstance(node, exp.ArrayOverlaps):  # &&
        return _ranges.overlaps_any(left, right)
    if isinstance(node, exp.ArrayContainedBy):  # <@ : left contained by right
        return _ranges.contains_any(right, left)
    # ArrayContainsAll -> @> : left contains right (element / range / multirange)
    return _ranges.contains_any(left, right)


def _array_membership(needle: Any, haystack: list) -> bool:
    """Postgres array element equality — a plain ``in`` test, but tolerant of the
    ``value in [..]`` raising on unhashable / mismatched element types."""
    for item in haystack:
        try:
            if item == needle:
                return True
        except Exception:  # noqa: BLE001 — heterogeneous element types compare unequal
            continue
    return False


def _array_dim_lengths(v: Any) -> list[int]:
    """The per-dimension lengths of a (rectangular) Postgres array — ``[2, 3]`` for
    a 2×3 array — walking the first element of each level. An empty array has no
    dimensions (``[]``), matching Postgres' ``array_ndims('{}') IS NULL``."""
    dims: list[int] = []
    cur = v
    while isinstance(cur, (list, tuple)) and len(cur) > 0:
        dims.append(len(cur))
        cur = cur[0]
    return dims


def _eval_array_op(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``@>`` (contains) / ``<@`` (contained by) / ``&&`` (overlaps) on Postgres
    *array* operands (both sides are lists). Returns ``_NOT_ARRAY`` when either
    operand isn't a list, so the caller falls back to the jsonb handling of the
    same operator token."""
    left = evaluate(node.this, scope, ctx)
    right = evaluate(node.expression, scope, ctx)
    if not (isinstance(left, (list, tuple)) and isinstance(right, (list, tuple))):
        return _NOT_ARRAY
    left, right = list(left), list(right)
    if isinstance(node, exp.ArrayOverlaps):  # && : share at least one element
        return any(_array_membership(x, right) for x in left)
    if isinstance(node, exp.ArrayContainedBy):  # <@ : every left element is in right
        return all(_array_membership(x, right) for x in left)
    # ArrayContainsAll -> @> : every right element is in left
    return all(_array_membership(x, left) for x in right)


def _jsonb_containment(a: Any, b: Any) -> bool:
    """Postgres ``a @> b`` on jsonb: does ``a`` contain ``b``? Objects match
    key-by-key (recursively); arrays require every element of ``b`` to be contained
    by some element of ``a``; a scalar ``b`` is contained by an array ``a`` when it
    is one of its elements; scalars match by equality. Mismatched container kinds
    (object vs array) don't contain each other."""
    if isinstance(a, dict) and isinstance(b, dict):
        return all(k in a and _jsonb_containment(a[k], b[k]) for k in b)
    if isinstance(a, list) and isinstance(b, list):
        return all(any(_jsonb_containment(ae, be) for ae in a) for be in b)
    if isinstance(a, list):  # scalar / object b contained in array a
        return any(_jsonb_containment(ae, b) for ae in a)
    if isinstance(b, (dict, list)):  # non-container a can't contain a container b
        return False
    return a == b


def _eval_jsonb_op(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``@>`` (contains) / ``<@`` (contained by) on jsonb operands (at least one is
    an object / array). Returns ``_NOT_JSONB`` when neither side is a jsonb
    container, so the caller can surface an unsupported-operator error. jsonb has no
    ``&&``, so overlap is never a jsonb op."""
    if isinstance(node, exp.ArrayOverlaps):
        return _NOT_JSONB
    # A jsonb cast of a literal (``'{...}'::jsonb``) evaluates to the raw JSON
    # text; decode it so both a stored column (already a dict/list) and a literal
    # cast compare as structured values.
    left = _coerce_jsonb(evaluate(node.this, scope, ctx))
    right = _coerce_jsonb(evaluate(node.expression, scope, ctx))
    if not (isinstance(left, (dict, list)) or isinstance(right, (dict, list))):
        return _NOT_JSONB
    if left is None or right is None:
        return None
    if isinstance(node, exp.ArrayContainedBy):  # a <@ b : b contains a
        return _jsonb_containment(right, left)
    return _jsonb_containment(left, right)  # a @> b : a contains b


def _coerce_jsonb(v: Any) -> Any:
    """Decode a JSON-text operand to a Python structure; leave non-strings as is."""
    if isinstance(v, str):
        try:
            return json.loads(v)
        except (ValueError, TypeError):
            return v
    return v


def _eval_jsonb_path_op(
    data_node: exp.Expression | None,
    path_node: exp.Expression | None,
    kind: str,
    scope: Scope,
    ctx: ScalarContext,
) -> Any:
    """Shared ``@?`` (exists) / ``@@`` (match) jsonpath operator evaluation."""
    from secantus.sql import jsonpath as _jsonpath

    doc = evaluate(data_node, scope, ctx) if data_node is not None else None
    path = evaluate(path_node, scope, ctx) if path_node is not None else None
    if doc is None or path is None:
        return None
    try:
        if kind == "exists":
            return _jsonpath.exists(doc, _as_text(path))
        return _jsonpath.match(doc, _as_text(path))
    except _jsonpath.JsonPathError as e:
        raise errors.feature_not_supported(f"unsupported jsonpath: {e}") from e


def _eval_jsonb_delete_path(node: exp.Expression, scope: Scope, ctx: ScalarContext) -> Any:
    """``jsonb #- text[]`` — return a copy of the jsonb with the element at the path
    removed."""
    import copy

    target = evaluate(node.this, scope, ctx)
    if target is None:
        return None
    keys = _pg_text_path(evaluate(node.expression, scope, ctx))
    if not keys:
        return target
    root = copy.deepcopy(target)
    node_ref = root
    for key in keys[:-1]:
        if isinstance(node_ref, dict) and key in node_ref:
            node_ref = node_ref[key]
        elif (
            isinstance(node_ref, list)
            and (i := _list_index(node_ref, key)) is not None
            and 0 <= i < len(node_ref)
        ):
            node_ref = node_ref[i]
        else:
            return root
    leaf = keys[-1]
    if isinstance(node_ref, dict):
        node_ref.pop(leaf, None)
    elif isinstance(node_ref, list):
        i = _list_index(node_ref, leaf)
        if i is not None and 0 <= i < len(node_ref):
            del node_ref[i]
    return root


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


def _eval_regexp(node: exp.Expression, outer: Scope, ctx: ScalarContext) -> Any:
    """POSIX regex-match operators ``~`` (``RegexpLike``) / ``~*`` (``RegexpILike``).
    The pattern is a raw regex matched *unanchored* (``re.search``), unlike LIKE.
    ``!~`` / ``!~*`` arrive as ``Not(...)`` and are negated by the caller."""
    import re

    val = evaluate(node.this, outer, ctx)
    pattern = evaluate(node.expression, outer, ctx)
    if val is None or pattern is None:
        return None
    flags = re.IGNORECASE if isinstance(node, exp.RegexpILike) else 0
    return re.search(_as_text(pattern), _as_text(val), flags) is not None


def _sub_scope(inner_alias: str, tdef: Any, row: dict[str, Any], outer: Scope) -> Scope:
    def resolve(node: exp.Column) -> Any:
        alias = node.table or None
        name = node.name
        if alias == inner_alias or (alias is None and tdef.column(name) is not None):
            return get_path(row, tdef.field_for(name))
        return outer(node)  # correlated reference to the enclosing query

    return resolve
