from __future__ import annotations

import datetime as _dt
from collections.abc import Mapping
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Any

from bson import Decimal128

from secantus.paths import get_path


class ExpressionError(Exception):
    pass


@dataclass
class _Ctx:
    doc: Mapping[str, Any]
    vars: dict[str, Any] = field(default_factory=dict)

    def with_var(self, name: str, value: Any) -> _Ctx:
        return _Ctx(doc=self.doc, vars={**self.vars, name: value})


def evaluate(expr: Any, doc: Mapping[str, Any], vars: dict[str, Any] | None = None) -> Any:
    return _eval(expr, _Ctx(doc=doc, vars=dict(vars) if vars else {}))


def _eval(expr: Any, ctx: _Ctx) -> Any:
    if isinstance(expr, str):
        if expr.startswith("$$"):
            return _resolve_var(expr[2:], ctx)
        if expr.startswith("$"):
            return get_path(dict(ctx.doc), expr[1:], default=None)
        return expr
    if isinstance(expr, list):
        return [_eval(e, ctx) for e in expr]
    if isinstance(expr, Mapping):
        if len(expr) == 1:
            (key,) = expr.keys()
            if key.startswith("$"):
                return _apply_op(key, expr[key], ctx)
        return {k: _eval(v, ctx) for k, v in expr.items()}
    return expr


def _resolve_var(name: str, ctx: _Ctx) -> Any:
    if name in ctx.vars:
        return ctx.vars[name]
    if name in ("ROOT", "CURRENT"):
        return ctx.doc
    raise ExpressionError(f"system variable $${name} is not defined")


def _apply_op(op: str, arg: Any, ctx: _Ctx) -> Any:
    if op == "$literal":
        return arg
    handler = _OPS.get(op)
    if handler is None:
        raise ExpressionError(f"unsupported aggregation expression operator: {op}")
    return handler(arg, ctx)


def _eval_args(arg: Any, ctx: _Ctx) -> list[Any]:
    if isinstance(arg, list):
        return [_eval(a, ctx) for a in arg]
    return [_eval(arg, ctx)]


def _bool(value: Any) -> bool:
    if value is None:
        return False
    if isinstance(value, (bool, int, float)):
        return bool(value)
    return True


def _op_concat(arg: Any, ctx: _Ctx) -> str:
    parts = _eval_args(arg, ctx)
    return "".join("" if p is None else str(p) for p in parts)


def _op_add(arg: Any, ctx: _Ctx) -> Any:
    values = _eval_args(arg, ctx)
    if any(v is None for v in values):
        return None
    total = values[0]
    for v in values[1:]:
        total = total + v
    return total


def _op_subtract(arg: Any, ctx: _Ctx) -> Any:
    a, b = _eval_args(arg, ctx)
    if a is None or b is None:
        return None
    return a - b


def _op_multiply(arg: Any, ctx: _Ctx) -> Any:
    values = _eval_args(arg, ctx)
    if any(v is None for v in values):
        return None
    result = 1
    for v in values:
        result = result * v
    return result


def _op_divide(arg: Any, ctx: _Ctx) -> Any:
    a, b = _eval_args(arg, ctx)
    if a is None or b is None or b == 0:
        return None
    return a / b


def _op_mod(arg: Any, ctx: _Ctx) -> Any:
    a, b = _eval_args(arg, ctx)
    if a is None or b is None or b == 0:
        return None
    return a % b


def _op_and(arg: Any, ctx: _Ctx) -> bool:
    return all(_bool(_eval(a, ctx)) for a in arg)


def _op_or(arg: Any, ctx: _Ctx) -> bool:
    return any(_bool(_eval(a, ctx)) for a in arg)


def _op_not(arg: Any, ctx: _Ctx) -> bool:
    inner = arg[0] if isinstance(arg, list) else arg
    return not _bool(_eval(inner, ctx))


def _cmp_pair(arg: Any, ctx: _Ctx) -> tuple[Any, Any]:
    a, b = _eval_args(arg, ctx)
    return a, b


def _op_eq(arg: Any, ctx: _Ctx) -> bool:
    a, b = _cmp_pair(arg, ctx)
    return a == b


def _op_ne(arg: Any, ctx: _Ctx) -> bool:
    a, b = _cmp_pair(arg, ctx)
    return a != b


def _op_gt(arg: Any, ctx: _Ctx) -> bool:
    a, b = _cmp_pair(arg, ctx)
    try:
        return bool(a > b)
    except TypeError:
        return False


def _op_gte(arg: Any, ctx: _Ctx) -> bool:
    a, b = _cmp_pair(arg, ctx)
    try:
        return bool(a >= b)
    except TypeError:
        return False


def _op_lt(arg: Any, ctx: _Ctx) -> bool:
    a, b = _cmp_pair(arg, ctx)
    try:
        return bool(a < b)
    except TypeError:
        return False


def _op_lte(arg: Any, ctx: _Ctx) -> bool:
    a, b = _cmp_pair(arg, ctx)
    try:
        return bool(a <= b)
    except TypeError:
        return False


def _op_cond(arg: Any, ctx: _Ctx) -> Any:
    if isinstance(arg, Mapping):
        condition = _eval(arg["if"], ctx)
        return _eval(arg["then"] if _bool(condition) else arg["else"], ctx)
    if isinstance(arg, list) and len(arg) == 3:
        return _eval(arg[1] if _bool(_eval(arg[0], ctx)) else arg[2], ctx)
    raise ExpressionError("$cond requires {if, then, else} or [cond, then, else]")


def _op_if_null(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) < 2:
        raise ExpressionError("$ifNull requires an array of at least two expressions")
    *checks, fallback = arg
    for check in checks:
        v = _eval(check, ctx)
        if v is not None:
            return v
    return _eval(fallback, ctx)


def _op_size(arg: Any, ctx: _Ctx) -> int:
    value = _eval(arg, ctx)
    if not isinstance(value, list):
        raise ExpressionError("$size requires an array")
    return len(value)


def _op_to_string(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    if value is None:
        return None
    return str(value)


def _op_to_lower(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    return value.lower() if isinstance(value, str) else value


def _op_to_upper(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    return value.upper() if isinstance(value, str) else value


def _op_split(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) != 2:
        raise ExpressionError("$split requires [string, separator]")
    s = _eval(arg[0], ctx)
    sep = _eval(arg[1], ctx)
    if s is None or sep is None:
        return None
    if not isinstance(s, str) or not isinstance(sep, str):
        raise ExpressionError("$split requires string operands")
    return s.split(sep)


def _op_trim(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$trim requires {input, chars?}")
    s = _eval(arg.get("input"), ctx)
    if s is None:
        return None
    if not isinstance(s, str):
        raise ExpressionError("$trim input must be a string")
    chars = _eval(arg.get("chars"), ctx) if "chars" in arg else None
    return s.strip(chars) if isinstance(chars, str) else s.strip()


def _op_ltrim(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$ltrim requires {input, chars?}")
    s = _eval(arg.get("input"), ctx)
    if s is None:
        return None
    if not isinstance(s, str):
        raise ExpressionError("$ltrim input must be a string")
    chars = _eval(arg.get("chars"), ctx) if "chars" in arg else None
    return s.lstrip(chars) if isinstance(chars, str) else s.lstrip()


def _op_rtrim(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$rtrim requires {input, chars?}")
    s = _eval(arg.get("input"), ctx)
    if s is None:
        return None
    if not isinstance(s, str):
        raise ExpressionError("$rtrim input must be a string")
    chars = _eval(arg.get("chars"), ctx) if "chars" in arg else None
    return s.rstrip(chars) if isinstance(chars, str) else s.rstrip()


def _op_substr_cp(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) != 3:
        raise ExpressionError("$substrCP requires [string, start, length]")
    s = _eval(arg[0], ctx)
    start = _eval(arg[1], ctx)
    length = _eval(arg[2], ctx)
    if s is None:
        return ""
    if not isinstance(s, str) or not isinstance(start, int) or not isinstance(length, int):
        raise ExpressionError("$substrCP requires string + ints")
    if length < 0:
        return s[start:]
    return s[start : start + length]


def _op_str_len_cp(arg: Any, ctx: _Ctx) -> Any:
    s = _eval(arg, ctx)
    if not isinstance(s, str):
        raise ExpressionError("$strLenCP requires a string")
    return len(s)


def _op_index_of_cp(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or not 2 <= len(arg) <= 4:
        raise ExpressionError("$indexOfCP requires [string, search, start?, end?]")
    s = _eval(arg[0], ctx)
    needle = _eval(arg[1], ctx)
    if s is None:
        return None
    if not isinstance(s, str) or not isinstance(needle, str):
        raise ExpressionError("$indexOfCP requires string operands")
    start = _eval(arg[2], ctx) if len(arg) >= 3 else 0
    end = _eval(arg[3], ctx) if len(arg) >= 4 else len(s)
    if not isinstance(start, int) or not isinstance(end, int):
        return -1
    return s.find(needle, start, end)


def _ensure_datetime(value: Any) -> _dt.datetime | None:
    if isinstance(value, _dt.datetime):
        return value
    return None


def _op_year(arg: Any, ctx: _Ctx) -> Any:
    d = _ensure_datetime(_eval(arg, ctx))
    return d.year if d is not None else None


def _op_month(arg: Any, ctx: _Ctx) -> Any:
    d = _ensure_datetime(_eval(arg, ctx))
    return d.month if d is not None else None


def _op_day_of_month(arg: Any, ctx: _Ctx) -> Any:
    d = _ensure_datetime(_eval(arg, ctx))
    return d.day if d is not None else None


def _op_day_of_week(arg: Any, ctx: _Ctx) -> Any:
    d = _ensure_datetime(_eval(arg, ctx))
    return (d.isoweekday() % 7) + 1 if d is not None else None


def _op_hour(arg: Any, ctx: _Ctx) -> Any:
    d = _ensure_datetime(_eval(arg, ctx))
    return d.hour if d is not None else None


def _op_minute(arg: Any, ctx: _Ctx) -> Any:
    d = _ensure_datetime(_eval(arg, ctx))
    return d.minute if d is not None else None


def _op_second(arg: Any, ctx: _Ctx) -> Any:
    d = _ensure_datetime(_eval(arg, ctx))
    return d.second if d is not None else None


def _op_date_to_string(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateToString requires {date, format}")
    d = _ensure_datetime(_eval(arg["date"], ctx))
    if d is None:
        return None
    fmt = arg.get("format", "%Y-%m-%dT%H:%M:%S.%LZ")
    if not isinstance(fmt, str):
        raise ExpressionError("$dateToString format must be a string")
    out = fmt
    if "%L" in out:
        out = out.replace("%L", f"{d.microsecond // 1000:03d}")
    return d.strftime(out)


def _op_array_elem_at(arg: Any, ctx: _Ctx) -> Any:
    arr_expr, idx_expr = arg
    arr = _eval(arr_expr, ctx)
    idx = _eval(idx_expr, ctx)
    if not isinstance(arr, list) or not isinstance(idx, int):
        return None
    if -len(arr) <= idx < len(arr):
        return arr[idx]
    return None


def _op_first(arg: Any, ctx: _Ctx) -> Any:
    arr = _eval(arg, ctx)
    return arr[0] if isinstance(arr, list) and arr else None


def _op_last(arg: Any, ctx: _Ctx) -> Any:
    arr = _eval(arg, ctx)
    return arr[-1] if isinstance(arr, list) and arr else None


def _op_slice(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) not in (2, 3):
        raise ExpressionError("$slice requires [array, n] or [array, position, n]")
    arr = _eval(arg[0], ctx)
    if not isinstance(arr, list):
        return None
    if len(arg) == 2:
        n = _eval(arg[1], ctx)
        if not isinstance(n, int):
            return None
        return arr[:n] if n >= 0 else arr[n:]
    position = _eval(arg[1], ctx)
    n = _eval(arg[2], ctx)
    if not isinstance(position, int) or not isinstance(n, int):
        return None
    return arr[position : position + n]


def _op_concat_arrays(arg: Any, ctx: _Ctx) -> Any:
    parts = [_eval(a, ctx) for a in arg]
    out: list[Any] = []
    for p in parts:
        if not isinstance(p, list):
            return None
        out.extend(p)
    return out


def _op_reverse_array(arg: Any, ctx: _Ctx) -> Any:
    arr = _eval(arg, ctx)
    return list(reversed(arr)) if isinstance(arr, list) else None


def _op_in(arg: Any, ctx: _Ctx) -> bool:
    needle, haystack = _eval(arg[0], ctx), _eval(arg[1], ctx)
    if not isinstance(haystack, list):
        return False
    return needle in haystack


def _op_to_int(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    if value is None:
        return None
    if isinstance(value, bool):
        return 1 if value else 0
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    if isinstance(value, Decimal128):
        return int(value.to_decimal())
    if isinstance(value, str):
        try:
            return int(value)
        except ValueError as exc:
            raise ExpressionError(f"$toInt cannot convert {value!r}") from exc
    raise ExpressionError(f"$toInt cannot convert {type(value).__name__}")


def _op_to_double(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    if value is None:
        return None
    if isinstance(value, bool):
        return 1.0 if value else 0.0
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, Decimal128):
        return float(value.to_decimal())
    if isinstance(value, str):
        try:
            return float(value)
        except ValueError as exc:
            raise ExpressionError(f"$toDouble cannot convert {value!r}") from exc
    raise ExpressionError(f"$toDouble cannot convert {type(value).__name__}")


def _op_to_bool(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    if value is None:
        return None
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return value != 0
    if isinstance(value, Decimal128):
        return value.to_decimal() != Decimal(0)
    if isinstance(value, str):
        return len(value) > 0
    return True


def _op_to_decimal(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    if value is None:
        return None
    if isinstance(value, Decimal128):
        return value
    if isinstance(value, (int, float)):
        return Decimal128(Decimal(repr(value) if isinstance(value, float) else value))
    if isinstance(value, str):
        try:
            return Decimal128(value)
        except (InvalidOperation, ValueError) as exc:
            raise ExpressionError(f"$toDecimal cannot convert {value!r}") from exc
    raise ExpressionError(f"$toDecimal cannot convert {type(value).__name__}")


def _op_filter(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$filter requires a document spec")
    arr = _eval(arg.get("input"), ctx)
    if not isinstance(arr, list):
        return None
    var_name = arg.get("as", "this")
    cond_expr = arg.get("cond")
    raw_limit = arg.get("limit")
    limit = _eval(raw_limit, ctx) if raw_limit is not None else None
    out: list[Any] = []
    for elem in arr:
        if _bool(_eval(cond_expr, ctx.with_var(var_name, elem))):
            out.append(elem)
            if limit is not None and isinstance(limit, int) and len(out) >= limit:
                break
    return out


def _op_map(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$map requires a document spec")
    arr = _eval(arg.get("input"), ctx)
    if not isinstance(arr, list):
        return None
    var_name = arg.get("as", "this")
    in_expr = arg.get("in")
    return [_eval(in_expr, ctx.with_var(var_name, elem)) for elem in arr]


def _op_reduce(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$reduce requires a document spec")
    arr = _eval(arg.get("input"), ctx)
    if not isinstance(arr, list):
        return None
    accumulator = _eval(arg.get("initialValue"), ctx)
    in_expr = arg.get("in")
    for elem in arr:
        scoped = ctx.with_var("value", accumulator).with_var("this", elem)
        accumulator = _eval(in_expr, scoped)
    return accumulator


_OPS = {
    "$concat": _op_concat,
    "$add": _op_add,
    "$subtract": _op_subtract,
    "$multiply": _op_multiply,
    "$divide": _op_divide,
    "$mod": _op_mod,
    "$and": _op_and,
    "$or": _op_or,
    "$not": _op_not,
    "$eq": _op_eq,
    "$ne": _op_ne,
    "$gt": _op_gt,
    "$gte": _op_gte,
    "$lt": _op_lt,
    "$lte": _op_lte,
    "$cond": _op_cond,
    "$ifNull": _op_if_null,
    "$size": _op_size,
    "$toString": _op_to_string,
    "$toLower": _op_to_lower,
    "$toUpper": _op_to_upper,
    "$split": _op_split,
    "$trim": _op_trim,
    "$ltrim": _op_ltrim,
    "$rtrim": _op_rtrim,
    "$substr": _op_substr_cp,
    "$substrCP": _op_substr_cp,
    "$strLenCP": _op_str_len_cp,
    "$indexOfCP": _op_index_of_cp,
    "$year": _op_year,
    "$month": _op_month,
    "$dayOfMonth": _op_day_of_month,
    "$dayOfWeek": _op_day_of_week,
    "$hour": _op_hour,
    "$minute": _op_minute,
    "$second": _op_second,
    "$dateToString": _op_date_to_string,
    "$arrayElemAt": _op_array_elem_at,
    "$first": _op_first,
    "$last": _op_last,
    "$slice": _op_slice,
    "$concatArrays": _op_concat_arrays,
    "$reverseArray": _op_reverse_array,
    "$in": _op_in,
    "$toInt": _op_to_int,
    "$toDouble": _op_to_double,
    "$toBool": _op_to_bool,
    "$toDecimal": _op_to_decimal,
    "$filter": _op_filter,
    "$map": _op_map,
    "$reduce": _op_reduce,
}
