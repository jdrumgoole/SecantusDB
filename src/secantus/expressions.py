from __future__ import annotations

import datetime as _dt
import math
import zoneinfo
from collections.abc import Mapping
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Any

import bson
from bson import Decimal128, Int64

from secantus.paths import get_path


class ExpressionError(Exception):
    """Expression-evaluation error surfaced to the client as ``{ok: 0}``.

    ``dispatch`` maps it to ``code: 14 TypeMismatch`` by default — the
    code mongod uses for most aggregation type errors. Operators whose
    mongod error carries a different code (``$divide`` by zero is 2
    BadValue; ``$mod`` uses 16610/16611 Location codes) pass it
    explicitly.
    """

    _CODE_NAMES = {14: "TypeMismatch", 2: "BadValue", 168: "InvalidPipelineOperator"}

    def __init__(self, msg: str, *, code: int = 14, code_name: str | None = None) -> None:
        super().__init__(msg)
        self.code = code
        self.code_name = code_name or self._CODE_NAMES.get(code, f"Location{code}")


class UnknownExpressionOperatorError(ExpressionError):
    """A ``$``-prefixed expression operator SecantusDB does not recognize.

    mongod surfaces different codes depending on context: a ``$expr`` inside
    a query yields ``168 InvalidPipelineOperator`` with the message
    ``Unrecognized expression '$op'`` (the default this class carries); an
    expression inside an aggregation stage such as ``$project`` yields a
    stage-specific ``Location`` code wrapping ``Unknown expression $op``.
    Stage handlers catch this and re-raise with the wrapped form.
    """

    def __init__(self, op: str) -> None:
        super().__init__(f"Unrecognized expression '{op}'", code=168)
        self.op = op


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
            # No defensive copy: get_path/walk_to_parent(create=False) is
            # read-only, and copying the whole doc per $field reference was
            # a per-doc-per-field allocation on every $project/$group.
            d = ctx.doc if isinstance(ctx.doc, dict) else dict(ctx.doc)
            return get_path(d, expr[1:], default=None)
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

#: Sentinel for "the expression resolved to a missing field" (distinct from an
#: explicit ``null``). Reuses the ``$$REMOVE`` marker.
MISSING: Any = _REMOVE_SENTINEL


def evaluate_or_missing(
    expr: Any, doc: Mapping[str, Any], vars: dict[str, Any] | None = None
) -> Any:
    """Like :func:`evaluate`, but a top-level absent field path yields
    :data:`MISSING` (distinct from ``None``) so accumulators can skip a missing
    value the way mongod does — ``$push`` / ``$addToSet`` accumulate an explicit
    ``null`` but not a missing field."""
    if isinstance(expr, str) and expr.startswith("$") and not expr.startswith("$$"):
        d = doc if isinstance(doc, dict) else dict(doc)
        return get_path(d, expr[1:], default=MISSING)
    return evaluate(expr, doc, vars)


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
        raise ExpressionError(
            f"Use of undefined variable: {base}", code=17276, code_name="Location17276"
        )
    if not rest:
        return value
    if not isinstance(value, Mapping):
        return None
    return get_path(value if isinstance(value, dict) else dict(value), rest, default=None)


def _apply_op(op: str, arg: Any, ctx: _Ctx) -> Any:
    if op == "$literal":
        return arg
    handler = _OPS.get(op)
    if handler is None:
        raise UnknownExpressionOperatorError(op)
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


def _op_concat(arg: Any, ctx: _Ctx) -> Any:
    # mongod: every operand must be a string; a null / missing operand short-
    # circuits to a null result (left-to-right), and a non-string operand is
    # Location16702 — no silent str() coercion.
    parts = []
    for p in _eval_args(arg, ctx):
        if p is None:
            return None
        if not isinstance(p, str):
            raise ExpressionError(
                f"$concat only supports strings, not {_bson_type_name(p)}",
                code=16702,
                code_name="Location16702",
            )
        parts.append(p)
    return "".join(parts)


def _bson_type_name(v: Any) -> str:
    """mongod's type vocabulary for arithmetic error messages."""
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, Int64):
        return "long"
    if isinstance(v, int):
        return "int" if -(2**31) <= v < 2**31 else "long"
    if isinstance(v, float):
        return "double"
    if isinstance(v, Decimal128):
        return "decimal"
    if isinstance(v, str):
        return "string"
    if isinstance(v, _dt.datetime):
        return "date"
    if isinstance(v, Mapping):
        return "object"
    if isinstance(v, list):
        return "array"
    if isinstance(v, bson.ObjectId):
        return "objectId"
    if v is None:
        return "null"
    return type(v).__name__


def _is_numeric(v: Any) -> bool:
    # bool is an int subclass in Python but NOT numeric in BSON arithmetic —
    # mongod rejects it ("$multiply only supports numeric types, not bool").
    return isinstance(v, (int, float, Decimal128)) and not isinstance(v, bool)


def _fmt_double(v: float) -> str:
    """Render a double the way mongod prints it in an error message: a
    shortest round-trip form (Python's `repr` matches for the values that
    reach these paths — small fractionals like `2.7`)."""
    return repr(v)


class _FractionalIndex(Exception):
    """Signal: a double index arg has a fractional part (mongod rejects it)."""


def _int_index(v: Any) -> Any:
    """Coerce a numeric aggregation index arg to `int`, mongod-style: an `int`
    passes through, a whole-number `float` becomes `int`, a fractional `float`
    raises `_FractionalIndex` (the caller turns it into the operator's exact
    error code). Any other type is returned unchanged for the caller's own
    non-numeric handling. `bool` must be rejected by the caller first."""
    if isinstance(v, float):
        if v.is_integer():
            return int(v)
        raise _FractionalIndex
    return v


def _fold_numeric(values: list[Any], *, mul: bool) -> Any:
    """Sum or product over validated numeric operands. Mixing in a
    Decimal128 promotes the whole fold to decimal, like mongod's
    type-widening; ``Decimal(float)`` keeps the exact binary expansion
    mongod's double→decimal conversion produces."""
    if any(isinstance(v, Decimal128) for v in values):
        acc = Decimal(1 if mul else 0)
        for v in values:
            d = v.to_decimal() if isinstance(v, Decimal128) else Decimal(v)
            acc = acc * d if mul else acc + d
        return Decimal128(acc)
    acc2: Any = 1 if mul else 0
    for v in values:
        acc2 = acc2 * v if mul else acc2 + v
    return acc2


def _op_add(arg: Any, ctx: _Ctx) -> Any:
    values = _eval_args(arg, ctx)
    if any(v is None for v in values):
        return None
    dates = [v for v in values if isinstance(v, _dt.datetime)]
    if len(dates) > 1:
        raise ExpressionError("only one date allowed in an $add expression", code=16612)
    nums = [v for v in values if not isinstance(v, _dt.datetime)]
    for v in nums:
        if not _is_numeric(v):
            raise ExpressionError(
                f"$add only supports numeric or date types, not {_bson_type_name(v)}"
            )
    if dates:
        # date + numerics: the numeric sum is a millisecond offset.
        offset = _fold_numeric(nums, mul=False) if nums else 0
        if isinstance(offset, Decimal128):
            offset = float(offset.to_decimal())
        return dates[0] + _dt.timedelta(milliseconds=offset)
    if len(values) == 1:
        return values[0]
    return _fold_numeric(values, mul=False)


def _op_subtract(arg: Any, ctx: _Ctx) -> Any:
    a, b = _eval_args(arg, ctx)
    if a is None or b is None:
        return None
    a_date, b_date = isinstance(a, _dt.datetime), isinstance(b, _dt.datetime)
    if a_date and b_date:
        return Int64(round((a - b).total_seconds() * 1000))
    if a_date and _is_numeric(b):
        ms = float(b.to_decimal()) if isinstance(b, Decimal128) else b
        return a - _dt.timedelta(milliseconds=ms)
    if _is_numeric(a) and _is_numeric(b):
        if isinstance(a, Decimal128) or isinstance(b, Decimal128):
            da = a.to_decimal() if isinstance(a, Decimal128) else Decimal(a)
            db = b.to_decimal() if isinstance(b, Decimal128) else Decimal(b)
            return Decimal128(da - db)
        return a - b
    raise ExpressionError(f"can't $subtract {_bson_type_name(b)} from {_bson_type_name(a)}")


def _op_multiply(arg: Any, ctx: _Ctx) -> Any:
    values = _eval_args(arg, ctx)
    if any(v is None for v in values):
        return None
    for v in values:
        if not _is_numeric(v):
            raise ExpressionError(
                f"$multiply only supports numeric types, not {_bson_type_name(v)}"
            )
    return _fold_numeric(values, mul=True)


def _op_divide(arg: Any, ctx: _Ctx) -> Any:
    a, b = _eval_args(arg, ctx)
    if a is None or b is None:
        return None
    if not (_is_numeric(a) and _is_numeric(b)):
        raise ExpressionError(
            f"$divide only supports numeric types, not "
            f"{_bson_type_name(a)} and {_bson_type_name(b)}"
        )
    if b == 0:
        raise ExpressionError("can't $divide by zero", code=2)
    if isinstance(a, Decimal128) or isinstance(b, Decimal128):
        da = a.to_decimal() if isinstance(a, Decimal128) else Decimal(a)
        db = b.to_decimal() if isinstance(b, Decimal128) else Decimal(b)
        return Decimal128(da / db)
    return a / b


def _op_mod(arg: Any, ctx: _Ctx) -> Any:
    a, b = _eval_args(arg, ctx)
    if a is None or b is None:
        return None
    if not (_is_numeric(a) and _is_numeric(b)):
        raise ExpressionError(
            f"$mod only supports numeric types, not {_bson_type_name(a)} and {_bson_type_name(b)}",
            code=16611,
        )
    if b == 0:
        raise ExpressionError("can't $mod by zero", code=16610)
    return a % b


def _op_and(arg: Any, ctx: _Ctx) -> bool:
    return all(_bool(_eval(a, ctx)) for a in arg)


def _op_or(arg: Any, ctx: _Ctx) -> bool:
    return any(_bool(_eval(a, ctx)) for a in arg)


def _op_not(arg: Any, ctx: _Ctx) -> bool:
    inner = arg[0] if isinstance(arg, list) else arg
    return not _bool(_eval(inner, ctx))


def _unwrap_d128(v: Any) -> Any:
    """A ``Decimal128`` as a plain ``Decimal`` for comparison.

    Decimal128 has no Python comparison operators, so ``12 < Decimal128("15")``
    raised TypeError, which the range operators below swallow into False, and
    ``==`` simply answered False. Every comparison against a decimal was
    therefore wrong. mongod compares the numeric types by value, and a
    ``Decimal`` compares correctly against int and float, so unwrapping is all
    that is needed.
    """
    return v.to_decimal() if isinstance(v, Decimal128) else v


def _cmp_pair(arg: Any, ctx: _Ctx) -> tuple[Any, Any]:
    a, b = _eval_args(arg, ctx)
    return _unwrap_d128(a), _unwrap_d128(b)


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
        n = len(arg) if isinstance(arg, list) else 1
        raise ExpressionError(
            f"$ifNull needs at least two arguments, had: {n}",
            code=1257300,
            code_name="Location1257300",
        )
    *checks, fallback = arg
    for check in checks:
        v = _eval(check, ctx)
        if v is not None:
            return v
    return _eval(fallback, ctx)


def _op_size(arg: Any, ctx: _Ctx) -> int:
    value = _eval(arg, ctx)
    if not isinstance(value, list):
        raise ExpressionError(
            f"The argument to $size must be an array, but was of type: {_bson_type_name(value)}",
            code=17124,
            code_name="Location17124",
        )
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


def _require_math_numeric(v: Any, op: str, code: int = 28765) -> None:
    """mongod's type guard for the unary math operators: a non-numeric operand is
    rejected (Location28765 for most, 51081 for $round / $trunc), rather than
    computing on a coerced bool or leaking a Python TypeError. A null operand is
    handled by the caller (returns null) before this is reached."""
    if not _is_numeric(v):
        raise ExpressionError(
            f"{op} only supports numeric types, not {_bson_type_name(v)}",
            code=code,
            code_name=f"Location{code}",
        )


def _op_abs(arg: Any, ctx: _Ctx) -> Any:
    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$abs")
    return abs(v)


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
    _require_math_numeric(n, "$round", 51081)
    if isinstance(place, bool):
        raise ExpressionError("can't convert from BSON type bool to long", code=16004)
    if isinstance(place, float):
        if not place.is_integer():
            raise ExpressionError(
                "precision argument to  $round must be a integral value", code=51082
            )
        place = int(place)
    if not isinstance(place, int):
        place = 0
    return round(n, place)


def _op_floor(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$floor")
    return math.floor(v)


def _op_ceil(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$ceil")
    return math.ceil(v)


def _op_sqrt(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$sqrt")
    # mongod's domain error (probed 7.0.12): Location28714, not a null result.
    if isinstance(v, (int, float)) and v < 0:
        raise ExpressionError(
            f"$sqrt's argument must be greater than or equal to 0, but is {v}",
            code=28714,
            code_name="Location28714",
        )
    return math.sqrt(v)


def _op_pow(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) != 2:
        raise ExpressionError("$pow requires [base, exponent]")
    base, exponent = _eval(arg[0], ctx), _eval(arg[1], ctx)
    if base is None or exponent is None:
        return None
    if not _is_numeric(base):
        raise ExpressionError(
            f"$pow's base must be numeric, not {_bson_type_name(base)}", code=28762
        )
    if not _is_numeric(exponent):
        raise ExpressionError(
            f"$pow's exponent must be numeric, not {_bson_type_name(exponent)}", code=28763
        )
    if base == 0 and exponent < 0:
        raise ExpressionError("$pow cannot take a base of 0 and a negative exponent", code=28764)
    result = base**exponent
    # A negative base with a fractional exponent yields a Python complex, which
    # is unencodable (crashes BSON) — mongod returns NaN instead.
    if isinstance(result, complex):
        return float("nan")
    return result


def _op_exp(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$exp")
    return math.exp(v)


def _op_ln(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$ln")
    # mongod's domain error (probed 7.0.12): Location28766, not a null result.
    if isinstance(v, (int, float)) and v <= 0:
        raise ExpressionError(
            f"$ln's argument must be a positive number, but is {v}",
            code=28766,
            code_name="Location28766",
        )
    return math.log(v)


def _op_log(arg: Any, ctx: _Ctx) -> Any:
    import math

    if not isinstance(arg, list) or len(arg) != 2:
        raise ExpressionError("$log requires [number, base]")
    n, base = _eval(arg[0], ctx), _eval(arg[1], ctx)
    if n is None or base is None:
        return None
    # mongod's type errors (probed 7.0.12): Location28756 / 28757 — a non-numeric
    # (incl. bool) argument or base is rejected before the domain check.
    if not _is_numeric(n):
        raise ExpressionError(
            f"$log's argument must be numeric, not {_bson_type_name(n)}",
            code=28756,
            code_name="Location28756",
        )
    if not _is_numeric(base):
        raise ExpressionError(
            f"$log's base must be numeric, not {_bson_type_name(base)}",
            code=28757,
            code_name="Location28757",
        )
    # mongod's domain errors (probed 7.0.12): Location28758 / 28759.
    if isinstance(n, (int, float)) and n <= 0:
        raise ExpressionError(
            f"$log's argument must be a positive number, but is {n}",
            code=28758,
            code_name="Location28758",
        )
    if isinstance(base, (int, float)) and (base <= 0 or base == 1):
        raise ExpressionError(
            f"$log's base must be a positive number not equal to 1, but is {base}",
            code=28759,
            code_name="Location28759",
        )
    return math.log(n, base)


def _op_log10(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$log10")
    # mongod's domain error (probed 7.0.12): Location28761, not a null result.
    if isinstance(v, (int, float)) and v <= 0:
        raise ExpressionError(
            f"$log10's argument must be a positive number, but is {v}",
            code=28761,
            code_name="Location28761",
        )
    return math.log10(v)


def _trig_coerce(name: str, v: Any, code: int = 28765) -> float:
    """Coerce a trig operand to float. bool / non-numeric raise ``code``
    (mongod's ``Location28765`` for the unary ops, ``51044`` for ``$atan2``).
    Decimal128 is float-cast, matching ``$degreesToRadians`` (SecantusDB does
    not reproduce mongod's decimal-precise transcendental result)."""
    if isinstance(v, bool) or not isinstance(v, (int, float, Decimal128)):
        raise ExpressionError(f"{name} only supports numeric types, not {_type_name(v)}", code=code)
    return float(v.to_decimal()) if isinstance(v, Decimal128) else float(v)


def _make_trig(name: str, fn: Any, domain: str) -> Any:
    """Build a unary trig operator. ``domain`` gates the input the way mongod
    does (all violations surface ``Location50989``): ``finite`` (sin/cos/tan
    reject ±inf / NaN), ``unit`` (asin/acos need [-1,1]), ``atanh`` (same, but
    ±1 → ±inf rather than a ``math`` domain error), ``geq1`` (acosh needs
    [1,inf)), ``any`` (atan / the hyperbolics accept every finite + infinity)."""

    def op(arg: Any, ctx: _Ctx) -> Any:
        v = _eval(arg, ctx)
        if v is None:
            return None
        x = _trig_coerce(name, v)
        if domain == "finite" and not math.isfinite(x):
            raise ExpressionError(
                f"cannot apply {name} to {x}, value must be in (-inf,inf)", code=50989
            )
        if domain in ("unit", "atanh") and not (-1.0 <= x <= 1.0):
            raise ExpressionError(
                f"cannot apply {name} to {x}, value must be in [-1,1]", code=50989
            )
        if domain == "geq1" and not x >= 1.0:
            raise ExpressionError(
                f"cannot apply {name} to {x}, value must be in [1,inf]", code=50989
            )
        if domain == "atanh" and abs(x) == 1.0:
            return math.inf if x > 0 else -math.inf
        return fn(x)

    return op


def _op_atan2(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) != 2:
        raise ExpressionError("$atan2 requires two arguments", code=51044)
    y = _eval(arg[0], ctx)
    x = _eval(arg[1], ctx)
    if y is None or x is None:
        return None
    fy = _trig_coerce("$atan2", y, code=51044)
    fx = _trig_coerce("$atan2", x, code=51044)
    return math.atan2(fy, fx)


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
    _require_math_numeric(n, "$trunc", 51081)
    if isinstance(place, bool):
        raise ExpressionError("can't convert from BSON type bool to long", code=16004)
    if isinstance(place, float):
        if not place.is_integer():
            raise ExpressionError(
                "precision argument to  $trunc must be a integral value", code=51082
            )
        place = int(place)
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
            raise ExpressionError(
                f"$mergeObjects requires object inputs, but input {v} is of type "
                f"{_bson_type_name(v)}",
                code=40400,
                code_name="Location40400",
            )
        result.update(v)
    return result


def _op_object_to_array(arg: Any, ctx: _Ctx) -> Any:
    v = _eval(arg, ctx)
    if v is None:
        return None
    if not isinstance(v, Mapping):
        raise ExpressionError(
            f"$objectToArray requires a document input, found: {_bson_type_name(v)}",
            code=40390,
            code_name="Location40390",
        )
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
        raise ExpressionError(
            "$setField requires 'field' to evaluate to type String, but got "
            f"{_bson_type_name(field)}",
            code=4161107,
            code_name="Location4161107",
        )
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
            raise ExpressionError(
                "$getField requires 'field' to evaluate to type String, but got "
                f"{_bson_type_name(field)}",
                code=5654602,
                code_name="Location5654602",
            )
    else:
        raise ExpressionError("$getField requires a string or {field, input} document")
    # Evaluate ``input`` in a missing-aware way so we can tell an input that
    # resolved to *missing* (an absent field path) apart from an explicit
    # ``null``. mongod (verified against 6.0):
    #   - input missing            -> $getField is missing  (field dropped)
    #   - input null               -> $getField is null     (field kept null)
    #   - input document, no field -> $getField is missing  (field dropped)
    #   - input document, field present (incl. null) -> that value
    if (
        isinstance(input_expr, str)
        and input_expr.startswith("$")
        and not input_expr.startswith("$$")
    ):
        input_doc = get_path(dict(ctx.doc), input_expr[1:], default=_REMOVE_SENTINEL)
    else:
        input_doc = _eval(input_expr, ctx)
    if input_doc is _REMOVE_SENTINEL:
        return _REMOVE_SENTINEL
    if input_doc is None or not isinstance(input_doc, Mapping):
        return None
    # A field absent from the input document resolves to "missing" (the same
    # marker as ``$$REMOVE``), so a ``$project`` / ``$addFields`` computed field
    # that reads it is omitted from the output. A field present with an explicit
    # ``null`` still returns ``None`` (and is emitted).
    if field not in input_doc:
        return _REMOVE_SENTINEL
    return input_doc[field]


def _op_switch(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$switch requires {branches, default?}")
    branches = arg.get("branches")
    if not isinstance(branches, list):
        raise ExpressionError("$switch branches must be an array")
    if not branches:
        raise ExpressionError(
            "$switch requires at least one branch", code=40068, code_name="Location40068"
        )
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
    if s is None:
        return False
    if not isinstance(s, str):
        raise ExpressionError(
            "$regexMatch needs 'input' to be of type string", code=51104, code_name="Location51104"
        )
    pattern, flags = _resolve_regex(arg, ctx)
    return bool(_re.compile(pattern, flags).search(s))


def _op_regex_find(arg: Any, ctx: _Ctx) -> Any:
    import re as _re

    if not isinstance(arg, Mapping):
        raise ExpressionError("$regexFind requires {input, regex, options?}")
    s = _eval(arg.get("input"), ctx)
    if s is None:
        return None
    if not isinstance(s, str):
        raise ExpressionError(
            "$regexFind needs 'input' to be of type string", code=51104, code_name="Location51104"
        )
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
    raise ExpressionError(f"unknown time unit value: {unit}", code=9, code_name="FailedToParse")


def _date_int(v: Any) -> int | None:
    """An integer date argument (mongod amount / binSize): an int or a whole
    double coerces to int; a fractional double / bool / non-numeric returns None
    for the caller to reject."""
    if isinstance(v, bool):
        return None
    if isinstance(v, int):
        return v
    if isinstance(v, float) and v.is_integer():
        return int(v)
    return None


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
    if not isinstance(unit, str):
        raise ExpressionError("$dateAdd needs a string unit")
    n = _date_int(amount)
    if n is None:
        raise ExpressionError(
            "$dateAdd expects integer amount of time units",
            code=5166405,
            code_name="Location5166405",
        )
    return _shift_date(start, unit, n)


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
    if not isinstance(unit, str):
        raise ExpressionError("$dateSubtract needs a string unit")
    n = _date_int(amount)
    if n is None:
        raise ExpressionError(
            "$dateSubtract expects integer amount of time units",
            code=5166405,
            code_name="Location5166405",
        )
    return _shift_date(start, unit, -n)


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
    raw_bin = _eval(arg.get("binSize"), ctx) if "binSize" in arg else 1
    bin_size = _date_int(raw_bin)
    if bin_size is None:
        raise ExpressionError(
            f"$dateTrunc requires 'binSize' to be a 64-bit integer, but got value "
            f"'{_mongo_val_repr(raw_bin)}' of type {_bson_type_name(raw_bin)}",
            code=5439017,
            code_name="Location5439017",
        )
    if bin_size < 1:
        raise ExpressionError(
            f"$dateTrunc requires 'binSize' to be greater than 0, but got value {bin_size}",
            code=5439018,
            code_name="Location5439018",
        )
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
    raise ExpressionError(f"unknown time unit value: {unit}", code=9, code_name="FailedToParse")


def _op_date_to_parts(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateToParts requires a document spec")
    date = _coerce_extractor_date(_eval(arg.get("date"), ctx))
    if date is None:
        return None
    tz = _resolve_timezone(arg.get("timezone"))
    if tz is not None:
        # Naive input is treated as UTC (BSON Date semantics); shift into the zone
        # so the parts read local wall-clock — instant->wall-clock, unambiguous.
        date_aware = date if date.tzinfo is not None else date.replace(tzinfo=_dt.timezone.utc)
        date = date_aware.astimezone(tz)
    iso8601 = _eval(arg.get("iso8601"), ctx) if "iso8601" in arg else False
    if iso8601:
        iso_year, iso_week, iso_dow = date.isocalendar()
        return {
            "isoWeekYear": iso_year,
            "isoWeek": iso_week,
            "isoDayOfWeek": iso_dow,
            "hour": date.hour,
            "minute": date.minute,
            "second": date.second,
            "millisecond": date.microsecond // 1000,
        }
    return {
        "year": date.year,
        "month": date.month,
        "day": date.day,
        "hour": date.hour,
        "minute": date.minute,
        "second": date.second,
        "millisecond": date.microsecond // 1000,
    }


def _dfp_int(name: str, v: Any) -> int:
    """Coerce a `$dateFromParts` component to an int, matching mongod's
    `Location40515` for a non-integral value (an integral double like `6.0` is
    accepted; `6.5` / a string is not)."""
    if isinstance(v, bool):
        raise ExpressionError(
            f"'{name}' must evaluate to an integer, found {_nelem_render(v)}", code=40515
        )
    if isinstance(v, int):
        return int(v)
    if isinstance(v, float) and v.is_integer():
        return int(v)
    if isinstance(v, Decimal128) and ((dec := v.to_decimal()) == dec.to_integral_value()):
        return int(dec)
    raise ExpressionError(
        f"'{name}' must evaluate to an integer, found {_nelem_render(v)}", code=40515
    )


def _dfp_components(
    arg: Mapping[str, Any], ctx: _Ctx, spec: tuple[tuple[str, int | None], ...]
) -> dict[str, int] | None:
    """Evaluate the named `$dateFromParts` components, applying defaults and
    mongod's null-propagation (any null component → ``None``) and integral
    validation (``_dfp_int``)."""
    parts: dict[str, int] = {}
    for name, default in spec:
        if name in arg:
            v = _eval(arg[name], ctx)
            if v is None:
                return None  # null component -> null result
            parts[name] = _dfp_int(name, v)
        else:
            parts[name] = default  # type: ignore[assignment]
    return parts


def _dfp_calendar(arg: Mapping[str, Any], ctx: _Ctx) -> _dt.datetime | None:
    """Calendar (year/month/day) form of ``$dateFromParts`` — month carry, then
    day/time as a `timedelta` so out-of-range components roll over."""
    parts = _dfp_components(
        arg,
        ctx,
        (
            ("year", None),
            ("month", 1),
            ("day", 1),
            ("hour", 0),
            ("minute", 0),
            ("second", 0),
            ("millisecond", 0),
        ),
    )
    if parts is None:
        return None
    year = parts["year"]
    if not (1 <= year <= 9999):
        raise ExpressionError(f"'year' must be in the range 1 to 9999, found {year}", code=40523)
    total_months = year * 12 + (parts["month"] - 1)
    base_year, base_month0 = divmod(total_months, 12)
    if not (1 <= base_year <= 9999):
        raise ExpressionError(
            f"'year' must be in the range 1 to 9999, found {base_year}", code=40523
        )
    return _dt.datetime(base_year, base_month0 + 1, 1) + _dt.timedelta(
        days=parts["day"] - 1,
        hours=parts["hour"],
        minutes=parts["minute"],
        seconds=parts["second"],
        milliseconds=parts["millisecond"],
    )


def _dfp_iso(arg: Mapping[str, Any], ctx: _Ctx) -> _dt.datetime | None:
    """ISO-week (isoWeekYear/isoWeek/isoDayOfWeek) form of ``$dateFromParts``:
    start at the Monday of ISO week 1, then add (week-1) weeks + (day-1) days +
    the time components as a `timedelta` (so `isoWeek` 53 rolls into the next
    ISO year, exactly as mongod does)."""
    if "isoWeekYear" not in arg:
        raise ExpressionError(
            "$dateFromParts requires either 'year' or 'isoWeekYear' to be present",
            code=40516,
        )
    parts = _dfp_components(
        arg,
        ctx,
        (
            ("isoWeekYear", None),
            ("isoWeek", 1),
            ("isoDayOfWeek", 1),
            ("hour", 0),
            ("minute", 0),
            ("second", 0),
            ("millisecond", 0),
        ),
    )
    if parts is None:
        return None
    try:
        base = _dt.datetime.fromisocalendar(parts["isoWeekYear"], 1, 1)
    except ValueError as exc:
        raise ExpressionError(
            f"'isoWeekYear' must be in the range 1 to 9999, found {parts['isoWeekYear']}",
            code=40523,
        ) from exc
    return base + _dt.timedelta(
        weeks=parts["isoWeek"] - 1,
        days=parts["isoDayOfWeek"] - 1,
        hours=parts["hour"],
        minutes=parts["minute"],
        seconds=parts["second"],
        milliseconds=parts["millisecond"],
    )


def _op_ts_second(arg: Any, ctx: _Ctx) -> Any:
    """``$tsSecond``: the seconds field of a BSON Timestamp (as a long). Null /
    missing → null; a non-timestamp raises ``Location5687301``."""
    v = _eval(arg, ctx)
    if v is None:
        return None
    if not isinstance(v, bson.Timestamp):
        raise ExpressionError("Argument to $tsSecond must be a timestamp", code=5687301)
    return Int64(v.time)


def _op_ts_increment(arg: Any, ctx: _Ctx) -> Any:
    """``$tsIncrement``: the increment (ordinal) field of a BSON Timestamp (as a
    long). Null / missing → null; a non-timestamp raises ``Location5687302``."""
    v = _eval(arg, ctx)
    if v is None:
        return None
    if not isinstance(v, bson.Timestamp):
        raise ExpressionError("Argument to $tsIncrement must be a timestamp", code=5687302)
    return Int64(v.inc)


def _type_name(v: Any) -> str:
    """The BSON type string mongod's ``$type`` reports."""
    from bson import Binary, MaxKey, MinKey, ObjectId, Regex, Timestamp

    if v is None:
        return "null"
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, Int64):
        return "long"
    if isinstance(v, int):
        return "int" if -(2**31) <= v < 2**31 else "long"
    if isinstance(v, float):
        return "double"
    if isinstance(v, Decimal128):
        return "decimal"
    if isinstance(v, str):
        return "string"
    if isinstance(v, (bytes, Binary)):
        return "binData"
    if isinstance(v, ObjectId):
        return "objectId"
    if isinstance(v, _dt.datetime):
        return "date"
    if isinstance(v, Timestamp):
        return "timestamp"
    if isinstance(v, Regex):
        return "regex"
    if isinstance(v, MinKey):
        return "minKey"
    if isinstance(v, MaxKey):
        return "maxKey"
    if isinstance(v, list):
        return "array"
    return "object"


_TYPE_MISSING = object()


def _op_type(arg: Any, ctx: _Ctx) -> Any:
    """``$type``: the BSON type string of the argument. A field path that doesn't
    exist yields ``"missing"`` (mongod distinguishes an absent field from an
    explicit null)."""
    if (
        isinstance(arg, str)
        and arg.startswith("$")
        and not arg.startswith("$$")
        and get_path(dict(ctx.doc), arg[1:], default=_TYPE_MISSING) is _TYPE_MISSING
    ):
        return "missing"
    return _type_name(_eval(arg, ctx))


def _op_is_number(arg: Any, ctx: _Ctx) -> bool:
    """``$isNumber``: true for int / long / double / decimal (not bool)."""
    v = _eval(arg, ctx)
    return isinstance(v, (int, float, Decimal128)) and not isinstance(v, bool)


def _op_is_array(arg: Any, ctx: _Ctx) -> bool:
    """``$isArray``: true iff the argument is an array."""
    return isinstance(_eval(arg, ctx), list)


def _strcasecmp_coerce(v: Any) -> str:
    """Coerce a `$strcasecmp` operand to a string the way mongod does: null →
    the empty string, a string stays, and any other value is `$toString`-coerced
    (numbers → their string form, dates → their string form). A bool is the one
    type mongod refuses to coerce → Location16007."""
    if v is None:
        return ""
    if isinstance(v, str):
        return v
    if isinstance(v, bool):
        raise ExpressionError(
            "$strcasecmp only takes strings and numbers, not bool",
            code=16007,
            code_name="Location16007",
        )
    return _convert_value(v, "string")


def _op_strcasecmp(arg: Any, ctx: _Ctx) -> int:
    """``$strcasecmp``: case-insensitive comparison of two operands → -1 / 0 / 1.
    mongod coerces each operand to a string (null → ""), rejecting only bool."""
    vals = _eval_args(arg, ctx)
    if len(vals) != 2:
        raise ExpressionError("$strcasecmp requires two arguments")
    au, bu = _strcasecmp_coerce(vals[0]).upper(), _strcasecmp_coerce(vals[1]).upper()
    return -1 if au < bu else (1 if au > bu else 0)


def _op_replace(arg: Any, ctx: _Ctx, *, count: int) -> Any:
    """``$replaceOne`` (count 1) / ``$replaceAll`` (count -1): replace occurrence(s)
    of ``find`` in ``input`` with ``replacement``. Any null input/find/replacement
    → null; a non-string one raises mongod's per-argument code (input → 51746,
    find → 51745, replacement → 51744)."""
    op = "$replaceOne" if count == 1 else "$replaceAll"
    if not isinstance(arg, Mapping) or not {"input", "find", "replacement"} <= set(arg):
        raise ExpressionError(f"{op} requires 'input', 'find' and 'replacement'")
    inp = _eval(arg["input"], ctx)
    find = _eval(arg["find"], ctx)
    rep = _eval(arg["replacement"], ctx)
    if inp is None or find is None or rep is None:
        return None
    for v, name, code in (
        (inp, "input", 51746),
        (find, "find", 51745),
        (rep, "replacement", 51744),
    ):
        if not isinstance(v, str):
            raise ExpressionError(f"{op} requires that '{name}' be a string, found: {v}", code=code)
    return inp.replace(find, rep) if count == -1 else inp.replace(find, rep, 1)


def _op_replace_one(arg: Any, ctx: _Ctx) -> Any:
    return _op_replace(arg, ctx, count=1)


def _op_replace_all(arg: Any, ctx: _Ctx) -> Any:
    return _op_replace(arg, ctx, count=-1)


def _op_date_from_parts(arg: Any, ctx: _Ctx) -> Any:
    """``$dateFromParts``: build a date from calendar components. Components default
    to month/day = 1 and hour/minute/second/millisecond = 0; out-of-range values
    roll over (month 13 -> next January, day 0 -> last day of the previous month,
    etc.) exactly as mongod does. Any null component yields null. ``year`` is
    required (1-9999); a non-integral component is ``Location40515``, a missing
    ``year`` is ``Location40516``, an out-of-range ``year`` is ``Location40523``.
    A ``timezone`` interprets the components as local time in that zone
    (local->instant). Two forms: the calendar form above and the **ISO-week** form
    (``isoWeekYear`` + optional ``isoWeek`` / ``isoDayOfWeek``, both defaulting to
    1). Verified against mongod 6.0 via a three-way probe."""
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateFromParts requires a document spec")
    is_iso = "isoWeekYear" in arg or "isoWeek" in arg or "isoDayOfWeek" in arg
    if is_iso:
        result = _dfp_iso(arg, ctx)
        if result is None:
            return None
    else:
        if "year" not in arg:
            raise ExpressionError(
                "$dateFromParts requires either 'year' or 'isoWeekYear' to be present",
                code=40516,
            )
        result = _dfp_calendar(arg, ctx)
        if result is None:
            return None
    tz = _resolve_timezone(arg.get("timezone"))
    if tz is not None:
        # The components are local time in `tz`; convert to the UTC instant.
        result = result.replace(tzinfo=tz).astimezone(_dt.timezone.utc).replace(tzinfo=None)
    return result


def _op_date_diff(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateDiff requires a document spec")
    # A missing required *parameter* (key absent) is a mongod parse error, distinct
    # from a present parameter that evaluates to null (which yields null).
    for param, code in (("startDate", 5166303), ("endDate", 5166304), ("unit", 5166305)):
        if param not in arg:
            raise ExpressionError(
                f"Missing '{param}' parameter to $dateDiff", code=code, code_name=f"Location{code}"
            )
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
    raise ExpressionError(f"unknown time unit value: {unit}", code=9, code_name="FailedToParse")


def _op_regex_find_all(arg: Any, ctx: _Ctx) -> Any:
    import re as _re

    if not isinstance(arg, Mapping):
        raise ExpressionError("$regexFindAll requires {input, regex, options?}")
    s = _eval(arg.get("input"), ctx)
    if s is None:
        return []
    if not isinstance(s, str):
        raise ExpressionError(
            "$regexFindAll needs 'input' to be of type string",
            code=51104,
            code_name="Location51104",
        )
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
        raise ExpressionError(
            f"$arrayToObject requires an array input, found: {_bson_type_name(v)}",
            code=40386,
            code_name="Location40386",
        )
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
    # mongod: exactly 2 args (16020); a null string/separator -> null; a
    # non-string first/second arg -> 40085/40086; an empty separator -> 40087.
    if not isinstance(arg, list) or len(arg) != 2:
        n = len(arg) if isinstance(arg, list) else 1
        raise ExpressionError(
            f"Expression $split takes exactly 2 arguments. {n} were passed in.",
            code=16020,
            code_name="Location16020",
        )
    s = _eval(arg[0], ctx)
    sep = _eval(arg[1], ctx)
    if s is None or sep is None:
        return None
    if not isinstance(s, str):
        raise ExpressionError(
            "$split requires an expression that evaluates to a string as a first "
            f"argument, found: {_bson_type_name(s)}",
            code=40085,
            code_name="Location40085",
        )
    if not isinstance(sep, str):
        raise ExpressionError(
            "$split requires an expression that evaluates to a string as a second "
            f"argument, found: {_bson_type_name(sep)}",
            code=40086,
            code_name="Location40086",
        )
    if sep == "":
        raise ExpressionError(
            "$split requires a non-empty separator", code=40087, code_name="Location40087"
        )
    return s.split(sep)


def _mongo_val_repr(v: Any) -> str:
    """The way mongod renders a value in a "got <v> (of type <t>)" message."""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, str):
        return f'"{v}"'
    return str(v)


def _trim_impl(op: str, side: str, arg: Any, ctx: _Ctx) -> Any:
    # mongod: input must be a string (50699) — a null / missing input yields null;
    # if `chars` is present it must be a string (50700), and a null `chars` yields
    # a null result (not the whitespace default).
    if not isinstance(arg, Mapping):
        raise ExpressionError(f"{op} requires {{input, chars?}}")
    s = _eval(arg.get("input"), ctx)
    if s is None:
        return None
    if not isinstance(s, str):
        raise ExpressionError(
            f"{op} requires its input to be a string, got {_mongo_val_repr(s)} "
            f"(of type {_bson_type_name(s)})",
            code=50699,
            code_name="Location50699",
        )
    chars: Any = None
    if "chars" in arg:
        chars = _eval(arg["chars"], ctx)
        if chars is None:
            return None
        if not isinstance(chars, str):
            raise ExpressionError(
                f"{op} requires 'chars' to be a string, got {_mongo_val_repr(chars)} "
                f"(of type {_bson_type_name(chars)})",
                code=50700,
                code_name="Location50700",
            )
    if side == "l":
        return s.lstrip(chars) if chars else s.lstrip()
    if side == "r":
        return s.rstrip(chars) if chars else s.rstrip()
    return s.strip(chars) if chars else s.strip()


def _op_trim(arg: Any, ctx: _Ctx) -> Any:
    return _trim_impl("$trim", "b", arg, ctx)


def _op_ltrim(arg: Any, ctx: _Ctx) -> Any:
    return _trim_impl("$ltrim", "l", arg, ctx)


def _op_rtrim(arg: Any, ctx: _Ctx) -> Any:
    return _trim_impl("$rtrim", "r", arg, ctx)


def _op_substr_cp(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) != 3:
        raise ExpressionError("$substrCP requires [string, start, length]")
    s = _eval(arg[0], ctx)
    start = _eval(arg[1], ctx)
    length = _eval(arg[2], ctx)
    if s is None:
        return ""
    if isinstance(start, bool):
        raise ExpressionError(
            "$substrCP: starting index must be a numeric type (is BSON type bool)",
            code=34450,
        )
    if isinstance(length, bool):
        raise ExpressionError(
            "$substrCP: length must be a numeric type (is BSON type bool)",
            code=34452,
        )
    try:
        start = _int_index(start)
    except _FractionalIndex:
        raise ExpressionError(
            "$substrCP: starting index cannot be represented as a 32-bit "
            f"integral value: {_fmt_double(start)}",
            code=34451,
        ) from None
    try:
        length = _int_index(length)
    except _FractionalIndex:
        raise ExpressionError(
            "$substrCP: length cannot be represented as a 32-bit integral "
            f"value: {_fmt_double(length)}",
            code=34453,
        ) from None
    if not isinstance(s, str) or not isinstance(start, int) or not isinstance(length, int):
        raise ExpressionError("$substrCP requires string + ints")
    # Unlike $substrBytes, mongod rejects a negative start *and* a negative
    # length for $substrCP (distinct codes/messages, verbatim).
    if start < 0:
        raise ExpressionError(
            "$substrCP: the starting index must be nonnegative integer.",
            code=34455,
        )
    if length < 0:
        raise ExpressionError(
            "$substrCP: length must be a nonnegative integer.",
            code=34454,
        )
    return s[start : start + length]


def _op_str_len_cp(arg: Any, ctx: _Ctx) -> Any:
    s = _eval(arg, ctx)
    if not isinstance(s, str):
        raise ExpressionError(
            f"$strLenCP requires a string argument, found: {_bson_type_name(s)}",
            code=34471,
            code_name="Location34471",
        )
    return len(s)


def _index_of_pos(op: str, which: str, v: Any) -> int:
    """Validate a ``$indexOf*`` start / end index. mongod accepts an int or whole
    double; a fractional double / bool / non-numeric is Location40096 (note its
    verbatim missing space after the operator name), and a negative index is
    Location40097."""
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise ExpressionError(
            f"{op}requires an integral {which} index, found a value of type: "
            f"{_bson_type_name(v)}, with value: {_mongo_val_repr(v)}",
            code=40096,
            code_name="Location40096",
        )
    if isinstance(v, float):
        if not v.is_integer():
            raise ExpressionError(
                f"{op}requires an integral {which} index, found a value of type: "
                f"{_bson_type_name(v)}, with value: {_mongo_val_repr(v)}",
                code=40096,
                code_name="Location40096",
            )
        v = int(v)
    if v < 0:
        raise ExpressionError(
            f"{op} requires a nonnegative {which} index, found: {v}",
            code=40097,
            code_name="Location40097",
        )
    return v


def _op_index_of_cp(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or not 2 <= len(arg) <= 4:
        raise ExpressionError("$indexOfCP requires [string, search, start?, end?]")
    s = _eval(arg[0], ctx)
    needle = _eval(arg[1], ctx)
    if s is None:
        return None
    if not isinstance(s, str) or not isinstance(needle, str):
        raise ExpressionError("$indexOfCP requires string operands")
    start = _index_of_pos("$indexOfCP", "starting", _eval(arg[2], ctx)) if len(arg) >= 3 else 0
    end = _index_of_pos("$indexOfCP", "ending", _eval(arg[3], ctx)) if len(arg) >= 4 else len(s)
    return s.find(needle, start, end)


def _op_index_of_bytes(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or not 2 <= len(arg) <= 4:
        raise ExpressionError("$indexOfBytes requires [string, search, start?, end?]")
    s = _eval(arg[0], ctx)
    needle = _eval(arg[1], ctx)
    if s is None:
        return None
    if not isinstance(s, str):
        raise ExpressionError(
            f"$indexOfBytes requires a string as the first argument, found: {_bson_type_name(s)}",
            code=40091,
            code_name="Location40091",
        )
    if not isinstance(needle, str):
        raise ExpressionError(
            "$indexOfBytes requires a string as the second argument, found: "
            f"{_bson_type_name(needle)}",
            code=40092,
            code_name="Location40092",
        )
    start = _index_of_pos("$indexOfBytes", "starting", _eval(arg[2], ctx)) if len(arg) >= 3 else 0
    haystack = s.encode("utf-8")
    end = (
        _index_of_pos("$indexOfBytes", "ending", _eval(arg[3], ctx))
        if len(arg) >= 4
        else len(haystack)
    )
    needle_b = needle.encode("utf-8")
    return haystack.find(needle_b, start, end)


def _op_str_len_bytes(arg: Any, ctx: _Ctx) -> Any:
    s = _eval(arg, ctx)
    if not isinstance(s, str):
        raise ExpressionError(
            f"$strLenBytes requires a string argument, found: {_bson_type_name(s)}",
            code=34473,
            code_name="Location34473",
        )
    return len(s.encode("utf-8"))


def _op_substr_bytes(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) != 3:
        raise ExpressionError("$substrBytes requires [string, start, length]")
    s = _eval(arg[0], ctx)
    start = _eval(arg[1], ctx)
    length = _eval(arg[2], ctx)
    if s is None:
        return ""
    # mongod's message has a verbatim double space after "$substrBytes:".
    if isinstance(start, bool):
        raise ExpressionError(
            "$substrBytes:  starting index must be a numeric type (is BSON type bool)",
            code=16034,
        )
    if isinstance(length, bool):
        raise ExpressionError(
            "$substrBytes:  length must be a numeric type (is BSON type bool)",
            code=16035,
        )
    # Unlike $substrCP (which rejects a fractional double), mongod's $substrBytes
    # accepts any double and truncates toward zero (1.7 -> 1, -1.7 -> -1, then the
    # negative-start check below rejects it). Non-finite falls through to the
    # generic type error.
    if isinstance(start, float) and math.isfinite(start):
        start = int(start)
    if isinstance(length, float) and math.isfinite(length):
        length = int(length)
    if not isinstance(s, str) or not isinstance(start, int) or not isinstance(length, int):
        raise ExpressionError("$substrBytes requires string + ints")
    encoded = s.encode("utf-8")
    n = len(encoded)
    # mongod rejects a negative start (a negative length is fine — it means
    # "to the end"). Message includes the value, with a verbatim double space.
    if start < 0:
        raise ExpressionError(
            f"$substrBytes:  starting index must be non-negative (got: {start})",
            code=50752,
        )
    # mongod rejects a byte range that splits a UTF-8 character rather than
    # returning a replacement char (verbatim double-space messages).
    if start < n and (encoded[start] & 0xC0) == 0x80:
        raise ExpressionError(
            "$substrBytes:  Invalid range, starting index is a UTF-8 continuation byte.",
            code=28656,
        )
    end = n if length < 0 else start + length
    if 0 <= end < n and (encoded[end] & 0xC0) == 0x80:
        raise ExpressionError(
            "$substrBytes:  Invalid range, ending index is in the middle of a UTF-8 character.",
            code=28657,
        )
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
        raise ExpressionError(
            f"$indexOfArray requires an array as a first argument, found: {_bson_type_name(arr)}",
            code=40090,
            code_name="Location40090",
        )
    needle = _eval(arg[1], ctx)
    start = _eval(arg[2], ctx) if len(arg) >= 3 else 0
    end = _eval(arg[3], ctx) if len(arg) >= 4 else len(arr)
    # mongod's message text is verbatim, including the missing space in
    # "$indexOfArrayrequires" (a real mongod quirk) so a surrogate matches.
    if isinstance(start, bool):
        raise ExpressionError(
            "$indexOfArrayrequires an integral starting index, found a value of "
            f"type: bool, with value: {'true' if start else 'false'}",
            code=40096,
        )
    if isinstance(end, bool):
        raise ExpressionError(
            "$indexOfArrayrequires an integral ending index, found a value of "
            f"type: bool, with value: {'true' if end else 'false'}",
            code=40096,
        )
    try:
        start = _int_index(start)
    except _FractionalIndex:
        raise ExpressionError(
            "$indexOfArrayrequires an integral starting index, found a value of "
            f"type: double, with value: {_fmt_double(start)}",
            code=40096,
        ) from None
    try:
        end = _int_index(end)
    except _FractionalIndex:
        raise ExpressionError(
            "$indexOfArrayrequires an integral ending index, found a value of "
            f"type: double, with value: {_fmt_double(end)}",
            code=40096,
        ) from None
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
    # Per-arg bool rejection with mongod's exact codes/messages (the step
    # message's "type:bool" missing space is verbatim from mongod).
    if isinstance(start, bool) or not isinstance(start, (int, float)):
        raise ExpressionError(
            "$range requires a numeric starting value, found value of type: "
            f"{_bson_type_name(start)}",
            code=34443,
        )
    if isinstance(end, bool) or not isinstance(end, (int, float)):
        raise ExpressionError(
            f"$range requires a numeric ending value, found value of type: {_bson_type_name(end)}",
            code=34445,
        )
    if isinstance(step, bool) or not isinstance(step, (int, float)):
        raise ExpressionError(
            f"$range requires a numeric step value, found value of type:{_bson_type_name(step)}",
            code=34447,
        )
    # A whole-number double is accepted (coerced to int); a fractional one is
    # rejected with mongod's per-arg "32-bit integer" code.
    try:
        start = _int_index(start)
    except _FractionalIndex:
        raise ExpressionError(
            "$range requires a starting value that can be represented as a "
            f"32-bit integer, found value: {_fmt_double(start)}",
            code=34444,
        ) from None
    try:
        end = _int_index(end)
    except _FractionalIndex:
        raise ExpressionError(
            "$range requires an ending value that can be represented as a "
            f"32-bit integer, found value: {_fmt_double(end)}",
            code=34446,
        ) from None
    try:
        step = _int_index(step)
    except _FractionalIndex:
        raise ExpressionError(
            "$range requires a step value that can be represented as a 32-bit "
            f"integer, found value: {_fmt_double(step)}",
            code=34448,
        ) from None
    if not all(isinstance(v, int) for v in (start, end, step)):
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
    if not isinstance(inputs, list):
        raise ExpressionError(
            f"inputs must be an array of expressions, found {_bson_type_name(inputs)}",
            code=34461,
            code_name="Location34461",
        )
    for a in inputs:
        if not isinstance(a, list):
            raise ExpressionError(
                f"$zip found a non-array expression in input: {a}",
                code=34468,
                code_name="Location34468",
            )
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
        raise ExpressionError(
            "The input argument to $sortArray must be an array, but was of type: "
            f"{_bson_type_name(arr)}",
            code=2942504,
            code_name="Location2942504",
        )
    sort_by = arg["sortBy"]
    if isinstance(sort_by, bool):
        raise ExpressionError(
            "The $sort is invalid: use 1/-1 to sort the whole element, or "
            "{field:1/-1} to sort embedded fields",
            code=2942507,
        )
    if isinstance(sort_by, int):
        return sorted(arr, reverse=(sort_by == -1))
    if not isinstance(sort_by, Mapping):
        raise ExpressionError(
            "The $sort is invalid: use 1/-1 to sort the whole element, or "
            "{field:1/-1} to sort embedded fields",
            code=2942507,
            code_name="Location2942507",
        )

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


def _coerce_extractor_date(value: Any) -> _dt.datetime | None:
    """A date-extractor operand (`$year`/`$dayOfYear`/…) must be a Date, null, or a
    missing field. mongod raises ``Location16006`` on any other present value (a
    string, a number, …); null / missing yield null."""
    if isinstance(value, _dt.datetime):
        return value
    if value is None:
        return None
    raise ExpressionError(f"can't convert from BSON type {_type_name(value)} to Date", code=16006)


def _date_operand(arg: Any, ctx: _Ctx) -> _dt.datetime | None:
    """Resolve a date-extractor operand (`$year`/`$hour`/…) to a `datetime` or
    `None`. mongod accepts two forms:

      * a bare date expression (`"$field"`, `{$dateFromParts: …}`, …), or
      * a `{date: <expr>, timezone: <expr>}` object that shifts the instant into a
        timezone before the component is read.

    The object form is detected as a document carrying a ``date`` key that is not
    itself an operator expression (`{$op: …}`). A `timezone` (fixed-offset or named
    IANA zone) re-expresses the instant in that zone (naive input treated as UTC,
    matching BSON Date semantics) so the returned `datetime`'s wall-clock fields are
    local — exactly like `$dateToString`'s `timezone`. Absent/`None` timezone leaves
    the instant in UTC."""
    if (
        isinstance(arg, Mapping)
        and "date" in arg
        and not (len(arg) == 1 and next(iter(arg)).startswith("$"))
    ):
        d = _coerce_extractor_date(_eval(arg["date"], ctx))
        if d is None:
            return None
        tz = _resolve_timezone(arg.get("timezone"))
        if tz is not None:
            d_aware = d if d.tzinfo is not None else d.replace(tzinfo=_dt.timezone.utc)
            d = d_aware.astimezone(tz)
        return d
    return _coerce_extractor_date(_eval(arg, ctx))


def _op_year(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.year if d is not None else None


def _op_month(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.month if d is not None else None


def _op_day_of_month(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.day if d is not None else None


def _op_day_of_week(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return (d.isoweekday() % 7) + 1 if d is not None else None


def _op_hour(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.hour if d is not None else None


def _op_minute(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.minute if d is not None else None


def _op_second(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.second if d is not None else None


def _op_millisecond(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.microsecond // 1000 if d is not None else None


def _op_day_of_year(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.timetuple().tm_yday if d is not None else None


def _us_week(d: _dt.datetime) -> int:
    """US week number (mongod ``$week``): weeks start Sunday, 0-53; week 0 is the
    days before the year's first Sunday. Equivalent to ``%U`` (strftime)."""
    yday = d.timetuple().tm_yday  # 1-366
    # Weekday of Jan 1 with Sunday=0 .. Saturday=6.
    jan1_wday_sun0 = (_dt.date(d.year, 1, 1).weekday() + 1) % 7
    # Days from Jan 1 to the year's first Sunday (0 if Jan 1 is a Sunday).
    days_to_first_sunday = (7 - jan1_wday_sun0) % 7
    if yday <= days_to_first_sunday:
        return 0
    return (yday - days_to_first_sunday - 1) // 7 + 1


def _op_week(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return _us_week(d) if d is not None else None


def _op_iso_week(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.isocalendar()[1] if d is not None else None


def _op_iso_day_of_week(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.isocalendar()[2] if d is not None else None


def _op_iso_week_year(arg: Any, ctx: _Ctx) -> Any:
    d = _date_operand(arg, ctx)
    return d.isocalendar()[0] if d is not None else None


def _resolve_timezone(name: Any) -> _dt.tzinfo | None:
    """Resolve MongoDB-style timezone strings to a Python ``tzinfo``.

    Accepts IANA names ("Europe/Dublin"), UTC offsets ("+05:30",
    "-04:00", "+0530"), and the aliases "GMT" / "UTC". ``None`` yields
    ``None`` (caller treats input as already in its own zone).
    """
    if name is None:
        return None
    if not isinstance(name, str):
        # mongod: Location40517 "timezone must evaluate to a string, found <type>"
        # (verified via a three-way probe against mongod 6.0).
        raise ExpressionError(
            f"timezone must evaluate to a string, found {_bson_type_name(name)}",
            code=40517,
        )
    if name in ("UTC", "GMT", "Etc/UTC", "Etc/GMT"):
        return _dt.timezone.utc
    if name and name[0] in ("+", "-"):
        sign = 1 if name[0] == "+" else -1
        digits = name[1:].replace(":", "")
        if len(digits) == 4 and digits.isdigit():
            hours = int(digits[:2])
            minutes = int(digits[2:])
            return _dt.timezone(sign * _dt.timedelta(hours=hours, minutes=minutes))
        raise ExpressionError(f'unrecognized time zone identifier: "{name}"', code=40485)
    try:
        return zoneinfo.ZoneInfo(name)
    except zoneinfo.ZoneInfoNotFoundError as exc:
        # mongod: Location40485 "unrecognized time zone identifier: \"<name>\""
        raise ExpressionError(f'unrecognized time zone identifier: "{name}"', code=40485) from exc


def _op_date_from_string(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateFromString requires a document spec")
    raw = _eval(arg.get("dateString"), ctx)
    if raw is None:
        return _eval(arg["onNull"], ctx) if "onNull" in arg else None
    if not isinstance(raw, str):
        raise ExpressionError(
            "$dateFromString requires that 'dateString' be a string, found: "
            f"{_bson_type_name(raw)}",
            code=241,
            code_name="ConversionFailure",
        )
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
    # A non-date, non-null 'date' is mongod Location16006 (was silently null).
    d = _coerce_extractor_date(_eval(arg["date"], ctx))
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
    if isinstance(idx, bool):
        raise ExpressionError(
            "$arrayElemAt's second argument must be a numeric value, but is bool",
            code=28690,
        )
    try:
        idx = _int_index(idx)
    except _FractionalIndex:
        raise ExpressionError(
            "$arrayElemAt's second argument must be representable as a 32-bit "
            f"integer: {_fmt_double(idx)}",
            code=28691,
        ) from None
    _reject_non_array(
        arr, f"$arrayElemAt's first argument must be an array, but is {_bson_type_name(arr)}", 28689
    )
    if not isinstance(arr, list) or not isinstance(idx, int):
        # A missing / null input array really is null — probed against mongod
        # 6.0.16: `{$arrayElemAt: ["$nope", 0]}` projects `r: null`.
        return None
    if -len(arr) <= idx < len(arr):
        return arr[idx]
    # An in-bounds array with an out-of-range index evaluates to MISSING, not
    # null, so `$project` omits the field entirely. mongod on `[1, 2]`:
    # index 9 and index -9 both yield `{_id: 1}` with no `r` at all, while
    # index 0 yields `r: 1`. We returned null, which added a field mongod does
    # not send.
    return MISSING


def _reject_non_array(v: Any, message: str, code: int) -> None:
    """mongod's 'input must be an array' guard for the array operators. A null /
    missing value is the caller's concern (returns null); a non-array, non-null
    value raises the operator's specific Location error instead of silently
    yielding null."""
    if v is not None and not isinstance(v, list):
        raise ExpressionError(message, code=code, code_name=f"Location{code}")


def _op_first(arg: Any, ctx: _Ctx) -> Any:
    arr = _eval(arg, ctx)
    _reject_non_array(
        arr, f"$first's argument must be an array, but is {_bson_type_name(arr)}", 28689
    )
    return arr[0] if isinstance(arr, list) and arr else None


def _op_last(arg: Any, ctx: _Ctx) -> Any:
    arr = _eval(arg, ctx)
    _reject_non_array(
        arr, f"$last's argument must be an array, but is {_bson_type_name(arr)}", 28689
    )
    return arr[-1] if isinstance(arr, list) and arr else None


def _nelem_render(v: Any) -> str:
    """Render a value the way mongod does in the "found <v>" tail of an ``n``
    type error — strings are quoted, other scalars stringified."""
    if isinstance(v, str):
        return f'"{v}"'
    return str(v)


def nelem_parse_n(n_val: Any) -> int:
    """Validate an already-evaluated ``n`` for the N-element operators, matching
    mongod's error codes (verified against mongod 6.0): a non-integral number is
    ``Location5787903``, a non-numeric is ``Location5787902``, and ``n <= 0`` is
    ``Location5787908``. An integral double (``2.0``) is accepted. Shared by the
    expression forms (``_nelem_n_and_input``) and the ``$group`` accumulator forms
    (``aggregate._acc_nelem``)."""
    if isinstance(n_val, bool):
        raise ExpressionError(
            f"Value for 'n' must be of integral type, but found {_nelem_render(n_val)}",
            code=5787902,
        )
    if isinstance(n_val, int) or (isinstance(n_val, float) and n_val.is_integer()):
        n = int(n_val)
    elif isinstance(n_val, Decimal128) and ((dec := n_val.to_decimal()) == dec.to_integral_value()):
        n = int(dec)
    elif isinstance(n_val, (float, Decimal128)):
        raise ExpressionError(
            f"Value for 'n' must be of integral type, but found {_nelem_render(n_val)}",
            code=5787903,
        )
    else:
        raise ExpressionError(
            f"Value for 'n' must be of integral type, but found {_nelem_render(n_val)}",
            code=5787902,
        )
    if n <= 0:
        raise ExpressionError(f"'n' must be greater than 0, found {n}", code=5787908)
    return n


def _nelem_n_and_input(arg: Any, ctx: _Ctx) -> tuple[int, list[Any]]:
    """Validate and evaluate the ``{n, input}`` spec shared by ``$firstN`` /
    ``$lastN`` / ``$maxN`` / ``$minN`` (expression form), matching mongod's error
    codes exactly (verified against mongod 6.0): a missing ``n`` / ``input`` is
    ``Location5787906`` / ``Location5787907``; ``n`` validation is
    ``nelem_parse_n``; and a null / missing / non-array ``input`` is
    ``Location5788200`` — mongod does **not** treat a null input as null here, it
    raises."""
    if not isinstance(arg, Mapping) or "n" not in arg:
        raise ExpressionError("Missing value for 'n'", code=5787906)
    if "input" not in arg:
        raise ExpressionError("Missing value for 'input'", code=5787907)
    n = nelem_parse_n(_eval(arg["n"], ctx))
    arr = _eval(arg["input"], ctx)
    if not isinstance(arr, list):
        raise ExpressionError("Input must be an array", code=5788200)
    return n, arr


def _first_last_n(arg: Any, ctx: _Ctx, *, first: bool) -> Any:
    """``$firstN`` / ``$lastN`` (expression form): the first / last ``n`` elements
    of an array. When the array has fewer than ``n`` elements the whole array is
    returned. Validation (``n`` / ``input``) matches mongod — see
    ``_nelem_n_and_input``."""
    n, arr = _nelem_n_and_input(arg, ctx)
    return arr[:n] if first else arr[-n:]


def _op_first_n(arg: Any, ctx: _Ctx) -> Any:
    return _first_last_n(arg, ctx, first=True)


def _op_last_n(arg: Any, ctx: _Ctx) -> Any:
    return _first_last_n(arg, ctx, first=False)


def _max_min_n(arg: Any, ctx: _Ctx, *, largest: bool) -> Any:
    """``$maxN`` / ``$minN`` (expression form): the ``n`` largest / smallest
    elements of an array, by MongoDB's cross-type BSON order. Null (and missing)
    *elements* are ignored (mongod does not consider them); the result is in
    descending order for ``$maxN`` and ascending for ``$minN``. Fewer than ``n``
    non-null values returns all of them. Validation matches mongod — see
    ``_nelem_n_and_input`` (a null / non-array ``input`` raises, unlike the
    elements)."""
    n, arr = _nelem_n_and_input(arg, ctx)
    from secantus.ordering import _SortKey

    non_null = [x for x in arr if x is not None]
    non_null.sort(key=_SortKey, reverse=largest)
    return non_null[:n]


def _op_max_n(arg: Any, ctx: _Ctx) -> Any:
    return _max_min_n(arg, ctx, largest=True)


def _op_min_n(arg: Any, ctx: _Ctx) -> Any:
    return _max_min_n(arg, ctx, largest=False)


def _op_slice(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or len(arg) not in (2, 3):
        raise ExpressionError("$slice requires [array, n] or [array, position, n]")
    arr = _eval(arg[0], ctx)
    _reject_non_array(
        arr, f"First argument to $slice must be an array, but is {_bson_type_name(arr)}", 28724
    )
    if not isinstance(arr, list):
        return None
    if len(arg) == 2:
        n = _eval(arg[1], ctx)
        if isinstance(n, bool):
            raise ExpressionError(
                "Second argument to $slice must be a numeric value, but is of type: bool",
                code=28725,
            )
        try:
            n = _int_index(n)
        except _FractionalIndex:
            raise ExpressionError(
                "Second argument to $slice can't be represented as a 32-bit "
                f"integer: {_fmt_double(n)}",
                code=28726,
            ) from None
        if not isinstance(n, int):
            return None
        return arr[:n] if n >= 0 else arr[n:]
    position = _eval(arg[1], ctx)
    n = _eval(arg[2], ctx)
    if isinstance(position, bool):
        raise ExpressionError(
            "Second argument to $slice must be a numeric value, but is of type: bool",
            code=28725,
        )
    if isinstance(n, bool):
        raise ExpressionError(
            "Third argument to $slice must be numeric, but is of type: bool",
            code=28727,
        )
    try:
        position = _int_index(position)
    except _FractionalIndex:
        raise ExpressionError(
            "Second argument to $slice can't be represented as a 32-bit "
            f"integer: {_fmt_double(position)}",
            code=28726,
        ) from None
    try:
        n = _int_index(n)
    except _FractionalIndex:
        raise ExpressionError(
            f"Third argument to $slice can't be represented as a 32-bit integer: {_fmt_double(n)}",
            code=28728,
        ) from None
    if not isinstance(position, int) or not isinstance(n, int):
        return None
    return arr[position : position + n]


def _op_concat_arrays(arg: Any, ctx: _Ctx) -> Any:
    out: list[Any] = []
    for a in arg:
        p = _eval(a, ctx)
        if p is None:
            return None  # a null / missing operand -> null result
        _reject_non_array(p, f"$concatArrays only supports arrays, not {_bson_type_name(p)}", 28664)
        out.extend(p)
    return out


def _op_reverse_array(arg: Any, ctx: _Ctx) -> Any:
    arr = _eval(arg, ctx)
    _reject_non_array(
        arr,
        f"The argument to $reverseArray must be an array, but was of type: {_bson_type_name(arr)}",
        34435,
    )
    return list(reversed(arr)) if isinstance(arr, list) else None


def _op_in(arg: Any, ctx: _Ctx) -> bool:
    needle, haystack = _eval(arg[0], ctx), _eval(arg[1], ctx)
    if not isinstance(haystack, list):
        raise ExpressionError(
            f"$in requires an array as a second argument, found: {_bson_type_name(haystack)}",
            code=40081,
            code_name="Location40081",
        )
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
        # mongod routes $toInt/$toLong/$convert(int/long) through $convert and
        # reports an unparseable string as ConversionFailure (241), not the
        # generic TypeMismatch (14).
        raise ExpressionError(
            f"Failed to parse number {value!r} in {op_name} with no onError value",
            code=241,
            code_name="ConversionFailure",
        ) from exc


_INT32_MIN, _INT32_MAX = -(2**31), 2**31 - 1
_INT64_MIN, _INT64_MAX = -(2**63), 2**63 - 1
_OVERFLOW_MSG = "Conversion would overflow target type in $convert"


def _op_to_int(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    if value is None:
        return None
    if isinstance(value, bool):
        return 1 if value else 0
    if isinstance(value, int):
        result = int(value)  # strip Int64 -> plain int (int32 on the wire)
    elif isinstance(value, float):
        if not math.isfinite(value):
            raise ExpressionError(_OVERFLOW_MSG, code=241)
        result = int(value)  # truncates toward zero
    elif isinstance(value, Decimal128):
        result = int(value.to_decimal())
    elif isinstance(value, str):
        result = _safe_int_from_str(value, "$toInt")
    else:
        raise ExpressionError(f"$toInt cannot convert {type(value).__name__}")
    # mongod: an int32 target must fit [-2^31, 2^31-1], else overflow (241).
    if not _INT32_MIN <= result <= _INT32_MAX:
        raise ExpressionError(_OVERFLOW_MSG, code=241)
    return result


def _op_to_long(arg: Any, ctx: _Ctx) -> Any:
    # Mirrors `_op_to_int` but targets int64: a double truncates toward zero, a
    # string parses, bool -> 0/1, and the result is wrapped as `Int64` so it
    # renders as `$type: "long"`. An out-of-[-2^63, 2^63-1] result overflows (241).
    value = _eval(arg, ctx)
    if value is None:
        return None
    if isinstance(value, bool):
        return Int64(1 if value else 0)
    if isinstance(value, int):
        result = int(value)
    elif isinstance(value, float):
        if not math.isfinite(value):
            raise ExpressionError(_OVERFLOW_MSG, code=241)
        result = int(value)  # truncates toward zero
    elif isinstance(value, Decimal128):
        result = int(value.to_decimal())
    elif isinstance(value, str):
        result = _safe_int_from_str(value, "$toLong")
    else:
        raise ExpressionError(f"$toLong cannot convert {type(value).__name__}")
    if not _INT64_MIN <= result <= _INT64_MAX:
        raise ExpressionError(_OVERFLOW_MSG, code=241)
    return Int64(result)


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
            raise ExpressionError(
                f"Failed to parse number {value!r} in $convert with no onError value",
                code=241,
                code_name="ConversionFailure",
            ) from exc
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
        raise ExpressionError(f"Unknown type name: {target}", code=2)
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
        # mongod rejects bool -> date (no int coercion): ConversionFailure (241).
        if isinstance(value, bool):
            raise ExpressionError(
                "Unsupported conversion from bool to date in $convert with no onError value",
                code=241,
                code_name="ConversionFailure",
            )
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
            # int32 (16) / int64 (18) targets range-check like mongod (241).
            lo, hi = (_INT64_MIN, _INT64_MAX) if code == 18 else (_INT32_MIN, _INT32_MAX)
            if not lo <= n <= hi:
                raise ExpressionError(_OVERFLOW_MSG, code=241)
            return Int64(n) if code == 18 else int(n)

        if isinstance(value, bool):
            return _wrap(1 if value else 0)
        if isinstance(value, int):
            return _wrap(int(value))
        if isinstance(value, float):
            if not math.isfinite(value):
                raise ExpressionError(_OVERFLOW_MSG, code=241)
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
            # 15 significant digits, as `$toDecimal` — mongod-probed 6.0.16.
            from secantus.numerics import decimal_from_double

            return Decimal128(decimal_from_double(value))
        if isinstance(value, str):
            if len(value) > _MAX_INT_STR_DIGITS:
                raise ExpressionError(
                    f"$convert (decimal) input string of {len(value)} chars "
                    f"exceeds the {_MAX_INT_STR_DIGITS}-char cap"
                )
            return Decimal128(value)
    raise ExpressionError(f"$convert cannot convert {type(value).__name__} to {target!r}")


def _op_convert(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$convert requires {input, to}")
    for param in ("input", "to"):
        if param not in arg:
            raise ExpressionError(
                f"Missing '{param}' parameter to $convert", code=9, code_name="FailedToParse"
            )
    value = _eval(arg["input"], ctx)
    target = _eval(arg["to"], ctx)
    # An unknown target type name is a query-compile error (code 2) that mongod
    # raises before conversion — `onError` does NOT catch it.
    if target is not None and _CONVERT_TARGETS.get(target) is None:
        raise ExpressionError(f"Unknown type name: {target}", code=2)
    if value is None:
        return _eval(arg["onNull"], ctx) if "onNull" in arg else None
    try:
        return _convert_value(value, target)
    except (ValueError, TypeError, InvalidOperation, ExpressionError) as exc:
        if "onError" in arg:
            return _eval(arg["onError"], ctx)
        # Preserve the overflow code (mongod 241); other failures stay the
        # generic $convert error.
        if isinstance(exc, ExpressionError) and exc.code == 241:
            raise
        raise ExpressionError(f"$convert failed: {exc}") from exc


def _op_to_decimal(arg: Any, ctx: _Ctx) -> Any:
    value = _eval(arg, ctx)
    if value is None:
        return None
    if isinstance(value, Decimal128):
        return value
    if isinstance(value, (int, float)):
        from secantus.numerics import decimal_from_double

        # mongod converts a double at 15 significant digits; ints stay exact.
        return Decimal128(
            decimal_from_double(value) if isinstance(value, float) else Decimal(value)
        )
    if isinstance(value, str):
        try:
            return Decimal128(value)
        except (InvalidOperation, ValueError) as exc:
            raise ExpressionError(
                f"Failed to parse number {value!r} in $convert with no onError value",
                code=241,
                code_name="ConversionFailure",
            ) from exc
    raise ExpressionError(f"$toDecimal cannot convert {type(value).__name__}")


def _op_to_date(arg: Any, ctx: _Ctx) -> Any:
    # ``$toDate: <expr>`` is exactly ``$convert: {input: <expr>, to: "date"}``.
    # Delegate to the same conversion path so the two stay identical (same
    # supported input types, same errors). null / missing -> null.
    value = _eval(arg, ctx)
    if value is None:
        return None
    try:
        return _convert_value(value, "date")
    except (ValueError, TypeError, InvalidOperation, ExpressionError) as exc:
        # Preserve mongod's ConversionFailure code (241, e.g. bool -> date).
        if isinstance(exc, ExpressionError) and exc.code == 241:
            raise
        raise ExpressionError(f"$toDate cannot convert {type(value).__name__}") from exc


def _op_filter(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$filter requires a document spec")
    arr = _eval(arg.get("input"), ctx)
    _reject_non_array(arr, f"input to $filter must be an array not {_bson_type_name(arr)}", 28651)
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
    _reject_non_array(arr, f"input to $map must be an array not {_bson_type_name(arr)}", 16883)
    if not isinstance(arr, list):
        return None
    var_name = arg.get("as", "this")
    in_expr = arg.get("in")
    return [_eval(in_expr, ctx.with_var(var_name, elem)) for elem in arr]


def _op_reduce(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$reduce requires a document spec")
    arr = _eval(arg.get("input"), ctx)
    _reject_non_array(
        arr, f"$reduce requires that 'input' be an array, found: {_nelem_render(arr)}", 40080
    )
    if not isinstance(arr, list):
        return None
    accumulator = _eval(arg.get("initialValue"), ctx)
    in_expr = arg.get("in")
    for elem in arr:
        scoped = ctx.with_var("value", accumulator).with_var("this", elem)
        accumulator = _eval(in_expr, scoped)
    return accumulator


def _set_eq(a: Any, b: Any) -> bool:
    """Two values are the same set element iff neither sorts before the other in
    BSON order (so ``1`` == ``1.0`` but ``1`` != ``true``, matching mongod's set
    semantics)."""
    from secantus.ordering import _bson_lt

    return not _bson_lt(a, b) and not _bson_lt(b, a)


def _set_dedup_sorted(items: list[Any]) -> list[Any]:
    """Deduplicate (by BSON-order equality) and sort by BSON order — the shape
    mongod returns from ``$setUnion`` / ``$setIntersection``."""
    from secantus.ordering import _SortKey

    out: list[Any] = []
    for x in sorted(items, key=_SortKey):
        if not out or not _set_eq(out[-1], x):
            out.append(x)
    return out


_SET_OP_CODES = {
    "$setUnion": 17043,
    "$setIntersection": 17047,
    "$setDifference": 17048,
    "$setIsSubset": 17046,
    "$setEquals": 17044,
}


def _set_arrays(op: str, arg: Any, ctx: _Ctx, *, n: int | None = None) -> list[list[Any]]:
    """Evaluate a set operator's array arguments, validating each is an array
    (mongod's per-operator Location code, not a generic TypeMismatch)."""
    vals = _eval_args(arg, ctx)
    if n is not None and len(vals) != n:
        raise ExpressionError(f"{op} requires {n} arguments")
    code = _SET_OP_CODES.get(op, 14)
    for i, v in enumerate(vals):
        if not isinstance(v, list):
            if op in ("$setDifference", "$setIsSubset"):
                which = "First" if i == 0 else "Second"
                msg = (
                    f"both operands of {op} must be arrays. {which} argument is of type: "
                    f"{_bson_type_name(v)}"
                )
            else:
                msg = (
                    f"All operands of {op} must be arrays. One argument is of type: "
                    f"{_bson_type_name(v)}"
                )
            raise ExpressionError(msg, code=code, code_name=f"Location{code}")
    return vals


def _op_set_union(arg: Any, ctx: _Ctx) -> list[Any]:
    all_elems: list[Any] = []
    for v in _set_arrays("$setUnion", arg, ctx):
        all_elems.extend(v)
    return _set_dedup_sorted(all_elems)


def _op_set_intersection(arg: Any, ctx: _Ctx) -> list[Any]:
    arrays = _set_arrays("$setIntersection", arg, ctx)
    if not arrays:
        return []
    result = [
        x for x in arrays[0] if all(any(_set_eq(x, y) for y in other) for other in arrays[1:])
    ]
    return _set_dedup_sorted(result)


def _op_set_difference(arg: Any, ctx: _Ctx) -> list[Any]:
    a, b = _set_arrays("$setDifference", arg, ctx, n=2)
    out: list[Any] = []
    for x in a:  # first-array order, deduplicated
        if not any(_set_eq(x, y) for y in b) and not any(_set_eq(x, y) for y in out):
            out.append(x)
    return out


def _op_set_equals(arg: Any, ctx: _Ctx) -> bool:
    arrays = _set_arrays("$setEquals", arg, ctx)
    base = _set_dedup_sorted(arrays[0]) if arrays else []
    for other in arrays[1:]:
        o = _set_dedup_sorted(other)
        if len(o) != len(base) or any(not _set_eq(base[i], o[i]) for i in range(len(base))):
            return False
    return True


def _op_set_is_subset(arg: Any, ctx: _Ctx) -> bool:
    a, b = _set_arrays("$setIsSubset", arg, ctx, n=2)
    return all(any(_set_eq(x, y) for y in b) for x in a)


def _op_all_elements_true(arg: Any, ctx: _Ctx) -> bool:
    arr = _eval_args(arg, ctx)[0]
    if not isinstance(arr, list):
        raise ExpressionError(
            f"$allElementsTrue's argument must be an array, but is {_bson_type_name(arr)}",
            code=17040,
            code_name="Location17040",
        )
    return all(_bool(x) for x in arr)


def _op_any_element_true(arg: Any, ctx: _Ctx) -> bool:
    arr = _eval_args(arg, ctx)[0]
    if not isinstance(arr, list):
        raise ExpressionError(
            f"$anyElementTrue's argument must be an array, but is {_bson_type_name(arr)}",
            code=17041,
            code_name="Location17041",
        )
    return any(_bool(x) for x in arr)


def _op_cmp(arg: Any, ctx: _Ctx) -> int:
    """``$cmp``: three-way comparison of two values by BSON order → -1 / 0 / 1."""
    from secantus.ordering import _bson_lt

    a, b = _eval_args(arg, ctx)
    if _bson_lt(a, b):
        return -1
    return 1 if _bson_lt(b, a) else 0


def _op_binary_size(arg: Any, ctx: _Ctx) -> Any:
    """``$binarySize``: byte length of a string (UTF-8) or binary value. Null /
    missing → null."""
    v = _eval(arg, ctx)
    if v is None:
        return None
    if isinstance(v, str):
        return len(v.encode("utf-8"))
    if isinstance(v, (bytes, bson.Binary)):
        return len(v)
    raise ExpressionError(
        f"$binarySize requires a string or BinData argument, found: {_bson_type_name(v)}",
        code=51276,
        code_name="Location51276",
    )


def _op_bson_size(arg: Any, ctx: _Ctx) -> Any:
    """``$bsonSize``: the BSON-encoded byte size of a document. Null → null."""
    v = _eval(arg, ctx)
    if v is None:
        return None
    if not isinstance(v, Mapping):
        raise ExpressionError(
            f"$bsonSize requires a document input, found: {_bson_type_name(v)}",
            code=31393,
            code_name="Location31393",
        )
    return len(bson.encode(dict(v)))


def _op_degrees_to_radians(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    if isinstance(v, bool) or not isinstance(v, (int, float, Decimal128)):
        raise ExpressionError("$degreesToRadians requires a number")
    x = float(v.to_decimal()) if isinstance(v, Decimal128) else float(v)
    return x * math.pi / 180.0


def _op_radians_to_degrees(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    if isinstance(v, bool) or not isinstance(v, (int, float, Decimal128)):
        raise ExpressionError("$radiansToDegrees requires a number")
    x = float(v.to_decimal()) if isinstance(v, Decimal128) else float(v)
    return x * 180.0 / math.pi


def _bit_operand(op: str, v: Any) -> tuple[int, bool]:
    """Coerce a ``$bit*`` operand to ``(value, is_long)``. mongod's bitwise
    operators accept only int (32-bit) and long (64-bit) — a bool, double,
    decimal, or anything else raises. ``bson.Int64`` marks a long; a plain ``int``
    is a 32-bit int (``bson`` widens on encode only when out of int32 range)."""
    if isinstance(v, bool):
        raise ExpressionError(f"{op} only supports int and long operands, not bool")
    if isinstance(v, Int64):
        return int(v), True
    if isinstance(v, int):
        return v, False
    raise ExpressionError(f"{op} only supports int and long operands, not {_bson_type_name(v)}")


def _bit_result(value: int, is_long: bool) -> Any:
    """Wrap a bitwise result: ``Int64`` when any operand was long, else a plain
    ``int`` (encoded as int32 when in range, matching mongod's int result)."""
    return Int64(value) if is_long else value


def _op_bit_fold(op: str, identity: int, arg: Any, ctx: _Ctx) -> Any:
    """``$bitAnd`` / ``$bitOr`` / ``$bitXor``: fold the (int/long) operands with a
    bitwise operator. A null / missing operand makes the whole result null; the
    result is long iff any operand was long; an empty operand list yields the
    operator's identity (all-ones for and, 0 for or / xor)."""
    vals = _eval_args(arg, ctx)
    if any(v is None for v in vals):
        return None
    acc = identity
    is_long = False
    for v in vals:
        n, lng = _bit_operand(op, v)
        is_long = is_long or lng
        if op == "$bitAnd":
            acc &= n
        elif op == "$bitOr":
            acc |= n
        else:  # $bitXor
            acc ^= n
    return _bit_result(acc, is_long)


def _op_bit_and(arg: Any, ctx: _Ctx) -> Any:
    return _op_bit_fold("$bitAnd", -1, arg, ctx)


def _op_bit_or(arg: Any, ctx: _Ctx) -> Any:
    return _op_bit_fold("$bitOr", 0, arg, ctx)


def _op_bit_xor(arg: Any, ctx: _Ctx) -> Any:
    return _op_bit_fold("$bitXor", 0, arg, ctx)


def _op_bit_not(arg: Any, ctx: _Ctx) -> Any:
    """``$bitNot``: bitwise complement of a single int/long operand (null → null)."""
    v = _eval(arg, ctx)
    if v is None:
        return None
    n, is_long = _bit_operand("$bitNot", v)
    return _bit_result(~n, is_long)


def _expr_acc_values(arg: Any, ctx: _Ctx) -> list[Any]:
    """The values an expression-form accumulator (`$sum`/`$avg`/`$max`/`$min`)
    reduces over: an array argument contributes its elements, a missing/absent
    argument contributes nothing, and any other value is a single element.
    Mirrors mongod's MongoDB-5.0+ expression-accumulator semantics."""
    v = _eval(arg, ctx)
    if isinstance(v, list):
        return v
    if v is None:
        return []
    return [v]


def _expr_is_number(v: Any) -> bool:
    return isinstance(v, (int, float, Decimal128)) and not isinstance(v, bool)


def _op_expr_sum(arg: Any, ctx: _Ctx) -> Any:
    from secantus.numerics import bson_sum

    total: Any = 0
    for x in _expr_acc_values(arg, ctx):
        if _expr_is_number(x):
            total = bson_sum(total, x)
    return total


def _op_expr_avg(arg: Any, ctx: _Ctx) -> Any:
    total: Any = 0
    n = 0
    for x in _expr_acc_values(arg, ctx):
        if _expr_is_number(x):
            total += x
            n += 1
    return (total / n) if n else None


def _op_expr_max(arg: Any, ctx: _Ctx) -> Any:
    from secantus.ordering import _SortKey

    best: Any = None
    for x in _expr_acc_values(arg, ctx):
        if x is None:
            continue
        if best is None or _SortKey(x) > _SortKey(best):
            best = x
    return best


def _op_expr_min(arg: Any, ctx: _Ctx) -> Any:
    from secantus.ordering import _SortKey

    best: Any = None
    for x in _expr_acc_values(arg, ctx):
        if x is None:
            continue
        if best is None or _SortKey(x) < _SortKey(best):
            best = x
    return best


_OPS = {
    "$sum": _op_expr_sum,
    "$avg": _op_expr_avg,
    "$max": _op_expr_max,
    "$min": _op_expr_min,
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
    "$sin": _make_trig("$sin", math.sin, "finite"),
    "$cos": _make_trig("$cos", math.cos, "finite"),
    "$tan": _make_trig("$tan", math.tan, "finite"),
    "$asin": _make_trig("$asin", math.asin, "unit"),
    "$acos": _make_trig("$acos", math.acos, "unit"),
    "$atan": _make_trig("$atan", math.atan, "any"),
    "$atan2": _op_atan2,
    "$sinh": _make_trig("$sinh", math.sinh, "any"),
    "$cosh": _make_trig("$cosh", math.cosh, "any"),
    "$tanh": _make_trig("$tanh", math.tanh, "any"),
    "$asinh": _make_trig("$asinh", math.asinh, "any"),
    "$acosh": _make_trig("$acosh", math.acosh, "geq1"),
    "$atanh": _make_trig("$atanh", math.atanh, "atanh"),
    "$rand": _op_rand,
    "$trunc": _op_trunc,
    "$bitAnd": _op_bit_and,
    "$bitOr": _op_bit_or,
    "$bitXor": _op_bit_xor,
    "$bitNot": _op_bit_not,
    "$firstN": _op_first_n,
    "$lastN": _op_last_n,
    "$maxN": _op_max_n,
    "$minN": _op_min_n,
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
    "$dateFromParts": _op_date_from_parts,
    "$tsSecond": _op_ts_second,
    "$tsIncrement": _op_ts_increment,
    "$type": _op_type,
    "$isNumber": _op_is_number,
    "$isArray": _op_is_array,
    "$strcasecmp": _op_strcasecmp,
    "$replaceOne": _op_replace_one,
    "$replaceAll": _op_replace_all,
    "$split": _op_split,
    "$trim": _op_trim,
    "$ltrim": _op_ltrim,
    "$rtrim": _op_rtrim,
    "$substr": _op_substr_bytes,  # mongod: $substr is a deprecated alias of $substrBytes
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
    "$millisecond": _op_millisecond,
    "$dayOfYear": _op_day_of_year,
    "$week": _op_week,
    "$isoWeek": _op_iso_week,
    "$isoDayOfWeek": _op_iso_day_of_week,
    "$isoWeekYear": _op_iso_week_year,
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
    "$toLong": _op_to_long,
    "$toDouble": _op_to_double,
    "$toBool": _op_to_bool,
    "$toDecimal": _op_to_decimal,
    "$toDate": _op_to_date,
    "$convert": _op_convert,
    "$filter": _op_filter,
    "$map": _op_map,
    "$reduce": _op_reduce,
    "$setUnion": _op_set_union,
    "$setIntersection": _op_set_intersection,
    "$setDifference": _op_set_difference,
    "$setEquals": _op_set_equals,
    "$setIsSubset": _op_set_is_subset,
    "$allElementsTrue": _op_all_elements_true,
    "$anyElementTrue": _op_any_element_true,
    "$cmp": _op_cmp,
    "$binarySize": _op_binary_size,
    "$bsonSize": _op_bson_size,
    "$degreesToRadians": _op_degrees_to_radians,
    "$radiansToDegrees": _op_radians_to_degrees,
}


def _op_median_expr(arg: Any, ctx: _Ctx) -> Any:
    return _percentile_expr(arg, ctx, op="$median")


def _op_percentile_expr(arg: Any, ctx: _Ctx) -> Any:
    return _percentile_expr(arg, ctx, op="$percentile")


def _percentile_expr(arg: Any, ctx: _Ctx, *, op: str) -> Any:
    """Expression-form ``$median`` / ``$percentile`` over an array input —
    mongod's discrete percentile (``sorted[max(0, ceil(p*n) - 1)]`` as a
    double), sharing spec validation and value filtering with the group
    accumulators. Probed against mongod 7.0.12."""
    from secantus.aggregate import _percentile_rank, _percentile_spec, _percentile_value

    input_expr, ps = _percentile_spec(arg, op)
    raw = _eval(input_expr, ctx)
    values = sorted(
        v
        for v in (_percentile_value(x) for x in (raw if isinstance(raw, list) else [raw]))
        if v is not None
    )
    if ps is None:  # $median
        return _percentile_rank(values, 0.5)
    return [_percentile_rank(values, p) for p in ps]


_OPS["$median"] = _op_median_expr
_OPS["$percentile"] = _op_percentile_expr
