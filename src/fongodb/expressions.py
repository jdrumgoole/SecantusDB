from __future__ import annotations

from collections.abc import Mapping
from typing import Any

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
}
