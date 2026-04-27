from __future__ import annotations

import datetime as _dt
from collections.abc import Mapping
from decimal import Decimal, InvalidOperation
from typing import Any

from bson import Decimal128

from fongodb.paths import get_path


class ExpressionError(Exception):
    pass


def evaluate(expr: Any, doc: Mapping[str, Any]) -> Any:
    if isinstance(expr, str):
        if expr.startswith("$$"):
            raise ExpressionError(f"system variable {expr} not supported")
        if expr.startswith("$"):
            return get_path(doc, expr[1:], default=None)
        return expr
    if isinstance(expr, list):
        return [evaluate(e, doc) for e in expr]
    if isinstance(expr, Mapping):
        if len(expr) == 1:
            (key,) = expr.keys()
            if key.startswith("$"):
                return _apply_op(key, expr[key], doc)
        return {k: evaluate(v, doc) for k, v in expr.items()}
    return expr


def _apply_op(op: str, arg: Any, doc: Mapping[str, Any]) -> Any:
    if op == "$literal":
        return arg
    handler = _OPS.get(op)
    if handler is None:
        raise ExpressionError(f"unsupported aggregation expression operator: {op}")
    return handler(arg, doc)


def _eval_args(arg: Any, doc: Mapping[str, Any]) -> list[Any]:
    if isinstance(arg, list):
        return [evaluate(a, doc) for a in arg]
    return [evaluate(arg, doc)]


def _op_concat(arg: Any, doc: Mapping[str, Any]) -> str:
    parts = _eval_args(arg, doc)
    return "".join("" if p is None else str(p) for p in parts)


def _op_add(arg: Any, doc: Mapping[str, Any]) -> Any:
    values = _eval_args(arg, doc)
    if any(v is None for v in values):
        return None
    total = values[0]
    for v in values[1:]:
        total = total + v
    return total


def _op_subtract(arg: Any, doc: Mapping[str, Any]) -> Any:
    a, b = _eval_args(arg, doc)
    if a is None or b is None:
        return None
    return a - b


def _op_multiply(arg: Any, doc: Mapping[str, Any]) -> Any:
    values = _eval_args(arg, doc)
    if any(v is None for v in values):
        return None
    result = 1
    for v in values:
        result = result * v
    return result


def _op_divide(arg: Any, doc: Mapping[str, Any]) -> Any:
    a, b = _eval_args(arg, doc)
    if a is None or b is None or b == 0:
        return None
    return a / b


def _op_mod(arg: Any, doc: Mapping[str, Any]) -> Any:
    a, b = _eval_args(arg, doc)
    if a is None or b is None or b == 0:
        return None
    return a % b


def _bool(value: Any) -> bool:
    if value is None:
        return False
    if isinstance(value, (bool, int, float)):
        return bool(value)
    return True


def _op_and(arg: Any, doc: Mapping[str, Any]) -> bool:
    return all(_bool(evaluate(a, doc)) for a in arg)


def _op_or(arg: Any, doc: Mapping[str, Any]) -> bool:
    return any(_bool(evaluate(a, doc)) for a in arg)


def _op_not(arg: Any, doc: Mapping[str, Any]) -> bool:
    inner = arg[0] if isinstance(arg, list) else arg
    return not _bool(evaluate(inner, doc))


def _cmp_pair(arg: Any, doc: Mapping[str, Any]) -> tuple[Any, Any]:
    a, b = _eval_args(arg, doc)
    return a, b


def _op_eq(arg: Any, doc: Mapping[str, Any]) -> bool:
    a, b = _cmp_pair(arg, doc)
    return a == b


def _op_ne(arg: Any, doc: Mapping[str, Any]) -> bool:
    a, b = _cmp_pair(arg, doc)
    return a != b


def _op_gt(arg: Any, doc: Mapping[str, Any]) -> bool:
    a, b = _cmp_pair(arg, doc)
    try:
        return bool(a > b)
    except TypeError:
        return False


def _op_gte(arg: Any, doc: Mapping[str, Any]) -> bool:
    a, b = _cmp_pair(arg, doc)
    try:
        return bool(a >= b)
    except TypeError:
        return False


def _op_lt(arg: Any, doc: Mapping[str, Any]) -> bool:
    a, b = _cmp_pair(arg, doc)
    try:
        return bool(a < b)
    except TypeError:
        return False


def _op_lte(arg: Any, doc: Mapping[str, Any]) -> bool:
    a, b = _cmp_pair(arg, doc)
    try:
        return bool(a <= b)
    except TypeError:
        return False


def _op_cond(arg: Any, doc: Mapping[str, Any]) -> Any:
    if isinstance(arg, Mapping):
        condition = evaluate(arg["if"], doc)
        return evaluate(arg["then"] if _bool(condition) else arg["else"], doc)
    if isinstance(arg, list) and len(arg) == 3:
        return evaluate(arg[1] if _bool(evaluate(arg[0], doc)) else arg[2], doc)
    raise ExpressionError("$cond requires {if, then, else} or [cond, then, else]")


def _op_if_null(arg: Any, doc: Mapping[str, Any]) -> Any:
    if not isinstance(arg, list) or len(arg) < 2:
        raise ExpressionError("$ifNull requires an array of at least two expressions")
    *checks, fallback = arg
    for check in checks:
        v = evaluate(check, doc)
        if v is not None:
            return v
    return evaluate(fallback, doc)


def _op_size(arg: Any, doc: Mapping[str, Any]) -> int:
    value = evaluate(arg, doc)
    if not isinstance(value, list):
        raise ExpressionError("$size requires an array")
    return len(value)


def _op_to_string(arg: Any, doc: Mapping[str, Any]) -> Any:
    value = evaluate(arg, doc)
    if value is None:
        return None
    return str(value)


def _op_to_lower(arg: Any, doc: Mapping[str, Any]) -> Any:
    value = evaluate(arg, doc)
    return value.lower() if isinstance(value, str) else value


def _op_to_upper(arg: Any, doc: Mapping[str, Any]) -> Any:
    value = evaluate(arg, doc)
    return value.upper() if isinstance(value, str) else value


def _ensure_datetime(value: Any) -> _dt.datetime | None:
    if isinstance(value, _dt.datetime):
        return value
    return None


def _op_year(arg: Any, doc: Mapping[str, Any]) -> Any:
    d = _ensure_datetime(evaluate(arg, doc))
    return d.year if d is not None else None


def _op_month(arg: Any, doc: Mapping[str, Any]) -> Any:
    d = _ensure_datetime(evaluate(arg, doc))
    return d.month if d is not None else None


def _op_day_of_month(arg: Any, doc: Mapping[str, Any]) -> Any:
    d = _ensure_datetime(evaluate(arg, doc))
    return d.day if d is not None else None


def _op_day_of_week(arg: Any, doc: Mapping[str, Any]) -> Any:
    d = _ensure_datetime(evaluate(arg, doc))
    return (d.isoweekday() % 7) + 1 if d is not None else None


def _op_hour(arg: Any, doc: Mapping[str, Any]) -> Any:
    d = _ensure_datetime(evaluate(arg, doc))
    return d.hour if d is not None else None


def _op_minute(arg: Any, doc: Mapping[str, Any]) -> Any:
    d = _ensure_datetime(evaluate(arg, doc))
    return d.minute if d is not None else None


def _op_second(arg: Any, doc: Mapping[str, Any]) -> Any:
    d = _ensure_datetime(evaluate(arg, doc))
    return d.second if d is not None else None


_DATE_FORMAT_MAP = {
    "%Y": "%Y",
    "%m": "%m",
    "%d": "%d",
    "%H": "%H",
    "%M": "%M",
    "%S": "%S",
    "%j": "%j",
    "%w": "%w",
    "%U": "%U",
    "%L": None,
}


def _op_date_to_string(arg: Any, doc: Mapping[str, Any]) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateToString requires {date, format}")
    d = _ensure_datetime(evaluate(arg["date"], doc))
    if d is None:
        return None
    fmt = arg.get("format", "%Y-%m-%dT%H:%M:%S.%LZ")
    if not isinstance(fmt, str):
        raise ExpressionError("$dateToString format must be a string")
    out = fmt
    if "%L" in out:
        out = out.replace("%L", f"{d.microsecond // 1000:03d}")
    return d.strftime(out)


def _op_array_elem_at(arg: Any, doc: Mapping[str, Any]) -> Any:
    arr_expr, idx_expr = arg
    arr = evaluate(arr_expr, doc)
    idx = evaluate(idx_expr, doc)
    if not isinstance(arr, list) or not isinstance(idx, int):
        return None
    if -len(arr) <= idx < len(arr):
        return arr[idx]
    return None


def _op_first(arg: Any, doc: Mapping[str, Any]) -> Any:
    arr = evaluate(arg, doc)
    return arr[0] if isinstance(arr, list) and arr else None


def _op_last(arg: Any, doc: Mapping[str, Any]) -> Any:
    arr = evaluate(arg, doc)
    return arr[-1] if isinstance(arr, list) and arr else None


def _op_slice(arg: Any, doc: Mapping[str, Any]) -> Any:
    if not isinstance(arg, list) or len(arg) not in (2, 3):
        raise ExpressionError("$slice requires [array, n] or [array, position, n]")
    arr = evaluate(arg[0], doc)
    if not isinstance(arr, list):
        return None
    if len(arg) == 2:
        n = evaluate(arg[1], doc)
        if not isinstance(n, int):
            return None
        return arr[:n] if n >= 0 else arr[n:]
    position = evaluate(arg[1], doc)
    n = evaluate(arg[2], doc)
    if not isinstance(position, int) or not isinstance(n, int):
        return None
    return arr[position : position + n]


def _op_concat_arrays(arg: Any, doc: Mapping[str, Any]) -> Any:
    parts = [evaluate(a, doc) for a in arg]
    out: list[Any] = []
    for p in parts:
        if not isinstance(p, list):
            return None
        out.extend(p)
    return out


def _op_reverse_array(arg: Any, doc: Mapping[str, Any]) -> Any:
    arr = evaluate(arg, doc)
    return list(reversed(arr)) if isinstance(arr, list) else None


def _op_in(arg: Any, doc: Mapping[str, Any]) -> bool:
    needle, haystack = evaluate(arg[0], doc), evaluate(arg[1], doc)
    if not isinstance(haystack, list):
        return False
    return needle in haystack


def _op_to_int(arg: Any, doc: Mapping[str, Any]) -> Any:
    value = evaluate(arg, doc)
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


def _op_to_double(arg: Any, doc: Mapping[str, Any]) -> Any:
    value = evaluate(arg, doc)
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


def _op_to_bool(arg: Any, doc: Mapping[str, Any]) -> Any:
    value = evaluate(arg, doc)
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


def _op_to_decimal(arg: Any, doc: Mapping[str, Any]) -> Any:
    value = evaluate(arg, doc)
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
}
