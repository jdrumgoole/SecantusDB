from __future__ import annotations

import datetime as _dt
import zoneinfo
from collections.abc import Mapping
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Any

from bson import Decimal128, Int64

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


_REMOVE_SENTINEL: Any = object()


def _resolve_var(name: str, ctx: _Ctx) -> Any:
    # ``$$var.a.b`` means resolve ``var`` from system / user vars, then
    # walk the dotted path into the result. Real mongod supports this
    # everywhere (e.g. ``$$ROOT.field``, ``$$new.delta``); without it
    # the only way to read a field of a var would be ``$$var`` whole-
    # doc + downstream stage massage, which is awkward for $merge let.
    base, _, rest = name.partition(".")
    if base in ctx.vars:
        value: Any = ctx.vars[base]
    elif base in ("ROOT", "CURRENT"):
        value = ctx.doc
    elif base == "REMOVE":
        # MongoDB 5.0+ ``$$REMOVE`` is a sentinel that, when used as a
        # ``$setField`` / ``$addFields`` / ``$project`` value, deletes
        # the field instead of writing it. ``_op_set_field`` checks for
        # this identity to drop the key.
        return _REMOVE_SENTINEL
    elif base in ("KEEP", "PRUNE", "DESCEND"):
        # ``$redact`` sentinels. The expression evaluator returns the
        # ``"$$NAME"`` string literal so the stage handler can dispatch
        # on equality. Real mongod's ``$redact`` docs show these as the
        # only legal return values from the stage's expression.
        value = f"$${base}"
    else:
        raise ExpressionError(f"system variable $${base} is not defined")
    if not rest:
        return value
    if not isinstance(value, Mapping):
        return None
    return get_path(dict(value), rest, default=None)


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


def _op_abs(arg: Any, ctx: _Ctx) -> Any:
    v = _eval(arg, ctx)
    return abs(v) if v is not None else None


def _op_round(arg: Any, ctx: _Ctx) -> Any:
    if isinstance(arg, list):
        if not arg:
            raise ExpressionError("$round requires [number, place?]")
        n = _eval(arg[0], ctx)
        place = _eval(arg[1], ctx) if len(arg) > 1 else 0
    else:
        n = _eval(arg, ctx)
        place = 0
    if n is None:
        return None
    if not isinstance(place, int):
        place = 0
    return round(n, place)


def _op_floor(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    return math.floor(v) if v is not None else None


def _op_ceil(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    return math.ceil(v) if v is not None else None


def _op_sqrt(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    return math.sqrt(v) if v is not None and v >= 0 else None


def _op_pow(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) != 2:
        raise ExpressionError("$pow requires [base, exponent]")
    base, exponent = _eval(arg[0], ctx), _eval(arg[1], ctx)
    if base is None or exponent is None:
        return None
    return base**exponent


def _op_exp(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    return math.exp(v) if v is not None else None


def _op_ln(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    return math.log(v) if v is not None and v > 0 else None


def _op_log(arg: Any, ctx: _Ctx) -> Any:
    import math

    if not isinstance(arg, list) or len(arg) != 2:
        raise ExpressionError("$log requires [number, base]")
    n, base = _eval(arg[0], ctx), _eval(arg[1], ctx)
    if n is None or base is None or n <= 0 or base <= 0 or base == 1:
        return None
    return math.log(n, base)


def _op_log10(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    return math.log10(v) if v is not None and v > 0 else None


def _op_rand(arg: Any, _ctx: _Ctx) -> float:
    # MongoDB 5.0+: ``{$rand: {}}`` returns a uniform random double in
    # [0, 1). Argument must be an empty document; anything else is a
    # parse error in mongod (we mirror).
    if not (isinstance(arg, Mapping) and not arg):
        raise ExpressionError("$rand expects an empty document")
    import random as _random

    return _random.random()


def _op_trunc(arg: Any, ctx: _Ctx) -> Any:
    import math

    if isinstance(arg, list):
        n = _eval(arg[0], ctx)
        place = _eval(arg[1], ctx) if len(arg) > 1 else 0
    else:
        n = _eval(arg, ctx)
        place = 0
    if n is None:
        return None
    if not isinstance(place, int):
        place = 0
    factor = 10**place
    return math.trunc(n * factor) / factor


def _op_merge_objects(arg: Any, ctx: _Ctx) -> Any:
    items = arg if isinstance(arg, list) else [arg]
    result: dict[str, Any] = {}
    for item in items:
        v = _eval(item, ctx)
        if v is None:
            continue
        if not isinstance(v, Mapping):
            raise ExpressionError("$mergeObjects requires document-valued arguments")
        result.update(v)
    return result


def _op_object_to_array(arg: Any, ctx: _Ctx) -> Any:
    v = _eval(arg, ctx)
    if v is None:
        return None
    if not isinstance(v, Mapping):
        raise ExpressionError("$objectToArray requires a document")
    return [{"k": k, "v": val} for k, val in v.items()]


def _op_set_field(arg: Any, ctx: _Ctx) -> Any:
    """MongoDB 5.0+ ``$setField`` — set/replace a field in a document.

    Accepts ``{field, input, value}`` or its array-form alias. The
    field name is evaluated (so dynamic field names work), but
    typically a constant string. Used by drivers' dots-and-dollars
    tests to write keys that the normal document-builder API would
    refuse.
    """
    if not isinstance(arg, Mapping):
        raise ExpressionError("$setField requires {field, input, value}")
    field_expr = arg.get("field")
    input_expr = arg.get("input")
    value_expr = arg.get("value")
    if field_expr is None or input_expr is None or value_expr is None:
        raise ExpressionError("$setField requires field, input, value")
    field = _eval(field_expr, ctx)
    if not isinstance(field, str):
        raise ExpressionError("$setField field must evaluate to a string")
    input_doc = _eval(input_expr, ctx)
    if input_doc is None:
        return None
    if not isinstance(input_doc, Mapping):
        raise ExpressionError("$setField input must evaluate to a document")
    value = _eval(value_expr, ctx)
    result = dict(input_doc)
    # Sentinel-equivalent for removal: ``$$REMOVE`` (system var) maps
    # to the same MISSING marker we use for "field doesn't exist". If
    # the user supplied ``"$$REMOVE"`` or it resolved to None-via-
    # MISSING, drop the key. Anything else is a normal assignment.
    if value is _REMOVE_SENTINEL:
        result.pop(field, None)
    else:
        result[field] = value
    return result


def _op_get_field(arg: Any, ctx: _Ctx) -> Any:
    """MongoDB 5.0+ ``$getField`` — read a field by name from a document.

    Accepts ``{field, input}`` (full form) or a bare string (shorthand
    for ``{field: <string>, input: $$CURRENT}``). The field name may
    contain dots / dollars without being interpreted as a path —
    that's the whole point of ``$getField`` vs. a bare ``$path``.
    """
    if isinstance(arg, str):
        field, input_expr = arg, "$$CURRENT"
    elif isinstance(arg, Mapping):
        field_expr = arg.get("field")
        input_expr = arg.get("input", "$$CURRENT")
        if field_expr is None:
            raise ExpressionError("$getField requires a field")
        field = _eval(field_expr, ctx)
        if not isinstance(field, str):
            raise ExpressionError("$getField field must evaluate to a string")
    else:
        raise ExpressionError("$getField requires a string or {field, input} document")
    input_doc = _eval(input_expr, ctx)
    if input_doc is None or not isinstance(input_doc, Mapping):
        return None
    return input_doc.get(field)


def _op_switch(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$switch requires {branches, default?}")
    branches = arg.get("branches")
    if not isinstance(branches, list):
        raise ExpressionError("$switch branches must be an array")
    for branch in branches:
        if not isinstance(branch, Mapping) or "case" not in branch or "then" not in branch:
            raise ExpressionError("each $switch branch needs case and then")
        if _bool(_eval(branch["case"], ctx)):
            return _eval(branch["then"], ctx)
    if "default" in arg:
        return _eval(arg["default"], ctx)
    raise ExpressionError("$switch found no matching branch and no default")


# Mirror of query.py's pattern-length cap. Python `re` has no match
# timeout; capping pattern length sidesteps catastrophic-backtracking
# patterns reachable via $regexMatch / $regexFind / $regexFindAll.
_MAX_REGEX_PATTERN_LEN = 1000


def _resolve_regex(arg: Any, ctx: _Ctx) -> tuple[str, int]:

    from bson import Regex

    if not isinstance(arg, Mapping):
        raise ExpressionError("regex expression requires {input, regex, options?}")
    raw_pattern = _eval(arg.get("regex"), ctx)
    raw_options = _eval(arg.get("options"), ctx) if "options" in arg else ""
    pattern = raw_pattern
    flags = 0
    if isinstance(pattern, Regex):
        flags |= _re_flags(pattern.flags)
        pattern = pattern.pattern
    if isinstance(raw_options, str):
        flags |= _re_flags(raw_options)
    if not isinstance(pattern, str):
        raise ExpressionError("regex must be a string or BSON Regex")
    if len(pattern) > _MAX_REGEX_PATTERN_LEN:
        raise ExpressionError(
            f"regex pattern of {len(pattern)} chars exceeds the {_MAX_REGEX_PATTERN_LEN}-char cap"
        )
    return pattern, flags


def _op_regex_match(arg: Any, ctx: _Ctx) -> Any:
    import re as _re

    if not isinstance(arg, Mapping):
        raise ExpressionError("$regexMatch requires {input, regex, options?}")
    s = _eval(arg.get("input"), ctx)
    if not isinstance(s, str):
        return False
    pattern, flags = _resolve_regex(arg, ctx)
    return bool(_re.compile(pattern, flags).search(s))


def _op_regex_find(arg: Any, ctx: _Ctx) -> Any:
    import re as _re

    if not isinstance(arg, Mapping):
        raise ExpressionError("$regexFind requires {input, regex, options?}")
    s = _eval(arg.get("input"), ctx)
    if not isinstance(s, str):
        return None
    pattern, flags = _resolve_regex(arg, ctx)
    m = _re.compile(pattern, flags).search(s)
    if m is None:
        return None
    return {"match": m.group(0), "idx": m.start(), "captures": list(m.groups())}


def _add_months(d: _dt.datetime, months: int) -> _dt.datetime:
    import calendar

    new_month_total = d.month - 1 + months
    new_year = d.year + new_month_total // 12
    new_month = (new_month_total % 12) + 1
    last_day = calendar.monthrange(new_year, new_month)[1]
    new_day = min(d.day, last_day)
    return d.replace(year=new_year, month=new_month, day=new_day)


def _shift_date(d: _dt.datetime, unit: str, amount: int) -> _dt.datetime:
    if unit == "year":
        return _add_months(d, amount * 12)
    if unit == "quarter":
        return _add_months(d, amount * 3)
    if unit == "month":
        return _add_months(d, amount)
    if unit == "week":
        return d + _dt.timedelta(weeks=amount)
    if unit == "day":
        return d + _dt.timedelta(days=amount)
    if unit == "hour":
        return d + _dt.timedelta(hours=amount)
    if unit == "minute":
        return d + _dt.timedelta(minutes=amount)
    if unit == "second":
        return d + _dt.timedelta(seconds=amount)
    if unit == "millisecond":
        return d + _dt.timedelta(milliseconds=amount)
    raise ExpressionError(f"unsupported date unit: {unit!r}")


def _op_date_add(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateAdd requires a document spec")
    start = _eval(arg.get("startDate"), ctx)
    unit = _eval(arg.get("unit"), ctx)
    amount = _eval(arg.get("amount"), ctx)
    if start is None or amount is None:
        return None
    if not isinstance(start, _dt.datetime):
        raise ExpressionError("$dateAdd startDate must be a datetime")
    if not isinstance(unit, str) or not isinstance(amount, int):
        raise ExpressionError("$dateAdd needs string unit and integer amount")
    return _shift_date(start, unit, amount)


def _op_date_subtract(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateSubtract requires a document spec")
    start = _eval(arg.get("startDate"), ctx)
    unit = _eval(arg.get("unit"), ctx)
    amount = _eval(arg.get("amount"), ctx)
    if start is None or amount is None:
        return None
    if not isinstance(start, _dt.datetime):
        raise ExpressionError("$dateSubtract startDate must be a datetime")
    if not isinstance(unit, str) or not isinstance(amount, int):
        raise ExpressionError("$dateSubtract needs string unit and integer amount")
    return _shift_date(start, unit, -amount)


def _op_date_trunc(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateTrunc requires a document spec")
    date = _eval(arg.get("date"), ctx)
    if date is None:
        return None
    if not isinstance(date, _dt.datetime):
        raise ExpressionError("$dateTrunc date must be a datetime")
    unit = _eval(arg.get("unit"), ctx)
    if not isinstance(unit, str):
        raise ExpressionError("$dateTrunc unit must be a string")
    bin_size = _eval(arg.get("binSize"), ctx) if "binSize" in arg else 1
    if not isinstance(bin_size, int) or bin_size < 1:
        raise ExpressionError("$dateTrunc binSize must be a positive integer")
    if unit == "year":
        new_year = date.year - ((date.year - 1) % bin_size)
        return _dt.datetime(new_year, 1, 1, tzinfo=date.tzinfo)
    if unit == "quarter":
        q_index = (date.month - 1) // 3
        q_index -= q_index % bin_size
        return _dt.datetime(date.year, q_index * 3 + 1, 1, tzinfo=date.tzinfo)
    if unit == "month":
        m = date.month - ((date.month - 1) % bin_size)
        return _dt.datetime(date.year, m, 1, tzinfo=date.tzinfo)
    if unit == "week":
        epoch = _dt.datetime(1970, 1, 5, tzinfo=date.tzinfo)  # Mondays
        weeks = (date - epoch).days // 7
        weeks -= weeks % bin_size
        return epoch + _dt.timedelta(weeks=weeks)
    if unit == "day":
        epoch = _dt.datetime(1970, 1, 1, tzinfo=date.tzinfo)
        days = (date - epoch).days
        days -= days % bin_size
        return epoch + _dt.timedelta(days=days)
    if unit == "hour":
        zeroed = date.replace(minute=0, second=0, microsecond=0)
        zeroed = zeroed.replace(hour=zeroed.hour - (zeroed.hour % bin_size))
        return zeroed
    if unit == "minute":
        zeroed = date.replace(second=0, microsecond=0)
        zeroed = zeroed.replace(minute=zeroed.minute - (zeroed.minute % bin_size))
        return zeroed
    if unit == "second":
        zeroed = date.replace(microsecond=0)
        zeroed = zeroed.replace(second=zeroed.second - (zeroed.second % bin_size))
        return zeroed
    if unit == "millisecond":
        ms = date.microsecond // 1000
        ms -= ms % bin_size
        return date.replace(microsecond=ms * 1000)
    raise ExpressionError(f"unsupported $dateTrunc unit: {unit!r}")


def _op_date_to_parts(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateToParts requires a document spec")
    date = _eval(arg.get("date"), ctx)
    if date is None:
        return None
    if not isinstance(date, _dt.datetime):
        raise ExpressionError("$dateToParts date must be a datetime")
    return {
        "year": date.year,
        "month": date.month,
        "day": date.day,
        "hour": date.hour,
        "minute": date.minute,
        "second": date.second,
        "millisecond": date.microsecond // 1000,
    }


def _op_date_diff(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateDiff requires a document spec")
    start = _eval(arg.get("startDate"), ctx)
    end = _eval(arg.get("endDate"), ctx)
    unit = _eval(arg.get("unit"), ctx)
    if start is None or end is None:
        return None
    if not isinstance(start, _dt.datetime) or not isinstance(end, _dt.datetime):
        raise ExpressionError("$dateDiff endpoints must be datetimes")
    if not isinstance(unit, str):
        raise ExpressionError("$dateDiff needs a string unit")
    if unit == "year":
        return end.year - start.year - (1 if (end.month, end.day) < (start.month, start.day) else 0)
    if unit == "quarter":
        sq = (start.year, (start.month - 1) // 3)
        eq = (end.year, (end.month - 1) // 3)
        return (eq[0] - sq[0]) * 4 + (eq[1] - sq[1])
    if unit == "month":
        return (
            (end.year - start.year) * 12
            + (end.month - start.month)
            - (1 if end.day < start.day else 0)
        )
    delta = end - start
    if unit == "week":
        return delta.days // 7
    if unit == "day":
        return delta.days
    if unit == "hour":
        return int(delta.total_seconds() // 3600)
    if unit == "minute":
        return int(delta.total_seconds() // 60)
    if unit == "second":
        return int(delta.total_seconds())
    if unit == "millisecond":
        return int(delta.total_seconds() * 1000)
    raise ExpressionError(f"unsupported date unit: {unit!r}")


def _op_regex_find_all(arg: Any, ctx: _Ctx) -> Any:
    import re as _re

    if not isinstance(arg, Mapping):
        raise ExpressionError("$regexFindAll requires {input, regex, options?}")
    s = _eval(arg.get("input"), ctx)
    if not isinstance(s, str):
        return []
    pattern, flags = _resolve_regex(arg, ctx)
    out: list[dict[str, Any]] = []
    for m in _re.compile(pattern, flags).finditer(s):
        out.append({"match": m.group(0), "idx": m.start(), "captures": list(m.groups())})
    return out


def _re_flags(flags_input: Any) -> int:
    import re as _re

    if isinstance(flags_input, int):
        return flags_input
    if isinstance(flags_input, bytes):
        flags_input = flags_input.decode()
    flags = 0
    flag_map = {"i": _re.IGNORECASE, "m": _re.MULTILINE, "s": _re.DOTALL, "x": _re.VERBOSE}
    for c in flags_input or "":
        flags |= flag_map.get(c, 0)
    return flags


def _op_array_to_object(arg: Any, ctx: _Ctx) -> Any:
    v = _eval(arg, ctx)
    if v is None:
        return None
    if not isinstance(v, list):
        raise ExpressionError("$arrayToObject requires an array")
    out: dict[str, Any] = {}
    for entry in v:
        if isinstance(entry, Mapping) and "k" in entry and "v" in entry:
            out[str(entry["k"])] = entry["v"]
        elif isinstance(entry, list) and len(entry) == 2:
            out[str(entry[0])] = entry[1]
        else:
            raise ExpressionError("$arrayToObject entries must be {k, v} docs or [k, v] pairs")
    return out


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


def _op_index_of_bytes(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or not 2 <= len(arg) <= 4:
        raise ExpressionError("$indexOfBytes requires [string, search, start?, end?]")
    s = _eval(arg[0], ctx)
    needle = _eval(arg[1], ctx)
    if s is None:
        return None
    if not isinstance(s, str) or not isinstance(needle, str):
        raise ExpressionError("$indexOfBytes requires string operands")
    start = _eval(arg[2], ctx) if len(arg) >= 3 else 0
    end = _eval(arg[3], ctx) if len(arg) >= 4 else len(s.encode("utf-8"))
    if not isinstance(start, int) or not isinstance(end, int):
        return -1
    haystack = s.encode("utf-8")
    needle_b = needle.encode("utf-8")
    return haystack.find(needle_b, start, end)


def _op_str_len_bytes(arg: Any, ctx: _Ctx) -> Any:
    s = _eval(arg, ctx)
    if not isinstance(s, str):
        raise ExpressionError("$strLenBytes requires a string")
    return len(s.encode("utf-8"))


def _op_substr_bytes(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) != 3:
        raise ExpressionError("$substrBytes requires [string, start, length]")
    s = _eval(arg[0], ctx)
    start = _eval(arg[1], ctx)
    length = _eval(arg[2], ctx)
    if s is None:
        return ""
    if not isinstance(s, str) or not isinstance(start, int) or not isinstance(length, int):
        raise ExpressionError("$substrBytes requires string + ints")
    encoded = s.encode("utf-8")
    if length < 0:
        return encoded[start:].decode("utf-8", errors="replace")
    return encoded[start : start + length].decode("utf-8", errors="replace")


def _op_index_of_array(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or not 2 <= len(arg) <= 4:
        raise ExpressionError("$indexOfArray requires [array, search, start?, end?]")
    arr = _eval(arg[0], ctx)
    if arr is None:
        return None
    if not isinstance(arr, list):
        raise ExpressionError("$indexOfArray first argument must be an array")
    needle = _eval(arg[1], ctx)
    start = _eval(arg[2], ctx) if len(arg) >= 3 else 0
    end = _eval(arg[3], ctx) if len(arg) >= 4 else len(arr)
    if not isinstance(start, int) or not isinstance(end, int):
        return -1
    for i in range(max(0, start), min(len(arr), end)):
        if arr[i] == needle:
            return i
    return -1


def _op_let(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping) or "vars" not in arg or "in" not in arg:
        raise ExpressionError("$let requires {vars, in}")
    bindings = arg["vars"]
    if not isinstance(bindings, Mapping):
        raise ExpressionError("$let.vars must be a document")
    inner = ctx
    for name, value_expr in bindings.items():
        inner = inner.with_var(name, _eval(value_expr, ctx))
    return _eval(arg["in"], inner)


# Hard cap on the size of a `$range` result. Without this, a single
# document like `{$project: {r: {$range: [0, 1_000_000_000]}}}` is an
# OOM bomb (allocates ~8 GB in CPython). MongoDB caps at 64 MB BSON
# but doesn't materialise into Python — we have to cap explicitly.
_MAX_RANGE_SIZE = 100_000


def _op_range(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or not 2 <= len(arg) <= 3:
        raise ExpressionError("$range requires [start, end, step?]")
    start = _eval(arg[0], ctx)
    end = _eval(arg[1], ctx)
    step = _eval(arg[2], ctx) if len(arg) == 3 else 1
    if not all(isinstance(v, int) and not isinstance(v, bool) for v in (start, end, step)):
        raise ExpressionError("$range requires integer arguments")
    if step == 0:
        raise ExpressionError("$range step cannot be zero")
    # Compute the size symbolically so we never call list(range(...)) on
    # a billion-element range.
    delta = end - start
    if (delta > 0) == (step > 0):
        size = (abs(delta) + abs(step) - 1) // abs(step)
        if size > _MAX_RANGE_SIZE:
            raise ExpressionError(
                f"$range result of {size} elements exceeds the {_MAX_RANGE_SIZE}-element cap"
            )
    return list(range(start, end, step))


def _op_zip(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping) or "inputs" not in arg:
        raise ExpressionError("$zip requires {inputs, useLongestLength?, defaults?}")
    inputs = _eval(arg["inputs"], ctx)
    if inputs is None:
        return None
    if not isinstance(inputs, list) or not all(isinstance(a, list) for a in inputs):
        raise ExpressionError("$zip inputs must be an array of arrays")
    use_longest = bool(arg.get("useLongestLength"))
    defaults = arg.get("defaults") or [None] * len(inputs)
    if not isinstance(defaults, list):
        raise ExpressionError("$zip defaults must be an array")
    if use_longest:
        n = max((len(a) for a in inputs), default=0)
        out: list[list[Any]] = []
        for i in range(n):
            row = []
            for j, a in enumerate(inputs):
                if i < len(a):
                    row.append(a[i])
                else:
                    row.append(defaults[j] if j < len(defaults) else None)
            out.append(row)
        return out
    n = min((len(a) for a in inputs), default=0)
    return [[a[i] for a in inputs] for i in range(n)]


def _op_sort_array(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping) or "input" not in arg or "sortBy" not in arg:
        raise ExpressionError("$sortArray requires {input, sortBy}")
    arr = _eval(arg["input"], ctx)
    if arr is None:
        return None
    if not isinstance(arr, list):
        raise ExpressionError("$sortArray input must be an array")
    sort_by = arg["sortBy"]
    if isinstance(sort_by, int):
        return sorted(arr, reverse=(sort_by == -1))
    if not isinstance(sort_by, Mapping):
        raise ExpressionError("$sortArray sortBy must be int or document")

    def _key(elem: Any) -> tuple[Any, ...]:
        from secantus.storage import _SortKey

        return tuple(
            _SortKey(get_path(elem if isinstance(elem, dict) else {}, field)) for field in sort_by
        )

    result = list(arr)
    for sort_field, direction in reversed(list(sort_by.items())):
        result.sort(
            key=lambda d, f=sort_field: _make_sort_key(d, f),
            reverse=(int(direction) == -1),
        )
    return result


def _make_sort_key(elem: Any, field: str) -> Any:
    from secantus.storage import _SortKey

    if isinstance(elem, Mapping):
        return _SortKey(get_path(dict(elem), field))
    return _SortKey(elem)


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


def _resolve_timezone(name: Any) -> _dt.tzinfo | None:
    """Resolve MongoDB-style timezone strings to a Python ``tzinfo``.

    Accepts IANA names ("Europe/Dublin"), UTC offsets ("+05:30",
    "-04:00", "+0530"), and the aliases "GMT" / "UTC". ``None`` yields
    ``None`` (caller treats input as already in its own zone).
    """
    if name is None:
        return None
    if not isinstance(name, str):
        raise ExpressionError(f"timezone must be a string, got {type(name).__name__}")
    if name in ("UTC", "GMT", "Etc/UTC", "Etc/GMT"):
        return _dt.timezone.utc
    if name and name[0] in ("+", "-"):
        sign = 1 if name[0] == "+" else -1
        digits = name[1:].replace(":", "")
        if len(digits) == 4 and digits.isdigit():
            hours = int(digits[:2])
            minutes = int(digits[2:])
            return _dt.timezone(sign * _dt.timedelta(hours=hours, minutes=minutes))
        raise ExpressionError(f"unknown timezone: {name!r}")
    try:
        return zoneinfo.ZoneInfo(name)
    except zoneinfo.ZoneInfoNotFoundError as exc:
        raise ExpressionError(f"unknown timezone: {name!r}") from exc


def _op_date_from_string(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateFromString requires a document spec")
    raw = _eval(arg.get("dateString"), ctx)
    if raw is None:
        return _eval(arg["onNull"], ctx) if "onNull" in arg else None
    if not isinstance(raw, str):
        raise ExpressionError("$dateFromString dateString must be a string")
    fmt = arg.get("format")
    tz = _resolve_timezone(arg.get("timezone"))
    try:
        if isinstance(fmt, str):
            parsed = _dt.datetime.strptime(raw, fmt)
        else:
            parsed = _dt.datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError as exc:
        if "onError" in arg:
            return _eval(arg["onError"], ctx)
        raise ExpressionError(f"$dateFromString cannot parse {raw!r}: {exc}") from exc
    if tz is not None and parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=tz)
    return parsed


def _op_date_to_string(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateToString requires {date, format}")
    d = _ensure_datetime(_eval(arg["date"], ctx))
    if d is None:
        return None
    fmt = arg.get("format", "%Y-%m-%dT%H:%M:%S.%LZ")
    if not isinstance(fmt, str):
        raise ExpressionError("$dateToString format must be a string")
    tz = _resolve_timezone(arg.get("timezone"))
    if tz is not None:
        # Naive input is treated as UTC, matching MongoDB's BSON Date semantics.
        d_aware = d if d.tzinfo is not None else d.replace(tzinfo=_dt.timezone.utc)
        d = d_aware.astimezone(tz)
    out = fmt
    # Pre-process tokens whose mongod semantics differ from Python's
    # strftime, then hand the rest off to strftime untouched.
    # ``%L`` — 3-digit milliseconds (mongod-only token).
    if "%L" in out:
        out = out.replace("%L", f"{d.microsecond // 1000:03d}")
    # ``%w`` — mongod numbers days 1-Sunday through 7-Saturday;
    # Python's strftime numbers them 0-Sunday through 6-Saturday.
    # Substitute the resolved digit directly so strftime never sees
    # the token. Formula: ((weekday() + 1) % 7) + 1 maps
    # Mon..Sun (0..6) → 2..7,1 i.e. mongod's Sunday=1 numbering.
    if "%w" in out:
        out = out.replace("%w", str(((d.weekday() + 1) % 7) + 1))
    # ``%G`` (ISO year), ``%V`` (ISO week 1-53), ``%j`` (day of year
    # 001-366), ``%U`` (Sunday-start week 00-53), ``%u`` (ISO weekday
    # 1-Mon … 7-Sun), ``%Y``, ``%m``, ``%d``, ``%H``, ``%M``, ``%S``,
    # ``%z``, ``%Z``, ``%%`` — all match mongod's tokens and pass
    # straight through to Python's strftime.
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


# `int(very_long_string)` is O(n^2) in CPython. Python 3.11+ enforces a
# default 4300-digit max via `sys.set_int_max_str_digits` (PEP 750), but
# (a) it can be disabled at runtime, (b) the threshold above which it
# bites is not consistent across versions, and (c) we'd rather raise a
# clear ExpressionError than the underlying ValueError. Hard-cap here.
_MAX_INT_STR_DIGITS = 4300


def _safe_int_from_str(value: str, op_name: str) -> int:
    if len(value) > _MAX_INT_STR_DIGITS:
        raise ExpressionError(
            f"{op_name} input string of {len(value)} chars exceeds the "
            f"{_MAX_INT_STR_DIGITS}-char int-conversion cap"
        )
    try:
        return int(value)
    except ValueError as exc:
        raise ExpressionError(f"{op_name} cannot convert {value!r}") from exc


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
        return _safe_int_from_str(value, "$toInt")
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


_CONVERT_TARGETS = {
    "double": 1,
    1: 1,
    "string": 2,
    2: 2,
    "objectId": 7,
    7: 7,
    "bool": 8,
    8: 8,
    "date": 9,
    9: 9,
    "int": 16,
    16: 16,
    "long": 18,
    18: 18,
    "decimal": 19,
    19: 19,
}


def _convert_value(value: Any, target: Any) -> Any:
    from bson import ObjectId as _ObjectId

    code = _CONVERT_TARGETS.get(target)
    if code is None:
        raise ExpressionError(f"$convert unsupported target type {target!r}")
    if code == 1:
        if isinstance(value, bool):
            return 1.0 if value else 0.0
        if isinstance(value, (int, float)):
            return float(value)
        if isinstance(value, Decimal128):
            return float(value.to_decimal())
        if isinstance(value, str):
            return float(value)
        if isinstance(value, _dt.datetime):
            return value.timestamp() * 1000.0
    elif code == 2:
        if isinstance(value, _dt.datetime):
            return value.isoformat()
        return str(value)
    elif code == 7:
        if isinstance(value, _ObjectId):
            return value
        if isinstance(value, str):
            return _ObjectId(value)
    elif code == 8:
        if isinstance(value, bool):
            return value
        if isinstance(value, (int, float)):
            return value != 0
        if isinstance(value, Decimal128):
            return value.to_decimal() != Decimal(0)
        if isinstance(value, str):
            return len(value) > 0
        return True
    elif code == 9:
        if isinstance(value, _dt.datetime):
            return value
        if isinstance(value, str):
            return _dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if isinstance(value, (int, float)):
            return _dt.datetime.fromtimestamp(value / 1000.0, tz=_dt.timezone.utc)
    elif code in (16, 18):
        # 16 = int32, 18 = int64. Wrap as ``Int64`` for code 18 so the
        # result matches ``$type: "long"`` downstream — the bson decoder
        # preserves the int32/int64 distinction by type, and ``$convert``
        # must respect the requested target type.
        def _wrap(n: int) -> int:
            return Int64(n) if code == 18 else int(n)

        if isinstance(value, bool):
            return _wrap(1 if value else 0)
        if isinstance(value, int):
            return _wrap(int(value))
        if isinstance(value, float):
            return _wrap(int(value))
        if isinstance(value, Decimal128):
            return _wrap(int(value.to_decimal()))
        if isinstance(value, str):
            return _wrap(_safe_int_from_str(value, "$convert (int/long)"))
    elif code == 19:
        if isinstance(value, Decimal128):
            return value
        if isinstance(value, bool):
            return Decimal128(Decimal(1 if value else 0))
        if isinstance(value, int):
            return Decimal128(Decimal(value))
        if isinstance(value, float):
            return Decimal128(Decimal(repr(value)))
        if isinstance(value, str):
            if len(value) > _MAX_INT_STR_DIGITS:
                raise ExpressionError(
                    f"$convert (decimal) input string of {len(value)} chars "
                    f"exceeds the {_MAX_INT_STR_DIGITS}-char cap"
                )
            return Decimal128(value)
    raise ExpressionError(f"$convert cannot convert {type(value).__name__} to {target!r}")


def _op_convert(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping) or "input" not in arg or "to" not in arg:
        raise ExpressionError("$convert requires {input, to}")
    value = _eval(arg["input"], ctx)
    target = _eval(arg["to"], ctx)
    if value is None:
        return _eval(arg["onNull"], ctx) if "onNull" in arg else None
    try:
        return _convert_value(value, target)
    except (ValueError, TypeError, InvalidOperation, ExpressionError) as exc:
        if "onError" in arg:
            return _eval(arg["onError"], ctx)
        raise ExpressionError(f"$convert failed: {exc}") from exc


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
    "$abs": _op_abs,
    "$round": _op_round,
    "$floor": _op_floor,
    "$ceil": _op_ceil,
    "$sqrt": _op_sqrt,
    "$pow": _op_pow,
    "$exp": _op_exp,
    "$ln": _op_ln,
    "$log": _op_log,
    "$log10": _op_log10,
    "$rand": _op_rand,
    "$trunc": _op_trunc,
    "$mergeObjects": _op_merge_objects,
    "$objectToArray": _op_object_to_array,
    "$setField": _op_set_field,
    "$getField": _op_get_field,
    "$arrayToObject": _op_array_to_object,
    "$switch": _op_switch,
    "$regexMatch": _op_regex_match,
    "$regexFind": _op_regex_find,
    "$regexFindAll": _op_regex_find_all,
    "$dateAdd": _op_date_add,
    "$dateSubtract": _op_date_subtract,
    "$dateDiff": _op_date_diff,
    "$dateTrunc": _op_date_trunc,
    "$dateToParts": _op_date_to_parts,
    "$split": _op_split,
    "$trim": _op_trim,
    "$ltrim": _op_ltrim,
    "$rtrim": _op_rtrim,
    "$substr": _op_substr_cp,
    "$substrCP": _op_substr_cp,
    "$strLenCP": _op_str_len_cp,
    "$indexOfCP": _op_index_of_cp,
    "$indexOfBytes": _op_index_of_bytes,
    "$strLenBytes": _op_str_len_bytes,
    "$substrBytes": _op_substr_bytes,
    "$indexOfArray": _op_index_of_array,
    "$let": _op_let,
    "$range": _op_range,
    "$zip": _op_zip,
    "$sortArray": _op_sort_array,
    "$year": _op_year,
    "$month": _op_month,
    "$dayOfMonth": _op_day_of_month,
    "$dayOfWeek": _op_day_of_week,
    "$hour": _op_hour,
    "$minute": _op_minute,
    "$second": _op_second,
    "$dateToString": _op_date_to_string,
    "$dateFromString": _op_date_from_string,
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
    "$convert": _op_convert,
    "$filter": _op_filter,
    "$map": _op_map,
    "$reduce": _op_reduce,
}
