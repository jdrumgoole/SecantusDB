from __future__ import annotations

import datetime as _dt
import decimal as _decimal
import functools
import math
import re
import zoneinfo
from collections.abc import Mapping
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Any

import bson
from bson import Decimal128, Int64, ObjectId, Timestamp

from secantus.bsontypes import is_bson_string
from secantus.numerics import IntegerOverflowError, bson_int_width
from secantus.ordering import bson_equal as _bson_equal
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


#: An evaluator for a sub-expression whose value is RETURNED unchanged. The
#: four operators below take one so the caller can choose the position: `_eval`
#: (an absent path is null) or `_eval_field_value` (it stays MISSING).
_Eval = Any


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
        # A document *literal*. Each member sits in field-value position, so a
        # member whose value is an absent field path is dropped rather than
        # written as null — mongod answers `{z: {}}` for
        # `{$project: {z: {w: "$nope"}}}`, not `{z: {w: null}}`.
        out: dict[str, Any] = {}
        for k, v in expr.items():
            value = _eval_field_value(v, ctx)
            if value is not MISSING:
                out[k] = value
        return out
    return expr


def _eval_field_value(expr: Any, ctx: _Ctx) -> Any:
    """Evaluate in *field-value* position, where an absent path is MISSING.

    Differs from :func:`_eval` only for a bare field-path string: as an
    operator argument a missing path is `null` (and arithmetic over null is
    null -- `{$add: ["$nope", 1]}` is `null`, probed 6.0.16),
    but as the value of a projected/added field it is *missing* and the key is
    omitted. Keeping the two distinct is why this isn't folded into `_eval`.
    """
    if isinstance(expr, str) and expr.startswith("$") and not expr.startswith("$$"):
        d = ctx.doc if isinstance(ctx.doc, dict) else dict(ctx.doc)
        return get_path(d, expr[1:], default=MISSING)
    # ``$$REMOVE`` IS the missing value -- probed 9-for-9 against mongod 8.2.11,
    # in every position, against the equivalent absent field path. So it follows
    # the same two-position rule as one: MISSING here, ``null`` in ``_eval``.
    # It used to return MISSING from ``_resolve_var`` in BOTH positions, which
    # leaked the marker object into results: ``{arr: [1, "$$REMOVE", 2]}``
    # reached ``bson.encode`` and CRASHED the command, ``$type`` answered
    # "object" instead of "missing", and ``$concat`` raised 16702.
    if expr == "$$REMOVE":
        return MISSING
    # The operators that RETURN one of their sub-expressions propagate its
    # missing-ness; the ones that COMPUTE a value collapse it to null.
    # `{$addFields: {z: {$cond: [true, "$nosuch", 1]}}}` omits `z` on mongod --
    # probed 8.2.11, where we wrote a null. `$getField` already had its own
    # handling. The position is lost once evaluation drops into the generic
    # operator path, which is why this dispatches here rather than inside `_eval`.
    if isinstance(expr, Mapping) and len(expr) == 1:
        op, arg = next(iter(expr.items()))
        propagating = _MISSING_PROPAGATING.get(op)
        if propagating is not None:
            return propagating(arg, ctx, _eval_field_value)
    return _eval(expr, ctx)


#: Sentinel for "the expression resolved to a missing field" (distinct from an
#: explicit ``null``). ``$$REMOVE`` resolves to this in field-value position --
#: it IS the missing value, not a marker of its own (probed 9-for-9 against
#: mongod 8.2.11 versus the equivalent absent field path).
MISSING: Any = object()


def evaluate_or_missing(
    expr: Any, doc: Mapping[str, Any], vars: dict[str, Any] | None = None
) -> Any:
    """Like :func:`evaluate`, but a top-level absent field path yields
    :data:`MISSING` (distinct from ``None``) so accumulators can skip a missing
    value the way mongod does — ``$push`` / ``$addToSet`` accumulate an explicit
    ``null`` but not a missing field."""
    # ONE implementation of the field-value rule. This used to be a second copy
    # of `_eval_field_value`'s logic, and the copies drifted: the `$$REMOVE`
    # fix had to be made twice, and the missing-propagating operators below
    # would have had to be as well.
    return _eval_field_value(expr, _Ctx(doc=doc, vars=dict(vars) if vars else {}))


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
        # Value position: an absent field path is ``null`` here, and
        # ``$$REMOVE`` is exactly an absent field path. ``_eval_field_value``
        # above returns MISSING for the field-value position, which is what
        # makes ``$project`` / ``$addFields`` omit the key.
        return None
    else:
        # ``$$KEEP`` / ``$$PRUNE`` / ``$$DESCEND`` deliberately fall through to
        # here. They are NOT globally-defined variables: mongod binds them only
        # while evaluating a ``$redact`` expression and answers
        # ``Use of undefined variable: KEEP`` (17276) anywhere else -- probed on
        # 8.2.11. This used to return the string ``"$$KEEP"`` for any of them,
        # which leaked an internal marker into user output
        # (``$project: {x: "$$KEEP"}`` returned it as data) and, worse, made a
        # stored string equal to ``"$$KEEP"`` indistinguishable from the
        # sentinel -- so ``$redact: "$field"`` over attacker-controlled content
        # kept a document mongod refuses to keep. ``aggregate._stage_redact``
        # now binds them in ``vars`` for the duration of its own evaluation.
        raise ExpressionError(
            f"Use of undefined variable: {base}", code=17276, code_name="Location17276"
        )
    if not rest:
        return value
    if not isinstance(value, Mapping):
        return None
    return get_path(value if isinstance(value, dict) else dict(value), rest, default=None)


#: Operators mongod rejects with 16020 when the argument count is wrong, and
#: the count it wants. DERIVED by asking mongod 8.2.11 each operator with 0-4
#: arguments and reading the arity out of its own message, not from docs.
#:
#: The count is `len(arg)` for a list and 1 for anything else -- a bare
#: `{$abs: 5}` is one argument, and so is a nested expression document.
#: `$cond`'s OBJECT form (`{if, then, else}`) is exempt: it is a document, so
#: it would count as 1 against an arity of 3.
_FIXED_ARITY: dict[str, int] = {
    # arity 1
    "$abs": 1,
    "$acos": 1,
    "$acosh": 1,
    "$allElementsTrue": 1,
    "$anyElementTrue": 1,
    "$arrayToObject": 1,
    "$asin": 1,
    "$asinh": 1,
    "$atan": 1,
    "$atanh": 1,
    "$binarySize": 1,
    "$bitNot": 1,
    "$bsonSize": 1,
    "$ceil": 1,
    "$cos": 1,
    "$cosh": 1,
    "$degreesToRadians": 1,
    "$exp": 1,
    "$first": 1,
    "$floor": 1,
    "$isArray": 1,
    "$isNumber": 1,
    "$last": 1,
    "$ln": 1,
    "$log10": 1,
    "$not": 1,
    "$objectToArray": 1,
    "$radiansToDegrees": 1,
    "$reverseArray": 1,
    "$sin": 1,
    "$sinh": 1,
    "$size": 1,
    "$sqrt": 1,
    "$strLenBytes": 1,
    "$strLenCP": 1,
    "$tan": 1,
    "$tanh": 1,
    "$toLower": 1,
    "$toUpper": 1,
    "$tsIncrement": 1,
    "$tsSecond": 1,
    "$type": 1,
    # arity 2
    "$arrayElemAt": 2,
    "$atan2": 2,
    "$cmp": 2,
    "$divide": 2,
    "$eq": 2,
    "$gt": 2,
    "$gte": 2,
    "$in": 2,
    "$log": 2,
    "$lt": 2,
    "$lte": 2,
    "$mod": 2,
    "$ne": 2,
    "$pow": 2,
    "$setDifference": 2,
    "$setIsSubset": 2,
    "$split": 2,
    "$strcasecmp": 2,
    "$subtract": 2,
    # arity 3
    "$cond": 3,
    "$substr": 3,
    "$substrBytes": 3,
    "$substrCP": 3,
}


#: Operators mongod reports under a different name, because they are aliases.
_ARITY_ALIASES = {"$substr": "$substrBytes"}


#: Operators whose argument must be a DOCUMENT, with mongod's code and wording.
#: Taken from mongod 8.2.11 one operator at a time -- the five phrasings are its
#: own and are not interchangeable ("found: {t}" vs "found {t}" vs no type at
#: all), which is why this is a table rather than one message with the operator
#: name substituted in.
#:
#: Like the arity check, this is a PARSE error: an empty collection reports it.
_OBJECT_ARG: dict[str, tuple[int, str]] = {
    "$convert": (9, "$convert expects an object of named arguments but found: {t}"),
    "$dateAdd": (5166400, "$dateAdd expects an object as its argument"),
    "$dateDiff": (5166301, "$dateDiff only supports an object as its argument"),
    "$dateFromParts": (40519, "$dateFromParts only supports an object as its argument"),
    "$dateFromString": (
        40540,
        "$dateFromString only supports an object as an argument, found: {t}",
    ),
    "$dateSubtract": (5166400, "$dateSubtract expects an object as its argument"),
    "$dateToParts": (40524, "$dateToParts only supports an object as its argument"),
    "$dateToString": (18629, "$dateToString only supports an object as its argument"),
    "$dateTrunc": (5439007, "$dateTrunc only supports an object as its argument"),
    "$filter": (28646, "$filter only supports an object as its argument"),
    "$let": (16874, "$let only supports an object as its argument"),
    "$ltrim": (50696, "$ltrim only supports an object as an argument, found {t}"),
    "$map": (16878, "$map only supports an object as its argument"),
    "$reduce": (40075, "$reduce requires an object as an argument, found: {t}"),
    "$regexFind": (51103, "$regexFind expects an object of named arguments but found: {t}"),
    "$regexFindAll": (51103, "$regexFindAll expects an object of named arguments but found: {t}"),
    "$regexMatch": (51103, "$regexMatch expects an object of named arguments but found: {t}"),
    "$replaceAll": (51751, "$replaceAll requires an object as an argument, found: {t}"),
    "$replaceOne": (51751, "$replaceOne requires an object as an argument, found: {t}"),
    "$rtrim": (50696, "$rtrim only supports an object as an argument, found {t}"),
    "$setField": (4161100, "$setField only supports an object as its argument"),
    "$sortArray": (2942500, "$sortArray requires an object as an argument, found: {t}"),
    "$switch": (40060, "$switch requires an object as an argument, found: {t}"),
    "$trim": (50696, "$trim only supports an object as an argument, found {t}"),
    "$zip": (34460, "$zip only supports an object as an argument, found {t}"),
}


#: Operators taking a RANGE of argument counts, with mongod's own 28667.
_RANGED_ARITY: dict[str, tuple[int, int]] = {"$trunc": (1, 2), "$round": (1, 2)}


def _ranged_arity_problem(op: str, arg: Any) -> tuple[int, str] | None:
    bounds = _RANGED_ARITY.get(op)
    if bounds is None:
        return None
    lo, hi = bounds
    got = len(arg) if isinstance(arg, list) else 1
    if lo <= got <= hi:
        return None
    # `{$trunc: []}` reached `arg[0]` and raised IndexError -> internal error.
    return (
        28667,
        f"Expression {op} takes at least {lo} arguments, and at most {hi}, "
        f"but {got} were passed in.",
    )


#: The keys a document-argument operator accepts, with mongod's code and wording
#: for an unrecognised one. Only the operators whose key set has been probed are
#: here -- an operator absent from this table is not checked, which is the
#: conservative direction.
_OBJECT_KEYS: dict[str, tuple[int, str, tuple[str, ...]]] = {
    "$cond": (17083, "Unrecognized parameter to $cond: {k}", ("if", "then", "else")),
    "$dateToString": (
        18534,
        "Unrecognized argument to $dateToString: {k}",
        ("date", "format", "timezone", "onNull"),
    ),
}


#: Operators that introduce their own variables, and so cannot be folded: the
#: bound value comes from the input. `$map` over a literal array still does not
#: fold on mongod -- probed.
_BINDING_OPS = frozenset({"$map", "$filter", "$reduce"})

#: Non-deterministic, so never folded.
_NON_DETERMINISTIC = frozenset({"$rand"})

#: `$getField` reads `$$CURRENT`, so mongod never folds it -- not even with a
#: wholly literal `input`: `{$getField: {field: 0, input: {a: 1}}}` is an
#: EXECUTOR error on 8.2.11 (probed 2026-09-02), where treating it as constant
#: reported the optimizer's prefix.
_NEVER_FOLDED = frozenset({"$getField"})

#: Variables that are constant for the whole pipeline. `$$ROOT` / `$$CURRENT`
#: are the document, so they are not.
_CONSTANT_VARS = frozenset({"NOW", "CLUSTER_TIME"})


def is_constant_expression(expr: Any, bound: frozenset[str]) -> bool:
    """Whether mongod can fold ``expr`` at optimization time.

    Decides which of two prefixes an error carries: a folded expression fails
    under ``Failed to optimize pipeline :: caused by ::``, and a
    document-dependent one under ``Executor error during aggregate command on
    namespace: … :: caused by ::``. Probed on mongod 8.2.11: a field path,
    ``$$ROOT`` / ``$$CURRENT``, a variable bound from the input and ``$rand``
    are all execution-time; literals, ``$$NOW`` and the command's own ``let``
    values fold.

    CONSERVATIVE: anything unrecognised is treated as non-constant, which keeps
    the executor prefix -- the behaviour before this existed.
    """
    if isinstance(expr, str):
        if expr.startswith("$$"):
            base = expr[2:].split(".", 1)[0]
            return base in _CONSTANT_VARS or base in bound
        return not expr.startswith("$")  # a bare `$path` reads the document
    if isinstance(expr, list):
        return all(is_constant_expression(e, bound) for e in expr)
    if not isinstance(expr, Mapping):
        return True
    for op, arg in expr.items():
        if op == "$literal":
            continue
        if op in _NON_DETERMINISTIC or op in _BINDING_OPS or op in _NEVER_FOLDED:
            return False
        if op == "$let":
            if not isinstance(arg, Mapping):
                return False
            names = arg.get("vars")
            if not isinstance(names, Mapping):
                return False
            if not all(is_constant_expression(v, bound) for v in names.values()):
                return False
            if not is_constant_expression(arg.get("in"), bound | set(names)):
                return False
            continue
        if not is_constant_expression(arg, bound):
            return False
    return True


def _object_keys_problem(op: str, arg: Any) -> tuple[int, str] | None:
    """An unrecognised key in a document-argument operator.

    A PARSE error to mongod, which is why it lives in the walker rather than at
    the raise site: reported from the operator itself it took the executor
    wrapper, where mongod uses `Invalid $<stage> :: caused by ::`. It also used
    to be a bare `KeyError` escaping as `internal server error`.
    """
    entry = _OBJECT_KEYS.get(op)
    if entry is None or not isinstance(arg, Mapping):
        return None
    code, template, allowed = entry
    for key in arg:
        if key not in allowed:
            return (code, template.format(k=key))
    return None


def _object_arg_problem(op: str, arg: Any) -> tuple[int, str] | None:
    """mongod's error when a document-argument operator gets something else."""
    entry = _OBJECT_ARG.get(op)
    if entry is None or isinstance(arg, Mapping):
        return None
    code, template = entry
    return (code, template.format(t=_bson_type_name(arg)))


#: The conversion shorthands. Single-argument like the ``_FIXED_ARITY`` family
#: -- and they accept the ``{$toInt: [expr]}`` list form the same way -- but
#: mongod gives them their OWN wrong-arity error (``50723 $toInt requires a
#: single argument, got 2``) rather than the 16020 wording, so they cannot just
#: join that table. Probed 8.2.11, 2026-09-01; before this they were absent
#: from both, so ``{$toInt: ["$s"]}`` -- the form every ``$`` field reference
#: naturally takes -- tried to convert the ARRAY and answered a type error.
_CONVERSION_SHORTHANDS = frozenset(
    {
        "$toBool",
        "$toDate",
        "$toDecimal",
        "$toDouble",
        "$toInt",
        "$toLong",
        "$toObjectId",
        "$toString",
    }
)


def _arity_problem(op: str, arg: Any) -> tuple[int, str] | None:
    """mongod's 16020 when a fixed-arity operator gets the wrong count."""
    if op in _CONVERSION_SHORTHANDS:
        if isinstance(arg, list) and len(arg) != 1:
            return (50723, f"{op} requires a single argument, got {len(arg)}")
        return None
    want = _FIXED_ARITY.get(op)
    if want is None:
        return None
    # `$cond`'s object form is the one document argument that is not "one
    # argument" -- it carries all three.
    if op == "$cond" and isinstance(arg, Mapping):
        return None
    got = len(arg) if isinstance(arg, list) else 1
    if got == want:
        return None
    # `$substr` is an ALIAS: mongod names the canonical operator in the message.
    name = _ARITY_ALIASES.get(op, op)
    # "1 arguments" is mongod's own plural, and it is reproduced.
    return (
        16020,
        f"Expression {name} takes exactly {want} arguments. {got} were passed in.",
    )


def _apply_op(op: str, arg: Any, ctx: _Ctx) -> Any:
    if op == "$literal":
        return arg
    # mongod's expression parser treats `{$op: [x]}` as ONE argument for the
    # single-argument operators, unwrapping the list. We passed the list
    # through, which produced silent WRONG VALUES rather than errors:
    # `{$size: [[1, 2]]}` counted the outer array (1, not 2), `{$toUpper: ["a"]}`
    # returned `["a"]`, and `{$first: ["$arr"]}` returned the whole array.
    if (
        isinstance(arg, list)
        and len(arg) == 1
        and (_FIXED_ARITY.get(op) == 1 or op in _CONVERSION_SHORTHANDS)
    ):
        arg = arg[0]
    elif isinstance(arg, list) and op in _CONVERSION_SHORTHANDS and len(arg) != 1:
        # Belt and braces: `_arity_problem` already reports this at PARSE time
        # for anything going through the pipeline, and answers the same code and
        # message. This catches a direct `evaluate()` call, which skips that
        # scan -- without it the bad arity would fall through and be reported as
        # a conversion of the ARRAY.
        raise ExpressionError(
            f"{op} requires a single argument, got {len(arg)}",
            code=50723,
            code_name="Location50723",
        )
    handler = _OPS.get(op)
    if handler is None:
        raise UnknownExpressionOperatorError(op)
    return handler(arg, ctx)


def _eval_args(arg: Any, ctx: _Ctx) -> list[Any]:
    if isinstance(arg, list):
        return [_eval(a, ctx) for a in arg]
    return [_eval(arg, ctx)]


def _bool(value: Any) -> bool:
    """mongod's truthiness: only null, missing, `false` and zero are false.

    Every other value is true -- including the EMPTY STRING, an empty array and
    an empty document, none of which follow Python's own truthiness. `$or: ""`
    is true on mongod and was false here, and the same rule governs `$and`,
    `$cond`, `$switch` cases and `$filter`. `Decimal128` is a number and has to
    be tested as one rather than falling through to the catch-all.
    """
    if value is None or value is MISSING:
        return False
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return bool(value)
    if isinstance(value, Decimal128):
        return bool(value.to_decimal())
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
    """mongod's type vocabulary for arithmetic error messages.

    Delegates to `secantus.bsontypes` — see there for why three copies of this
    existed and what each one got wrong.
    """
    from secantus.bsontypes import bson_type_name

    return bson_type_name(v)


def _is_numeric(v: Any) -> bool:
    # bool is an int subclass in Python but NOT numeric in BSON arithmetic —
    # mongod rejects it ("$multiply only supports numeric types, not bool").
    return isinstance(v, (int, float, Decimal128)) and not isinstance(v, bool)


def _fmt_double(v: float) -> str:
    """Render a double the way mongod prints it in an error message.

    mongod streams a double into a message with C++'s `ostream <<` at its
    default precision -- six significant digits -- so this is `printf("%g")`,
    not a round-trip form. The two agree on the small fractionals most of these
    messages carry (`2.7`), which is why `repr` stood here for so long, and
    diverge as soon as a value needs more digits: 1099511627776.0 prints as
    `1.09951e+12` and 0.0 as `0`. Probed 8.2.11 via `$acos`'s Location50989.
    `$toString` uses the round-trip form instead -- see `convert_to_string`.
    """
    return f"{v:g}"


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


def _int_result(value: Any, *operands: Any) -> Any:
    """mongod's width rule for an integer arithmetic result.

    Three parts, all probed on 8.2.11: `long` is contagious, so a long operand
    makes the answer a long even when it would fit in 32 bits (`Int64(1) + 1`
    is a long); an int result that outgrows 32 bits widens to long
    (`$abs` of -2147483648 is a long); and one that outgrows *64* bits
    saturates to a double rather than failing (`$pow: [2, 64]` is a double,
    and `$pow: [10, 400]` is `inf`).

    Python's ints are unbounded and `Int64.__add__` hands back a plain `int`,
    so without this every long silently narrowed to an int, and every 64-bit
    overflow reached `bson.encode` as an out-of-range int -- which raised
    `OverflowError` from inside the cursor-splitting code and surfaced to the
    client as an internal error instead of a double.
    """
    if not isinstance(value, int) or isinstance(value, bool):
        return value
    try:
        return bson_int_width(value, wide=any(isinstance(v, Int64) for v in operands))
    except IntegerOverflowError:
        try:
            return float(value)
        except OverflowError:
            return math.inf if value > 0 else -math.inf


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
    return _int_result(acc2, *values)


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
        return _int_result(a - b, a, b)
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
    if _has_decimal(a, b):
        # `Decimal.__mod__` truncates toward zero, which is C's `fmod` and
        # mongod's rule -- Python's `%` on ints/floats floors instead, which is
        # why this cannot just widen the existing expression.
        return _decimal_result(lambda x, y: x % y, a, b)
    if isinstance(a, float) or isinstance(b, float):
        # Truncating, not flooring: mongod answers -1.5 for `$mod: [-5.5, 2]`
        # where Python's `%` answers 0.5. Probed 8.2.11.
        return math.fmod(a, b)
    remainder = abs(a) % abs(b)
    return _int_result(-remainder if a < 0 else remainder, a, b)


def _operands(arg: Any) -> list[Any]:
    """The operand EXPRESSIONS of a logical operator, unevaluated.

    A single non-array operand is a one-element list to mongod
    (`{$and: "$s"}`); iterating the argument directly walked the string
    CHARACTER BY CHARACTER, so `"$s"` became `'$'` and `'s'` and the first
    parsed as an empty field path. Returning expressions rather than values
    keeps `$and` / `$or` lazy -- mongod short-circuits at runtime, so
    `{$and: [false, {$divide: ["$n", 0]}]}` is false and must not evaluate the
    divide (probed 8.2.11; an all-constant version folds at optimization time
    and DOES raise, which is why the field reference matters here).
    """
    return arg if isinstance(arg, list) else [arg]


def _op_and(arg: Any, ctx: _Ctx) -> bool:
    return all(_bool(_eval(a, ctx)) for a in _operands(arg))


def _op_or(arg: Any, ctx: _Ctx) -> bool:
    return any(_bool(_eval(a, ctx)) for a in _operands(arg))


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


#: A missing field ranks immediately BELOW null in the comparison operators --
#: probed on mongod 6.0.16, where `$cmp: ["$absent", null]` is -1 and
#: `$cmp: ["$absent", "$alsoAbsent"]` is 0. Anything else compares as a value.
_MISSING_RANK = object()


def _cmp_operand(expr: Any, ctx: _Ctx) -> Any:
    """One comparison operand, with a missing field path kept DISTINCT from null.

    The comparison operators are the one place in the expression language where
    the difference is observable: `$eq: ["$absent", null]` is **false** on
    mongod, while `$eq: ["$explicitNull", null]` is true. Everywhere else an
    operator argument resolving to a missing path is simply null
    (`{$add: ["$nope", 1]}` is 1), which is why this is not `_eval_field_value`
    for every operator.

    We evaluated both through `_eval`, which resolves a missing path to None, so
    every comparison against null answered true for documents that did not have
    the field at all -- and `$cond` built on `$eq` inherited it.
    """
    value = _eval_field_value(expr, ctx)
    return _MISSING_RANK if value is MISSING else _unwrap_d128(value)


def _cmp_pair(arg: Any, ctx: _Ctx) -> tuple[Any, Any]:
    if isinstance(arg, list) and len(arg) == 2:
        return _cmp_operand(arg[0], ctx), _cmp_operand(arg[1], ctx)
    a, b = _eval_args(arg, ctx)
    return _unwrap_d128(a), _unwrap_d128(b)


def _op_eq(arg: Any, ctx: _Ctx) -> bool:
    a, b = _cmp_pair(arg, ctx)
    # MISSING equals only MISSING -- `is` rather than `==` because the sentinel
    # must not compare equal to None.
    if a is _MISSING_RANK or b is _MISSING_RANK:
        return a is b
    return _bson_equal(a, b)


def _op_ne(arg: Any, ctx: _Ctx) -> bool:
    a, b = _cmp_pair(arg, ctx)
    if a is _MISSING_RANK or b is _MISSING_RANK:
        return a is not b
    return not _bson_equal(a, b)


def _relational(arg: Any, ctx: _Ctx, want: tuple[int, ...]) -> bool:
    """`$gt` / `$gte` / `$lt` / `$lte`, over mongod's BSON order.

    These used to compare with Python's own operators and swallow the
    `TypeError` a cross-type pair raises:

        try:
            return bool(a > b)
        except TypeError:
            return False

    So EVERY comparison between different BSON types answered false, silently.
    `{$gt: ["abc", 1]}` is true on mongod -- a string sorts after a number in
    the canonical order -- and `{$lt: [null, 1]}` is true likewise. `$cmp` two
    thousand lines below had it right all along, via `ordering._bson_lt`; these
    four never used it. The expression language drives `$expr`, `$cond`,
    `$filter`, `$switch` and `$bucket`, so the wrong answer reached rows.
    """
    a, b = _cmp_pair(arg, ctx)
    if a is _MISSING_RANK or b is _MISSING_RANK:
        # MISSING ranks below every real value.
        order = 0 if a is b else (-1 if a is _MISSING_RANK else 1)
    else:
        from secantus.ordering import _bson_lt

        order = -1 if _bson_lt(a, b) else (1 if _bson_lt(b, a) else 0)
    return order in want


def _op_gt(arg: Any, ctx: _Ctx) -> bool:
    return _relational(arg, ctx, (1,))


def _op_gte(arg: Any, ctx: _Ctx) -> bool:
    return _relational(arg, ctx, (0, 1))


def _op_lt(arg: Any, ctx: _Ctx) -> bool:
    return _relational(arg, ctx, (-1,))


def _op_lte(arg: Any, ctx: _Ctx) -> bool:
    return _relational(arg, ctx, (-1, 0))


def _op_cond(arg: Any, ctx: _Ctx, ret: _Eval = None) -> Any:
    ret = ret or _eval
    if isinstance(arg, Mapping):
        # An unrecognised key is mongod's 17083; a MISSING one is `null` rather
        # than a `KeyError` escaping as `internal server error`.
        for key in arg:
            if key not in ("if", "then", "else"):
                raise ExpressionError(
                    f"Unrecognized parameter to $cond: {key}",
                    code=17083,
                    code_name="Location17083",
                )
        condition = _eval(arg.get("if"), ctx)
        return ret(arg.get("then") if _bool(condition) else arg.get("else"), ctx)
    if isinstance(arg, list) and len(arg) == 3:
        return ret(arg[1] if _bool(_eval(arg[0], ctx)) else arg[2], ctx)
    raise ExpressionError("$cond requires {if, then, else} or [cond, then, else]")


def _op_if_null(arg: Any, ctx: _Ctx, ret: _Eval = None) -> Any:
    if not isinstance(arg, list) or len(arg) < 2:
        n = len(arg) if isinstance(arg, list) else 1
        raise ExpressionError(
            f"$ifNull needs at least two arguments, had: {n}",
            code=1257300,
            code_name="Location1257300",
        )
    *checks, fallback = arg
    ret = ret or _eval
    for check in checks:
        v = ret(check, ctx)
        # A MISSING check is skipped exactly like a null one: `$ifNull` is
        # looking for the first argument that HAS a value.
        if v is not None and v is not MISSING:
            return v
    return ret(fallback, ctx)


#: The operators whose result IS one of their sub-expressions, so a missing
#: sub-expression makes the whole thing missing. Populated after the functions
#: are defined; `_eval_field_value` looks each one up by name.
_MISSING_PROPAGATING: dict[str, Any] = {}


def _operand_type_name(arg: Any, value: Any, ctx: _Ctx) -> str:
    """The type name mongod prints for `value`, which `arg` evaluated to.

    `_eval` reports an absent field and an explicit null alike as ``None``, but
    mongod's wrong-type messages distinguish them: ``{$size: "$nosuch"}`` says
    `missing` where ``{$size: null}`` says `null` (probed 8.2.11, 2026-09-02).
    Only the message needs the distinction, so it is recovered while one is
    being built rather than threaded through evaluation.
    """
    if value is None and _eval_field_value(arg, ctx) is MISSING:
        return "missing"
    return _bson_type_name(value)


def _op_size(arg: Any, ctx: _Ctx) -> int:
    value = _eval(arg, ctx)
    if not isinstance(value, list):
        raise ExpressionError(
            "The argument to $size must be an array, but was of type: "
            f"{_operand_type_name(arg, value, ctx)}",
            code=17124,
            code_name="Location17124",
        )
    return len(value)


def _render_date_iso(v: _dt.datetime) -> str:
    """A date as mongod renders it into a string: ISO-8601, always three
    fractional digits, always `Z`."""
    return v.strftime("%Y-%m-%dT%H:%M:%S.") + f"{v.microsecond // 1000:03d}Z"


def coerce_to_string(value: Any) -> str:
    """mongod's `Value::coerceToString` -- what `$toLower` / `$toUpper` run
    their operand through before case-folding it.

    This is NOT `$toString`'s conversion, and the difference is not cosmetic:
    the two accept *different types* and render numbers *differently*.
    coerceToString takes a timestamp and a javascript value but rejects a bool
    and an ObjectId (Location16007); `$toString` does the reverse. A double
    here goes through `%g` (`1099511627776.0` -> `1.09951e+12`), where
    `$toString` round-trips it. Null and missing both become the empty string
    here, and null there. All probed against 8.2.11.
    """
    if value is None or value is MISSING:
        return ""
    # `bson.Code` subclasses `str`, so this branch already covers it -- and
    # mongod does convert a javascript value to its source text. `str()`
    # normalises it to a plain string rather than handing back the `Code`.
    if isinstance(value, str):
        return str(value)
    # bool is an int subclass, so it has to be rejected before the int branch.
    if isinstance(value, bool):
        raise ExpressionError(
            "can't convert from BSON type bool to String", code=16007, code_name="Location16007"
        )
    if isinstance(value, float):
        return _fmt_double(value)
    if isinstance(value, (int, Decimal128)):
        return str(value)
    if isinstance(value, _dt.datetime):
        return _render_date_iso(value)
    if isinstance(value, bson.Code):
        return str(value)
    if isinstance(value, bson.Timestamp):
        # mongod formats a timestamp for this one conversion as `%b %e
        # %H:%M:%S` in *local* time, with the increment appended after a colon:
        # `Jan  1 01:00:01:2`. Local, not UTC -- confirmed against four values
        # on 8.2.11, and it is why this rendering can't be checked against a
        # server in another zone.
        import time as _time

        stamp = _time.strftime("%b %e %H:%M:%S", _time.localtime(value.time))
        return f"{stamp}:{value.inc}"
    raise ExpressionError(
        f"can't convert from BSON type {_bson_type_name(value)} to String",
        code=16007,
        code_name="Location16007",
    )


def convert_to_string(value: Any) -> Any:
    """mongod's `$convert` to string -- what `$toString` does. See
    `coerce_to_string` for how the two differ."""
    if value is None or value is MISSING:
        return None
    # `bson.Code` subclasses `str` but mongod refuses to convert one, so it is
    # rejected here rather than falling into the str branch below.
    if isinstance(value, bson.Code):
        raise ExpressionError(
            "Unsupported conversion from javascript to string in $convert with no onError value",
            code=241,
            code_name="ConversionFailure",
        )
    if isinstance(value, str):
        return str(value)
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, float):
        # The round-trip form, with a whole double's trailing `.0` dropped:
        # `4.0` -> `4`, `1099511627776.0` -> `1099511627776`, `1e+300`
        # unchanged. Python's `repr` is already shortest-round-trip.
        if math.isnan(value):
            return "NaN"
        if math.isinf(value):
            return "Infinity" if value > 0 else "-Infinity"
        text = repr(value)
        return text[:-2] if text.endswith(".0") else text
    if isinstance(value, (int, Decimal128, ObjectId)):
        return str(value)
    if isinstance(value, _dt.datetime):
        return _render_date_iso(value)
    if isinstance(value, bytes):
        import base64

        return base64.b64encode(value).decode("ascii")
    raise ExpressionError(
        f"Unsupported conversion from {_bson_type_name(value)} to string in "
        "$convert with no onError value",
        code=241,
        code_name="ConversionFailure",
    )


def _op_to_string(arg: Any, ctx: _Ctx) -> Any:
    return convert_to_string(_eval(arg, ctx))


#: mongod's ``$trim`` default whitespace set — its documented 20-character
#: table, confirmed character by character against 8.2.11 (2026-09-01). It is
#: NOT Python's ``str.strip()`` set: it INCLUDES U+00A0 / U+1680 / U+2000-200A
#: and EXCLUDES U+0085 / U+2028 / U+2029 / U+202F / U+205F / U+3000, which
#: ``strip()`` removes. `"\u3000pad\u3000"` came back `"pad"` where mongod
#: leaves it untouched.
TRIM_WHITESPACE = "".join(
    chr(c) for c in (0x00, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x20, 0xA0, 0x1680, *range(0x2000, 0x200B))
)


def ascii_upper(s: str) -> str:
    """``$toUpper``'s case mapping: ASCII ONLY, which is mongod's.

    Python's ``str.upper()`` does full Unicode case mapping and is WRONG here --
    probed against 8.2.11 (2026-09-01), mongod answers ``'ÜNïCODé'`` for
    ``'Ünïcodé'`` and ``'STRAßE'`` for ``'straße'``, leaving every non-ASCII
    character alone, where ``.upper()`` gives ``'ÜNÏCODÉ'`` and ``'STRASSE'``.
    11 of 18 probed strings diverged.

    The Rust engine used to DEFER these operators "for Unicode-fidelity safety",
    which had it backwards: the faithful answer is the simple one, and the
    deferral is what made the standalone Rust server error on them. Both engines
    now do the same ASCII mapping natively.
    """
    return s.translate(_ASCII_UPPER)


def ascii_lower(s: str) -> str:
    """``$toLower``'s case mapping: ASCII ONLY. See :func:`ascii_upper`."""
    return s.translate(_ASCII_LOWER)


_ASCII_UPPER = {c: c - 32 for c in range(ord("a"), ord("z") + 1)}
_ASCII_LOWER = {c: c + 32 for c in range(ord("A"), ord("Z") + 1)}


def _op_to_lower(arg: Any, ctx: _Ctx) -> Any:
    return ascii_lower(coerce_to_string(_eval(arg, ctx)))


def _op_to_upper(arg: Any, ctx: _Ctx) -> Any:
    return ascii_upper(coerce_to_string(_eval(arg, ctx)))


#: IEEE 754 decimal128 carries 34 significant digits, and that is the precision
#: mongod computes these operators in. Converting through `float` -- which is
#: what every one of them used to do -- narrows the result to 17 digits, and for
#: nine of them raised a `TypeError` outright because `math` rejects a
#: `Decimal128`. Both were visible to a caller: `{$sqrt: Decimal128("2.5")}`
#: answered `internal server error`.
_DEC128_CTX = _decimal.Context(prec=34)


#: pi to more digits than decimal128 carries, so the conversion below rounds
#: rather than truncates.
_PI = _decimal.Decimal("3.14159265358979323846264338327950288419716939937510")

#: The double conversion factors, computed once so the multiplication below
#: associates the way mongod's does.
_RADIANS_PER_DEGREE = math.pi / 180.0
_DEGREES_PER_RADIAN = 180.0 / math.pi


def _to_decimal(v: Any) -> _decimal.Decimal:
    """A numeric operand as a `Decimal`, exactly."""
    if isinstance(v, Decimal128):
        return v.to_decimal()
    if isinstance(v, _decimal.Decimal):
        return v
    if isinstance(v, float):
        # Through `str`, not `Decimal(float)`: the latter carries the binary
        # value's full expansion (0.1 -> 0.1000000000000000055511151231257827).
        return _decimal.Decimal(str(v))
    return _decimal.Decimal(v)


def _has_decimal(*vals: Any) -> bool:
    return any(isinstance(v, Decimal128) for v in vals)


def _decimal_result(fn: Any, *vals: Any) -> Decimal128:
    """Run `fn` over the operands as `Decimal`s, at decimal128 precision."""
    with _decimal.localcontext(_DEC128_CTX):
        return Decimal128(fn(*(_to_decimal(v) for v in vals)))


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
    if _has_decimal(v):
        return _decimal_result(abs, v)
    return _int_result(abs(v), v)


#: The inclusive range mongod accepts for a `$round` / `$trunc` precision.
_PRECISION_MIN, _PRECISION_MAX = -20, 100

#: `Value::integral()` is a 32-bit test, not a "has no fractional part" test —
#: which is why an int64 precision of 2**31 is rejected as "not integral" while
#: 2**31 - 1 gets as far as the range check.
_INT32_MIN, _INT32_MAX = -(2**31), 2**31 - 1


def _round_precision(place: Any, op: str) -> int | None:
    """Validate a ``$round`` / ``$trunc`` precision the way mongod does.

    Reconstructed from probes against 8.2.11 (2026-09-01), which pinned a
    three-step order that produces three *different* error codes:

    1. ``Value::coerceToLong`` — a non-numeric (string, bool, ...) is
       Location16004, and a NaN / Infinity double is Location31109.
    2. ``Value::integral()`` — Location51082. This is the step the old code
       only half-had: it rejected a fractional ``float`` but silently ignored a
       fractional ``Decimal128`` (``$round: ["$n", Decimal128("1.5")]``
       answered 8.0) and an out-of-int32 integer (``1e10`` answered 7.5).
    3. the ``[-20, 100]`` bounds — Location51083, which was missing entirely:
       ``$round: ["$n", -25]`` answered 0.0 where mongod refuses.

    A null / missing precision short-circuits to a null *result*, so this
    returns ``None`` to mean "the whole operator is null" — distinct from a
    precision of 0.
    """
    if place is None:
        return None
    if isinstance(place, bool) or not isinstance(place, (int, float, Decimal128)):
        raise ExpressionError(
            f"can't convert from BSON type {_bson_type_name(place)} to long",
            code=16004,
            code_name="Location16004",
        )
    as_dec = place.to_decimal() if isinstance(place, Decimal128) else None
    if isinstance(place, float) and not math.isfinite(place):
        raise ExpressionError(
            f"Can't coerce out of range value {_fmt_double(place)} to long",
            code=31109,
            code_name="Location31109",
        )
    if as_dec is not None and not as_dec.is_finite():
        raise ExpressionError(
            f"Can't coerce out of range value {as_dec} to long",
            code=31109,
            code_name="Location31109",
        )
    numeric = (
        float(place) if isinstance(place, float) else (as_dec if as_dec is not None else place)
    )
    integral = _INT32_MIN <= numeric <= _INT32_MAX and (
        numeric == int(numeric) if not isinstance(place, int) else True
    )
    if not integral:
        raise ExpressionError(
            # The doubled space after "to" is mongod's own — it streams the
            # operator name into a slot that already carries a trailing space.
            f"precision argument to  {op} must be a integral value",
            code=51082,
            code_name="Location51082",
        )
    value = int(numeric)
    if not (_PRECISION_MIN <= value <= _PRECISION_MAX):
        raise ExpressionError(
            f"cannot apply {op} with precision value {value} value must be in "
            f"[{_PRECISION_MIN}, {_PRECISION_MAX}]",
            code=51083,
            code_name="Location51083",
        )
    return value


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
    place = _round_precision(place, "$round")
    if place is None:
        return None
    if _has_decimal(n):
        # Half-to-even, which is what `round` does for floats and what mongod
        # documents for `$round`.
        return _decimal_result(
            lambda d: d.quantize(
                _decimal.Decimal(1).scaleb(-place), rounding=_decimal.ROUND_HALF_EVEN
            ),
            n,
        )
    return _int_result(round(n, place), n)


def _op_floor(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$floor")
    if _has_decimal(v):
        return _decimal_result(lambda d: d.to_integral_value(rounding=_decimal.ROUND_FLOOR), v)
    # These operators are type-preserving in mongod: a double in is a double
    # out (`$floor` of 1.5 is 2.0, not 2), an int stays an int. Python's
    # `math.floor` returns an int for either, which changed the BSON type of
    # every double that reached it. Probed 8.2.11.
    return float(math.floor(v)) if isinstance(v, float) else _int_result(math.floor(v), v)


def _op_ceil(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$ceil")
    if _has_decimal(v):
        return _decimal_result(lambda d: d.to_integral_value(rounding=_decimal.ROUND_CEILING), v)
    # Type-preserving, as `$floor` above.
    return float(math.ceil(v)) if isinstance(v, float) else _int_result(math.ceil(v), v)


def _op_sqrt(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$sqrt")
    # mongod's domain error (probed 7.0.12): Location28714, not a null result.
    if isinstance(v, (int, float)) and v < 0:
        raise ExpressionError(
            # No ", but is <v>" suffix -- $sqrt is the one operator in this
            # family that omits it, where $ln / $log10 keep it (probed 8.2.11).
            "$sqrt's argument must be greater than or equal to 0",
            code=28714,
            code_name="Location28714",
        )
    if _has_decimal(v):
        return _decimal_result(lambda d: d.sqrt(), v)
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
    if _has_decimal(base, exponent):
        # `exp(e * ln(b))`, not `b ** e`. mongod computes it that way and the
        # rounding shows: `2.5 ** 2` is exactly 6.25, but mongod answers
        # 6.249999999999999999999999999999999, and matching the reference
        # server is the point. A zero base has no `ln`, so it is handled first.
        return _decimal_result(lambda b, e: b**e if b == 0 else (e * b.ln()).exp(), base, exponent)
    result = base**exponent
    # A negative base with a fractional exponent yields a Python complex, which
    # is unencodable (crashes BSON) — mongod returns NaN instead.
    if isinstance(result, complex):
        return float("nan")
    return _int_result(result, base, exponent)


def _op_exp(arg: Any, ctx: _Ctx) -> Any:
    import math

    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$exp")
    if _has_decimal(v):
        return _decimal_result(lambda d: d.exp(), v)
    try:
        return math.exp(v)
    except OverflowError:
        # mongod saturates to infinity; the OverflowError escaped as an
        # `internal server error`.
        return math.inf


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
    if _has_decimal(v):
        return _decimal_result(lambda d: d.ln(), v)
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
    if _has_decimal(n, base):
        return _decimal_result(lambda x, b: x.ln() / b.ln(), n, base)
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
    if _has_decimal(v):
        return _decimal_result(lambda d: d.log10(), v)
    return math.log10(v)


def _trig_coerce(name: str, v: Any, code: int = 28765) -> float:
    """Coerce a trig operand to float. bool / non-numeric raise ``code``
    (mongod's ``Location28765`` for the unary ops, ``51044`` for ``$atan2``).
    Decimal128 is float-cast, matching ``$degreesToRadians`` (SecantusDB does
    not reproduce mongod's decimal-precise transcendental result)."""
    if isinstance(v, bool) or not isinstance(v, (int, float, Decimal128)):
        raise ExpressionError(f"{name} only supports numeric types, not {_type_name(v)}", code=code)
    return float(v.to_decimal()) if isinstance(v, Decimal128) else float(v)


#: Decimal128 implementations of the HYPERBOLIC functions, as exact identities
#: over `exp` / `ln` / `sqrt` -- all of which `decimal` provides, so these carry
#: the full 34 digits mongod does. The CIRCULAR functions have no such identity
#: and are summed as series below.
#: Working precision for the circular functions below. `decimal` supplies
#: `exp` / `ln` / `sqrt` -- which is all the hyperbolics need -- but nothing
#: for sin / cos / tan / atan, so those are summed here. The series are
#: evaluated with guard digits and the result handed back at decimal128's 34,
#: so the rounding happens once, at the end.
_DEC_TRIG_CTX = _decimal.Context(prec=60)


def _dec_series_sin(x: _decimal.Decimal) -> _decimal.Decimal:
    """sin(x) by Taylor series, x already reduced into [-pi, pi]."""
    term = total = x
    x2 = x * x
    n = 1
    while term:
        n += 2
        term = -term * x2 / (n * (n - 1))
        total += term
    return total


def _dec_series_cos(x: _decimal.Decimal) -> _decimal.Decimal:
    """cos(x) by Taylor series, x already reduced into [-pi, pi]."""
    term = total = _decimal.Decimal(1)
    x2 = x * x
    n = 0
    while term:
        n += 2
        term = -term * x2 / (n * (n - 1))
        total += term
    return total


def _dec_reduce(x: _decimal.Decimal) -> _decimal.Decimal:
    """x mod 2*pi, brought into [-pi, pi] where the series converge fast."""
    two_pi = 2 * _PI
    r = x - (x / two_pi).to_integral_value(rounding=_decimal.ROUND_FLOOR) * two_pi
    return r - two_pi if r > _PI else r


def _dec_sin(x: _decimal.Decimal) -> _decimal.Decimal:
    with _decimal.localcontext(_DEC_TRIG_CTX):
        return _DEC128_CTX.plus(_dec_series_sin(_dec_reduce(x)))


def _dec_cos(x: _decimal.Decimal) -> _decimal.Decimal:
    with _decimal.localcontext(_DEC_TRIG_CTX):
        return _DEC128_CTX.plus(_dec_series_cos(_dec_reduce(x)))


def _dec_tan(x: _decimal.Decimal) -> _decimal.Decimal:
    with _decimal.localcontext(_DEC_TRIG_CTX):
        r = _dec_reduce(x)
        return _DEC128_CTX.plus(_dec_series_sin(r) / _dec_series_cos(r))


def _dec_atan_raw(x: _decimal.Decimal) -> _decimal.Decimal:
    """atan(x) for any finite x, at the working precision.

    The Taylor series only converges for |x| < 1 and crawls as |x| nears it,
    so the argument is shrunk with the identity
    `atan(x) = 2*atan(x / (1 + sqrt(1 + x*x)))` until it is comfortably small.

    Unrounded on purpose: `$asin` / `$acos` / `$atan2` build on this and must
    round once, at the end, rather than at every step.
    """
    if x.is_zero():
        return x
    sign, x = (-1, -x) if x < 0 else (1, x)
    halvings = 0
    while x > _decimal.Decimal("0.05"):
        x = x / (1 + (1 + x * x).sqrt())
        halvings += 1
    term = total = x
    x2 = x * x
    n = 1
    while term:
        n += 2
        term = -term * x2
        total += term / n
    return sign * total * (2**halvings)


def _dec_asin_raw(x: _decimal.Decimal) -> _decimal.Decimal:
    if abs(x) == 1:
        return x * _PI / 2
    return _dec_atan_raw(x / (1 - x * x).sqrt())


def _dec_atan(x: _decimal.Decimal) -> _decimal.Decimal:
    with _decimal.localcontext(_DEC_TRIG_CTX):
        return _DEC128_CTX.plus(_dec_atan_raw(x))


def _dec_asin(x: _decimal.Decimal) -> _decimal.Decimal:
    with _decimal.localcontext(_DEC_TRIG_CTX):
        return _DEC128_CTX.plus(_dec_asin_raw(x))


def _dec_acos(x: _decimal.Decimal) -> _decimal.Decimal:
    with _decimal.localcontext(_DEC_TRIG_CTX):
        return _DEC128_CTX.plus(_PI / 2 - _dec_asin_raw(x))


def _dec_atan2(y: _decimal.Decimal, x: _decimal.Decimal) -> _decimal.Decimal:
    """atan2(y, x) at decimal128 precision, with mongod's quadrant rules --
    probed 8.2.11, including that a negative-zero `y` carries its sign into
    the answer (atan2(-0, -1) is -pi, not pi)."""
    with _decimal.localcontext(_DEC_TRIG_CTX):
        if x.is_zero():
            if y.is_zero():
                return _DEC128_CTX.plus(y)
            return _DEC128_CTX.plus(-_PI / 2 if y.is_signed() else _PI / 2)
        result = _dec_atan_raw(y / x)
        if x < 0:
            result = result - _PI if y.is_signed() else result + _PI
        return _DEC128_CTX.plus(result)


_DEC_TRIG: dict[str, Any] = {
    "$sinh": lambda d: (d.exp() - (-d).exp()) / 2,
    "$cosh": lambda d: (d.exp() + (-d).exp()) / 2,
    "$tanh": lambda d: (d.exp() - (-d).exp()) / (d.exp() + (-d).exp()),
    "$asinh": lambda d: (d + (d * d + 1).sqrt()).ln(),
    "$acosh": lambda d: (d + (d * d - 1).sqrt()).ln(),
    "$atanh": lambda d: ((1 + d) / (1 - d)).ln() / 2,
    # Without these six a Decimal128 operand fell through to the double path,
    # so `{$sin: Decimal128("2.5")}` answered a *double* -- the wrong BSON
    # type, which then compares and sorts differently downstream. Probed
    # against 8.2.11, which answers Decimal128 for all six.
    "$sin": _dec_sin,
    "$cos": _dec_cos,
    "$tan": _dec_tan,
    "$asin": _dec_asin,
    "$acos": _dec_acos,
    "$atan": _dec_atan,
}


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
                f"cannot apply {name} to {_fmt_double(x)}, value must be in (-inf,inf)",
                code=50989,
            )
        if domain in ("unit", "atanh") and not (-1.0 <= x <= 1.0):
            raise ExpressionError(
                f"cannot apply {name} to {_fmt_double(x)}, value must be in [-1,1]", code=50989
            )
        if domain == "geq1" and not x >= 1.0:
            raise ExpressionError(
                f"cannot apply {name} to {_fmt_double(x)}, value must be in [1,inf]", code=50989
            )
        if domain == "atanh" and abs(x) == 1.0:
            return math.inf if x > 0 else -math.inf
        dec_fn = _DEC_TRIG.get(name)
        if dec_fn is None or not _has_decimal(v):
            try:
                return fn(x)
            except OverflowError:
                # `$sinh` / `$cosh` of a large value: mongod saturates to
                # infinity rather than failing the command.
                return math.copysign(math.inf, x) if name == "$sinh" else math.inf
        if dec_fn is not None and _has_decimal(v):
            # At decimal128 precision THROUGHOUT, deliberately. Computing wide
            # and rounding back is more accurate and matches mongod LESS: it
            # accumulates its own rounding at 34 digits, so `$cosh` moved from
            # agreeing to differing in the last digit when guard digits were
            # tried. Fidelity here means reproducing the arithmetic, not
            # improving on it.
            return _decimal_result(dec_fn, v)
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
    if _has_decimal(y, x):
        # One decimal operand is enough: mongod answers a Decimal128 whichever
        # side it is on. Falling through to `math.atan2` narrowed the answer to
        # a double -- the wrong BSON type, as with the unary trig above.
        return _decimal_result(_dec_atan2, y, x)
    return math.atan2(fy, fx)


def _op_rand(arg: Any, _ctx: _Ctx) -> float:
    # MongoDB 5.0+: ``{$rand: {}}`` returns a uniform random double in
    # [0, 1). Argument must be an empty document; anything else is a
    # parse error in mongod (we mirror).
    # Three outcomes, not one (probed 8.2.11): an empty document OR an empty
    # ARRAY is the legal no-argument form; a NON-empty array is 3040501 "does
    # not currently accept arguments"; anything that is neither a document nor
    # an array is 10065 "invalid parameter: expected an object ($rand)".
    if isinstance(arg, (list, Mapping)):
        if arg:
            raise ExpressionError(
                "$rand does not currently accept arguments",
                code=3040501,
                code_name="Location3040501",
            )
    else:
        raise ExpressionError(
            "invalid parameter: expected an object ($rand)",
            code=10065,
            code_name="Location10065",
        )
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
    place = _round_precision(place, "$trunc")
    if place is None:
        return None
    if _has_decimal(n):
        # `quantize` at the requested place, truncating toward zero.
        return _decimal_result(
            lambda d: d.quantize(_decimal.Decimal(1).scaleb(-place), rounding=_decimal.ROUND_DOWN),
            n,
        )
    factor = 10**place
    # Type-preserving, as `$floor` / `$ceil`: dividing by `factor` made every
    # int result a double (`$trunc` of 1 answered 1.0). Probed 8.2.11.
    truncated = math.trunc(n * factor) / factor
    return _int_result(int(truncated), n) if isinstance(n, int) else truncated


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
    # Membership, not `is None`: `value: null` is a PRESENT argument that writes
    # a null (`{$setField: {field: "a", input: "$$ROOT", value: null}}` sets
    # `a: null` -- probed 8.2.11). Testing for None read it as absent and
    # rejected the one form that distinguishes "write null" from "remove".
    if not all(k in arg for k in ("field", "input", "value")):
        raise ExpressionError("$setField requires field, input, value")
    field_expr = arg["field"]
    input_expr = arg["input"]
    value_expr = arg["value"]
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
    # FIELD-VALUE position: mongod removes the field for `$$REMOVE` *and* for an
    # absent path (`value: "$nosuch"` -- probed 8.2.11), and only an explicit
    # null writes a null. This used to use `_eval`, so an absent path wrote a
    # null where mongod removes.
    value = _eval_field_value(value_expr, ctx)
    result = dict(input_doc)
    if value is MISSING:
        result.pop(field, None)
    else:
        result[field] = value
    return result


def _op_get_field(arg: Any, ctx: _Ctx) -> Any:
    """MongoDB 5.0+ ``$getField`` — read a field by name from a document.

    Accepts ``{field, input}`` (full form) or a bare expression (shorthand
    for ``{field: <expr>, input: $$CURRENT}``). ``field`` is EVALUATED, not
    taken literally: a plain string evaluates to itself, so ``{$getField: "s"}``
    still reads field ``s``, but ``{$getField: "$n"}`` resolves the path and
    then refuses the int it finds. Taking the bare form literally looked for a
    field NAMED ``$n`` and answered missing where mongod errors (probed 8.2.11,
    2026-09-02). A literally-dollared name needs ``$literal`` — which is
    mongod's rule, and the reason the bare form exists at all.
    """
    is_options_form = isinstance(arg, Mapping) and not (
        len(arg) == 1 and next(iter(arg)).startswith("$")
    )
    if not is_options_form:
        field, input_expr = _eval(arg, ctx), "$$CURRENT"
        if not is_bson_string(field):
            # mongod distinguishes an ABSENT path from an explicit null here --
            # `{$getField: "$nosuch"}` says `missing` -- and `_eval` collapses
            # both to None, so the field-value evaluator supplies the name.
            named = _eval_field_value(arg, ctx)
            raise ExpressionError(
                "$getField requires 'field' to evaluate to type String, but got "
                f"{'missing' if named is MISSING else _bson_type_name(field)}",
                code=3041704,
                code_name="Location3041704",
            )
    else:
        # A single `$`-key document is a nested EXPRESSION, not the
        # `{field, input}` options form -- `{$getField: {$literal: "$odd"}}` is
        # how a literally-dollared field name is written, and treating it as the
        # options form answered "unknown argument: $literal" (probed 8.2.11,
        # 2026-09-02). The same operator-vs-options rule the date extractors use.
        #
        # An UNKNOWN argument outranks everything else, and `input` is required
        # once the object form is used (probed 8.2.11).
        for key in arg:
            if key not in ("field", "input"):
                raise ExpressionError(
                    f"$getField found an unknown argument: {key}",
                    code=3041701,
                    code_name="Location3041701",
                )
        field_expr = arg.get("field")
        if "input" not in arg:
            raise ExpressionError(
                "$getField requires 'input' to be specified",
                code=3041703,
                code_name="Location3041703",
            )
        input_expr = arg["input"]
        if field_expr is None:
            raise ExpressionError("$getField requires a field")
        field = _eval(field_expr, ctx)
        if not isinstance(field, str):
            raise ExpressionError(
                "$getField requires 'field' to evaluate to type String, but got "
                f"{_operand_type_name(field_expr, field, ctx)}",
                code=3041704,
                code_name="Location3041704",
            )
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
        input_doc = get_path(dict(ctx.doc), input_expr[1:], default=MISSING)
    else:
        input_doc = _eval(input_expr, ctx)
    if input_doc is MISSING:
        return MISSING
    if input_doc is None or not isinstance(input_doc, Mapping):
        return None
    # A field absent from the input document resolves to "missing" (the same
    # marker as ``$$REMOVE``), so a ``$project`` / ``$addFields`` computed field
    # that reads it is omitted from the output. A field present with an explicit
    # ``null`` still returns ``None`` (and is emitted).
    if field not in input_doc:
        return MISSING
    return input_doc[field]


def _op_switch(arg: Any, ctx: _Ctx, ret: _Eval = None) -> Any:
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
            return (ret or _eval)(branch["then"], ctx)
    if "default" in arg:
        return (ret or _eval)(arg["default"], ctx)
    # mongod 8.2.11 answers 40066 here, wrapped in its executor prefix. It
    # answers a DIFFERENT error -- 40069, "Cannot execute a switch statement
    # where all the cases evaluate to false without a default", under
    # `Failed to optimize pipeline` -- when every case is a constant it can
    # fold at parse time. Reproducing that split means modelling constant
    # folding for message text alone; see `tasks/remaining-work-plan.md` 1b.
    raise ExpressionError(
        "$switch could not find a matching branch for an input, and no default was specified.",
        code=40066,
        code_name="Location40066",
    )


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


def _shift_date_in_zone(
    d: _dt.datetime, unit: str, amount: int, tz: _dt.tzinfo | None
) -> _dt.datetime:
    """`$dateAdd` / `$dateSubtract`, which shift a CALENDAR unit on the LOCAL
    wall clock.

    Probed 8.2.11 (2026-09-01): noon Eastern on 2026-03-07 plus one day is noon
    Eastern on the 8th -- 23 real hours, because the spring-forward falls
    between them -- while the same shift in UTC adds 24. The timezone was
    ignored outright on both servers, so every calendar shift across a DST
    boundary was an hour out. Sub-day units (`hour` and below) are absolute and
    unaffected, which is why a 24-`hour` shift is NOT the same as a 1-`day` one.
    """
    if tz is None or unit in _SUBDAY_UNIT_MS:
        return _shift_date(d, unit, amount)
    aware = d if d.tzinfo is not None else d.replace(tzinfo=_dt.timezone.utc)
    local = aware.astimezone(tz).replace(tzinfo=None)
    shifted = _shift_date(local, unit, amount)
    utc = _localize(shifted, tz).astimezone(_dt.timezone.utc)
    return utc if d.tzinfo is not None else utc.replace(tzinfo=None)


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
    tz = _resolve_timezone(arg.get("timezone")) if "timezone" in arg else None
    return _shift_date_in_zone(start, unit, n, tz)


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
    tz = _resolve_timezone(arg.get("timezone")) if "timezone" in arg else None
    return _shift_date_in_zone(start, unit, -n, tz)


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
    tz = (
        _resolve_timezone(arg.get("timezone"), operator="$dateTrunc") if "timezone" in arg else None
    )
    return _truncate_date(date, unit, bin_size, tz, _eval(arg.get("startOfWeek"), ctx))


_TRUNC_REFERENCE = _dt.datetime(2000, 1, 1)
"""mongod bins every `$dateTrunc` unit from 2000-01-01T00:00:00 IN THE TARGET
ZONE -- not from the epoch, and not from year 1. Probed 8.2.11 (2026-09-01):
`binSize: 7` days over 2000-01-0N truncates to 2000-01-01 for N<8, and in
`Asia/Kolkata` that same bin starts at 1999-12-31T18:30Z, i.e. local midnight."""

_WEEKDAYS = ("monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday")

# Integer MILLISECONDS, not float seconds. The first version of this divided
# `timedelta.total_seconds()` -- a float -- by a float step, and the rounding
# put `$dateDiff` in milliseconds one out over a twenty-year span (mongod and
# the Rust engine both answer the exact integer). `timedelta // timedelta` is
# exact integer floor division, so nothing here goes through a float.
_SUBDAY_UNIT_MS = {
    "hour": 3_600_000,
    "minute": 60_000,
    "second": 1000,
    "millisecond": 1,
}


def _localize(naive: _dt.datetime, tz: _dt.tzinfo | None) -> _dt.datetime:
    """A local wall-clock reading back to the instant it names."""
    return naive.replace(tzinfo=tz or _dt.timezone.utc)


def _date_bin_index(
    aware: _dt.datetime,
    unit: str,
    bin_size: int,
    zone: _dt.tzinfo,
    start_of_week: Any = None,
) -> int:
    """Which `unit` bin an instant falls in, counting from the reference.

    The single source for both `$dateTrunc` (which rebuilds the datetime from
    this index) and `$dateDiff` (which subtracts two of them). mongod's
    `$dateDiff` counts BOUNDARY CROSSINGS, not elapsed whole units -- probed
    8.2.11 (2026-09-01): 02:30 to 03:10 is 1 hour, 02:00 to 02:59 is 0, and
    02:59:59.9 to 03:00 is 1 -- so it is exactly this subtraction, and keeping
    one function means the two operators cannot drift apart.

    Two arithmetics, because mongod uses both:

    * **year / quarter / month / week / day** are CALENDAR units, indexed off
      the local wall clock, so a `day` bin boundary is local midnight even
      though DST makes consecutive ones 23 or 25 real hours apart.
    * **hour / minute / second / millisecond** index by REAL ELAPSED TIME from
      the reference instant, so `binSize: 5` hours stays 5 real hours apart
      across a DST shift instead of re-aligning to the wall clock. The anchor
      is still LOCAL midnight of 2000-01-01, which is why a half-hour-offset
      zone like `Asia/Kolkata` puts hour boundaries on the half hour.
    """
    local = aware.astimezone(zone).replace(tzinfo=None)
    if unit in _SUBDAY_UNIT_MS:
        reference = _localize(_TRUNC_REFERENCE, zone).astimezone(_dt.timezone.utc)
        step = _dt.timedelta(milliseconds=_SUBDAY_UNIT_MS[unit] * bin_size)
        return (aware - reference) // step
    if unit == "year":
        return (local.year - _TRUNC_REFERENCE.year) // bin_size
    if unit == "quarter":
        quarters = (local.year - _TRUNC_REFERENCE.year) * 4 + (local.month - 1) // 3
        return quarters // bin_size
    if unit == "month":
        months = (local.year - _TRUNC_REFERENCE.year) * 12 + (local.month - 1)
        return months // bin_size
    if unit == "week":
        days = (
            _dt.datetime(local.year, local.month, local.day) - _week_reference(start_of_week)
        ).days
        return (days // 7) // bin_size
    if unit == "day":
        days = (_dt.datetime(local.year, local.month, local.day) - _TRUNC_REFERENCE).days
        return days // bin_size
    raise ExpressionError(f"unknown time unit value: {unit}", code=9, code_name="FailedToParse")


def _truncate_date(
    date: _dt.datetime,
    unit: str,
    bin_size: int,
    tz: _dt.tzinfo | None,
    start_of_week: Any = None,
) -> _dt.datetime:
    """mongod's `$dateTrunc`, which truncates IN the timezone.

    The timezone used to be ignored outright -- read off the spec, never
    applied -- so every bucket landed on a UTC boundary. A daily rollup for
    `America/New_York` bucketed at 00:00Z rather than 04:00Z, silently
    attributing four hours of each day to the wrong bucket. Nothing errored.
    """
    aware = date if date.tzinfo is not None else date.replace(tzinfo=_dt.timezone.utc)
    zone = tz or _dt.timezone.utc
    index = _date_bin_index(aware, unit, bin_size, zone, start_of_week)

    def answer(instant: _dt.datetime) -> _dt.datetime:
        # Same awareness as the input. Documents hold NAIVE UTC datetimes here
        # (pymongo decodes BSON dates that way), so returning an aware one makes
        # every later comparison against a stored date raise TypeError.
        utc = instant.astimezone(_dt.timezone.utc)
        return utc if date.tzinfo is not None else utc.replace(tzinfo=None)

    if unit in _SUBDAY_UNIT_MS:
        # In UTC, deliberately. Adding a `timedelta` to a ZONE-AWARE datetime is
        # WALL-CLOCK arithmetic: the local reading advances and the offset is
        # re-resolved, so crossing a DST boundary silently moves the instant by
        # an hour. Anchoring in UTC keeps this absolute.
        reference = _localize(_TRUNC_REFERENCE, zone).astimezone(_dt.timezone.utc)
        step = _dt.timedelta(milliseconds=_SUBDAY_UNIT_MS[unit] * bin_size)
        return answer(reference + index * step)

    if unit == "year":
        truncated = _dt.datetime(_TRUNC_REFERENCE.year + index * bin_size, 1, 1)
    elif unit == "quarter":
        quarters = index * bin_size
        truncated = _dt.datetime(_TRUNC_REFERENCE.year + quarters // 4, (quarters % 4) * 3 + 1, 1)
    elif unit == "month":
        months = index * bin_size
        truncated = _dt.datetime(_TRUNC_REFERENCE.year + months // 12, months % 12 + 1, 1)
    elif unit == "week":
        truncated = _week_reference(start_of_week) + _dt.timedelta(weeks=index * bin_size)
    else:  # day
        truncated = _TRUNC_REFERENCE + _dt.timedelta(days=index * bin_size)

    return answer(_localize(truncated, zone))


def _week_reference(start_of_week: Any) -> _dt.datetime:
    """The first `start_of_week` weekday ON OR AFTER 2000-01-01.

    mongod's default is SUNDAY -- 2000-01-02 -- which is what makes
    `$dateTrunc` by week land on a Sunday. The old code used 1970-01-05, a
    Monday, so the default bucket was a day out for every week truncation.
    """
    name = "sunday" if start_of_week is None else start_of_week
    if not isinstance(name, str) or name.lower() not in _WEEKDAYS:
        raise ExpressionError(
            f"unknown startOfWeek value: {name}", code=5439015, code_name="Location5439015"
        )
    want = _WEEKDAYS.index(name.lower())
    ref = _TRUNC_REFERENCE
    return ref + _dt.timedelta(days=(want - ref.weekday()) % 7)


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
    tz = _resolve_timezone(arg.get("timezone")) if "timezone" in arg else None
    return _shift_date_in_zone(start, unit, n, tz)


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
    tz = _resolve_timezone(arg.get("timezone")) if "timezone" in arg else None
    return _shift_date_in_zone(start, unit, -n, tz)


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
        # The leading space is mongod's own, and it names the offending type
        # (probed 8.2.11, 2026-09-02).
        raise ExpressionError(
            f" Argument to $tsSecond must be a timestamp, but is {_bson_type_name(v)}",
            code=5687301,
            code_name="Location5687301",
        )
    return Int64(v.time)


def _op_ts_increment(arg: Any, ctx: _Ctx) -> Any:
    """``$tsIncrement``: the increment (ordinal) field of a BSON Timestamp (as a
    long). Null / missing → null; a non-timestamp raises ``Location5687302``."""
    v = _eval(arg, ctx)
    if v is None:
        return None
    if not isinstance(v, bson.Timestamp):
        raise ExpressionError(
            f" Argument to $tsIncrement must be a timestamp, but is {_bson_type_name(v)}",
            code=5687302,
            code_name="Location5687302",
        )
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
    if arg == "$$REMOVE":
        return "missing"  # `$$REMOVE` IS the missing value -- probed
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
    # ASCII-only upper, like `$toUpper` — `.upper()` folded `ß` to `SS` and
    # reported `strcasecmp("ß", "SS")` as 0 where mongod says 1 (probed 8.2.11).
    au = ascii_upper(_strcasecmp_coerce(vals[0]))
    bu = ascii_upper(_strcasecmp_coerce(vals[1]))
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
    # mongod counts BOUNDARY CROSSINGS in the timezone, which is the same bin
    # index `$dateTrunc` floors to -- so this is one subtraction, not a second
    # implementation of the calendar. The timezone used to be ignored entirely
    # and the calendar units computed "whole units elapsed" instead: 02:00Z to
    # 23:00Z answered 0 days where New York answers 1 (locally that crosses
    # midnight), and 2026-07-01 to 2026-07-31 answered 0 months where mongod
    # answers 1. Both were silent wrong answers.
    tz = _resolve_timezone(arg.get("timezone"), operator="$dateDiff") if "timezone" in arg else None
    zone = tz or _dt.timezone.utc
    start_aware = start if start.tzinfo is not None else start.replace(tzinfo=_dt.timezone.utc)
    end_aware = end if end.tzinfo is not None else end.replace(tzinfo=_dt.timezone.utc)
    start_of_week = _eval(arg.get("startOfWeek"), ctx) if "startOfWeek" in arg else None
    return _date_bin_index(end_aware, unit, 1, zone, start_of_week) - _date_bin_index(
        start_aware, unit, 1, zone, start_of_week
    )


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
    # non-string first/second arg -> 40085/10503900; an empty separator -> 40087.
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
            # 10503900 on 8.2.11, not the 40086 this recorded -- the first
            # argument keeps 40085 and only the SECOND moved (probed
            # 2026-09-02). We target 8.x, so the newer code is the right one.
            code=10503900,
            code_name="Location10503900",
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
    # The DEFAULT set is mongod's own table, not Python's `strip()` set --
    # see TRIM_WHITESPACE. An explicit `chars` is used verbatim either way.
    cut = chars if chars else TRIM_WHITESPACE
    if side == "l":
        return s.lstrip(cut)
    if side == "r":
        return s.rstrip(cut)
    return s.strip(cut)


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


#: `$indexOfArray` was given its OWN error codes at some point after the string
#: forms got theirs, and mongod still carries both pairs (probed 8.2.11,
#: 2026-09-01): the string operators raise 40096 / 40097, the array operator
#: 9711600 / 9711601, with the same two message texts.
_INDEX_OF_CODES = {"$indexOfArray": (9711600, 9711601)}
_INDEX_OF_DEFAULT_CODES = (40096, 40097)


def _index_of_pos(op: str, which: str, v: Any) -> int:
    """Validate a ``$indexOf*`` start / end index. mongod accepts an int or whole
    double; a fractional double / bool / non-numeric is the operator's "integral"
    code (note the message's verbatim missing space after the operator name),
    and a negative index is its "nonnegative" code."""
    integral_code, nonneg_code = _INDEX_OF_CODES.get(op, _INDEX_OF_DEFAULT_CODES)
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise ExpressionError(
            f"{op}requires an integral {which} index, found a value of type: "
            f"{_bson_type_name(v)}, with value: {_mongo_val_repr(v)}",
            code=integral_code,
            code_name=f"Location{integral_code}",
        )
    if isinstance(v, float):
        if not v.is_integer():
            raise ExpressionError(
                f"{op}requires an integral {which} index, found a value of type: "
                f"{_bson_type_name(v)}, with value: {_mongo_val_repr(v)}",
                code=integral_code,
                code_name=f"Location{integral_code}",
            )
        v = int(v)
    if v < 0:
        raise ExpressionError(
            f"{op} requires a nonnegative {which} index, found: {v}",
            code=nonneg_code,
            code_name=f"Location{nonneg_code}",
        )
    return v


def _op_index_of_cp(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, list) or not 2 <= len(arg) <= 4:
        raise ExpressionError("$indexOfCP requires [string, search, start?, end?]")
    s = _eval(arg[0], ctx)
    needle = _eval(arg[1], ctx)
    if s is None:
        return None
    # mongod names the OFFENDING argument and its type, with a distinct code
    # per position (probed 8.2.11): 40093 for the first, 40094 for the second.
    if not is_bson_string(s):
        raise ExpressionError(
            f"$indexOfCP requires a string as the first argument, found: {_bson_type_name(s)}",
            code=40093,
            code_name="Location40093",
        )
    if not is_bson_string(needle):
        raise ExpressionError(
            f"$indexOfCP requires a string as the second argument, "
            f"found: {_bson_type_name(needle)}",
            code=40094,
            code_name="Location40094",
        )
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
    # Shares the string forms' validator, which this used to duplicate by hand
    # and get wrong in three ways (probed 8.2.11, 2026-09-01): the codes were
    # the string operators' 40096 / 40097 rather than this operator's own
    # 9711600 / 9711601; a non-numeric index (`"x"`) silently answered -1 where
    # mongod refuses; and a NEGATIVE index was clamped to 0 by `max(0, start)`,
    # so `{$indexOfArray: [[1, 2, 3], 3, -1]}` answered 2 where mongod raises.
    start = _index_of_pos("$indexOfArray", "starting", _eval(arg[2], ctx)) if len(arg) >= 3 else 0
    end = (
        _index_of_pos("$indexOfArray", "ending", _eval(arg[3], ctx)) if len(arg) >= 4 else len(arr)
    )
    for i in range(start, min(len(arr), end)):
        if arr[i] == needle:
            return i
    return -1


def _op_let(arg: Any, ctx: _Ctx, ret: _Eval = None) -> Any:
    if not isinstance(arg, Mapping) or "vars" not in arg or "in" not in arg:
        raise ExpressionError("$let requires {vars, in}")
    bindings = arg["vars"]
    if not isinstance(bindings, Mapping):
        raise ExpressionError("$let.vars must be a document")
    inner = ctx
    for name, value_expr in bindings.items():
        # Bind in FIELD-VALUE position so a missing path stays MISSING rather
        # than collapsing to null. mongod binds `$$v` from an absent field as
        # missing, so `$eq: ["$$v", null]` is false -- we bound null and it was
        # true. The same rule governs `$lookup`'s `let`, where it meant a
        # document without the local field joined rows mongod excludes.
        inner = inner.with_var(name, _eval_field_value(value_expr, ctx))
    return (ret or _eval)(arg["in"], inner)


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
        # Location34449 — the generic BadValue this used to raise had neither
        # mongod's code nor its wording (probed 8.2.11, 2026-09-01).
        raise ExpressionError(
            "$range requires a non-zero step value",
            code=34449,
            code_name="Location34449",
        )
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
    """A date-extractor operand (`$year`/`$dayOfYear`/…) resolved to a `datetime`.

    mongod accepts every BSON type that CARRIES a timestamp -- Date, ObjectId
    (its 4-byte generation time) and Timestamp (its seconds field) -- and raises
    ``Location16006`` on anything else present; null / missing yield null.
    Probed 8.2.11 (2026-09-02): `{$year: ObjectId("64b7f9a2…")}` answers 2023,
    where this used to refuse the whole document as unconvertible. That was a
    wrong ANSWER on 13 shapes: an error where mongod returns a value.
    """
    # mongod treats a ONE-ELEMENT array as the argument itself, so
    # `{$year: [<date>]}` is `{$year: <date>}`. Any other length is a parse
    # error caught before this (40536).
    if isinstance(value, list) and len(value) == 1:
        value = value[0]
    if isinstance(value, _dt.datetime):
        return value
    if value is None:
        return None
    if isinstance(value, ObjectId):
        # `generation_time` is tz-aware UTC; the rest of the date family works
        # in naive UTC, so strip it rather than mixing the two.
        return value.generation_time.replace(tzinfo=None)
    if isinstance(value, Timestamp):
        return _dt.datetime.fromtimestamp(value.time, _dt.timezone.utc).replace(tzinfo=None)
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


def resolve_timezone_argument(name: Any, *, operator: str | None = None) -> _dt.tzinfo | None:
    """Public alias -- the aggregate layer validates literal timezones with it."""
    return _resolve_timezone(name, operator=_WRAPPED_TZ_OPERATORS.get(operator or ""))


# Only these two name the parameter in the message; every other date operator
# reports the bare "unrecognized time zone identifier" (probed 8.2.11).
_WRAPPED_TZ_OPERATORS = {"$dateTrunc": "$dateTrunc", "$dateDiff": "$dateDiff"}


def _bad_timezone(name: str, operator: str | None) -> str:
    """mongod's unrecognised-zone message, in the two forms it has.

    `$dateTrunc` and `$dateDiff` name the parameter they were parsing;
    every other date operator reports the bare message. Probed 8.2.11
    (2026-09-01) -- and it really is only those two.
    """
    base = f'unrecognized time zone identifier: "{name}"'
    if operator is None:
        return base
    return f"{operator} parameter 'timezone' value parsing failed :: caused by :: {base}"


def _resolve_timezone(name: Any, *, operator: str | None = None) -> _dt.tzinfo | None:
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
        raise ExpressionError(_bad_timezone(name, operator), code=40485)
    # Case-sensitively, via the canonical name set. `zoneinfo.ZoneInfo` resolves
    # through the filesystem, so on a case-INSENSITIVE one (macOS, Windows)
    # "America/new_york" loads happily while mongod -- and this same server on
    # Linux -- rejects it. That made the answer depend on the host filesystem.
    if name not in _known_timezones():
        # mongod: Location40485 "unrecognized time zone identifier: \"<name>\""
        raise ExpressionError(_bad_timezone(name, operator), code=40485)
    try:
        return zoneinfo.ZoneInfo(name)
    except zoneinfo.ZoneInfoNotFoundError as exc:
        raise ExpressionError(_bad_timezone(name, operator), code=40485) from exc


@functools.lru_cache(maxsize=1)
def _known_timezones() -> frozenset[str]:
    """Every IANA name this host knows, exactly as spelled."""
    return frozenset(zoneinfo.available_timezones())


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


#: The month names `%b` / `%B` render. Hard-coded English: mongod does not
#: consult a locale, and `strftime` does, so a machine with a non-English
#: `LC_TIME` used to answer month names no mongod ever emits.
_MONTH_ABBR = (
    "Jan",
    "Feb",
    "Mar",
    "Apr",
    "May",
    "Jun",
    "Jul",
    "Aug",
    "Sep",
    "Oct",
    "Nov",
    "Dec",
)
_MONTH_FULL = (
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
)


def _render_date_format(d: _dt.datetime, fmt: str) -> str:
    """``$dateToString``'s format language, which is NOT ``strftime``.

    This used to hand the format to Python's ``strftime`` after rewriting three
    tokens. Three consequences, all probed against 8.2.11 (2026-09-01):

    * ``strftime`` accepts directives mongod REFUSES. The full accepted set is
      ``%b %d %j %m %u %w %z %B %G %H %L %M %S %U %V %Y %%`` and nothing else;
      everything from ``%a`` to ``%Y``'s neighbours is Location18536. We
      rendered ``%a`` as ``Fri`` where mongod raises, so a typo'd format
      silently produced a wrong string instead of an error.
    * ``%z`` and ``%Z`` came out EMPTY, because the datetime is naive unless a
      timezone was asked for. mongod always has an offset: ``%z`` is ``+0000``
      and ``%Z`` is the offset in MINUTES as a bare integer (``0``, ``330``,
      ``-240``) -- not a zone abbreviation, which is what the name suggests.
    * ``%b`` / ``%B`` were locale-dependent.
    """
    offset = d.utcoffset() or _dt.timedelta(0)
    off_minutes = int(offset.total_seconds()) // 60
    sign = "-" if off_minutes < 0 else "+"
    iso_year, iso_week, _ = d.isocalendar()
    fields = {
        "Y": f"{d.year:04d}",
        "m": f"{d.month:02d}",
        "d": f"{d.day:02d}",
        "H": f"{d.hour:02d}",
        "M": f"{d.minute:02d}",
        "S": f"{d.second:02d}",
        "L": f"{d.microsecond // 1000:03d}",
        "j": f"{d.timetuple().tm_yday:03d}",
        # mongod numbers days 1-Sunday through 7-Saturday; Python's `weekday()`
        # is 0-Monday through 6-Sunday.
        "w": str(((d.weekday() + 1) % 7) + 1),
        "u": str(d.isoweekday()),
        # glibc's `(tm_yday + 7 - tm_wday) / 7`, computed rather than delegated:
        # `strftime("%U")` is the platform's libc, and this format has to answer
        # the same on every platform SecantusDB runs on. The Rust engine
        # computes it, so delegating here also put a libc between two engines
        # the parity suite pins to each other.
        "U": f"{(d.timetuple().tm_yday - 1 + 7 - (d.weekday() + 1) % 7) // 7:02d}",
        "G": f"{iso_year:04d}",
        "V": f"{iso_week:02d}",
        "b": _MONTH_ABBR[d.month - 1],
        "B": _MONTH_FULL[d.month - 1],
        "z": f"{sign}{abs(off_minutes) // 60:02d}{abs(off_minutes) % 60:02d}",
        "Z": str(off_minutes),
        "%": "%",
    }
    out: list[str] = []
    i = 0
    while i < len(fmt):
        ch = fmt[i]
        if ch != "%":
            out.append(ch)
            i += 1
            continue
        directive = fmt[i + 1] if i + 1 < len(fmt) else ""
        if directive not in fields:
            raise ExpressionError(
                f"Invalid format character '%{directive}' in format string",
                code=18536,
                code_name="Location18536",
            )
        out.append(fields[directive])
        i += 2
    return "".join(out)


def _op_date_to_string(arg: Any, ctx: _Ctx) -> Any:
    if not isinstance(arg, Mapping):
        raise ExpressionError("$dateToString requires {date, format}")
    # A non-date, non-null 'date' is mongod Location16006 (was silently null).
    for _k in arg:
        if _k not in ("date", "format", "timezone", "onNull"):
            raise ExpressionError(
                f"Unrecognized argument to $dateToString: {_k}",
                code=18534,
                code_name="Location18534",
            )
    d = _coerce_extractor_date(_eval(arg.get("date"), ctx))
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
    return _render_date_format(d, fmt)


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
        arr,
        f"First argument to $slice must be an array, but is of type: {_bson_type_name(arr)}",
        28724,
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
    # `in` uses Python equality, where `False == 0`, so `{$in: [false, [0]]}`
    # answered true; mongod says false, bool and number being different BSON
    # types. `_set_eq` is the same rule the set operators already use.
    return any(_set_eq(needle, x) for x in haystack)


# `int(very_long_string)` is O(n^2) in CPython. Python 3.11+ enforces a
# default 4300-digit max via `sys.set_int_max_str_digits` (PEP 750), but
# (a) it can be disabled at runtime, (b) the threshold above which it
# bites is not consistent across versions, and (c) we'd rather raise a
# clear ExpressionError than the underlying ValueError. Hard-cap here.
_MAX_INT_STR_DIGITS = 4300


_INT32_MIN, _INT32_MAX = -(2**31), 2**31 - 1
_INT64_MIN, _INT64_MAX = -(2**63), 2**63 - 1
_OVERFLOW_MSG = "Conversion would overflow target type in $convert"

#: Strict integer syntax: an optional sign then ASCII digits, whole string.
#: Python's own ``int()`` is much more permissive -- it strips surrounding
#: whitespace and accepts PEP-515 underscores -- so ``$toInt: " 5 "`` and
#: ``$toInt: "1_0"`` both returned a NUMBER where mongod rejects the string.
#: Wrong values, not wrong messages (measured against 8.2.11, 2026-09-01).
_STRICT_INT_RE = re.compile(r"[+-]?[0-9]+\Z")

#: Strict C ``strtod`` syntax, which is what mongod's double / decimal parsing
#: accepts: decimal or exponent form, plus the infinity and NaN spellings.
#: Deliberately does NOT allow surrounding whitespace or underscores.
_STRICT_FLOAT_RE = re.compile(
    r"[+-]?(?:inf(?:inity)?|nan|(?:[0-9]+\.?[0-9]*|\.[0-9]+)(?:[eE][+-]?[0-9]+)?)\Z",
    re.IGNORECASE,
)

#: The prefix of a numeric string that ``strtod`` WOULD consume. Used only to
#: tell mongod's two "not a number" reasons apart: nothing consumed at all
#: (``"x"``) versus a valid prefix with junk after it (``"12abc"``).
_FLOAT_PREFIX_RE = re.compile(
    r"[+-]?(?:inf(?:inity)?|nan|(?:[0-9]+\.?[0-9]*|\.[0-9]+)(?:[eE][+-]?[0-9]+)?)",
    re.IGNORECASE,
)

#: mongod's hexadecimal gate is a LITERAL `startsWith("0x")` -- lower-case, and
#: with no sign allowed before it. Probed 8.2.11 (2026-09-01): `"0x10"` is
#: "Illegal hexadecimal input", while `"0X10"`, `"-0x10"` and `"+0x10"` all slip
#: past it and are then handled by the ordinary per-target parser. This used to
#: be `[+-]?0[xX]`, which caught all four and reported the hex message for three
#: strings mongod describes differently -- and, for `$toDouble`, refused two it
#: successfully converts.
_HEX_PREFIX_RE = re.compile(r"0x")


#: The spellings that legitimately MEAN infinity, so an infinite parse result
#: is the answer rather than an out-of-range failure.
_INFINITY_SPELLING_RE = re.compile(r"[+-]?inf(?:inity)?\Z", re.IGNORECASE)


#: C99 hexadecimal-float syntax, which `strtod` accepts and `float()` does not.
#: Only reachable for the spellings `_HEX_PREFIX_RE` lets through.
_HEX_FLOAT_RE = re.compile(r"[+-]?0[xX][0-9a-fA-F]*\.?[0-9a-fA-F]*(?:[pP][+-]?[0-9]+)?\Z")


#: The characters an ObjectId string may hold. Case-insensitive: mongod accepts
#: `"507F1F77BCF86CD799439011"`.
_HEX_DIGITS = frozenset("0123456789abcdefABCDEF")


#: Sentinel reason selecting the hexadecimal message shape below.
_HEX_REASON = "<hex>"


def _number_parse_error(value: str, reason: str) -> ExpressionError:
    """mongod's ConversionFailure for an unreadable numeric string.

    Two shapes, both probed on 8.2.11 (2026-09-01)::

        Failed to parse number 'x' in $convert with no onError value: Did not consume whole string.
        Illegal hexadecimal input in $convert with no onError value: 0x10

    The operator is always named ``$convert`` even when the caller wrote
    ``$toInt`` -- mongod routes every conversion through it -- and the reason
    suffix is what this used to omit entirely. It is an ordinary
    ``ExpressionError``, so ``$convert``'s ``onError`` still catches it.
    """
    if reason == _HEX_REASON:
        message = f"Illegal hexadecimal input in $convert with no onError value: {value}"
    else:
        message = f"Failed to parse number '{value}' in $convert with no onError value: {reason}"
    return ExpressionError(message, code=241, code_name="ConversionFailure")


def _parse_int_string(value: str) -> int:
    """Parse ``value`` as mongod's int/long conversion does. Strict."""
    if len(value) > _MAX_INT_STR_DIGITS:
        raise ExpressionError(
            f"$convert input string of {len(value)} chars exceeds the "
            f"{_MAX_INT_STR_DIGITS}-char int-conversion cap"
        )
    if not value:
        raise _number_parse_error(value, "No digits")
    if _HEX_PREFIX_RE.match(value):
        raise _number_parse_error(value, _HEX_REASON)
    if not _STRICT_INT_RE.match(value):
        raise _number_parse_error(value, "Did not consume whole string.")
    return int(value)


def _parse_float_string(value: str) -> float:
    """Parse ``value`` as mongod's double conversion does. Strict."""
    if not value:
        raise _number_parse_error(value, "Empty string")
    if value[0].isspace():
        raise _number_parse_error(value, "Leading whitespace")
    if _HEX_PREFIX_RE.match(value):
        raise _number_parse_error(value, _HEX_REASON)
    if _STRICT_FLOAT_RE.match(value):
        parsed = float(value)
        # `strtod` reports a magnitude it cannot represent as a RANGE error
        # rather than saturating: `$toDouble: "1e400"` is a 241, not `inf`
        # (probed 8.2.11). Python's `float()` happily answers `inf`, so this
        # returned a wrong VALUE. A literal "inf" / "Infinity" spelling is of
        # course still infinity.
        if math.isinf(parsed) and not _INFINITY_SPELLING_RE.match(value):
            raise _number_parse_error(value, "Out of range")
        return parsed
    if True:
        # `strtod` is a C99 parser, so it reads HEXADECIMAL floats too -- the
        # ones the gate above did not catch because they carry a sign or a
        # capital X. mongod converts them: `$toDouble: "0X1f"` is 31.0 and
        # `"-0x10"` is -16.0 (probed 8.2.11). Rejecting them was a wrong answer,
        # not just a wrong message.
        if _HEX_FLOAT_RE.match(value):
            try:
                return float.fromhex(value)
            except ValueError:
                pass
        # Distinguish "strtod consumed nothing" from "strtod consumed a prefix".
        prefix = _FLOAT_PREFIX_RE.match(value)
        if prefix is None or prefix.end() == 0:
            raise _number_parse_error(value, "Did not consume any digits")
        raise _number_parse_error(value, "Did not consume whole string.")


def _parse_decimal_string(value: str) -> Decimal128:
    """Parse ``value`` as mongod's decimal conversion does. Strict.

    Decimal has only ONE failure reason beyond the empty and hex cases -- it
    does not separate "no digits" from "trailing junk" the way double does.
    """
    if not value:
        raise _number_parse_error(value, "Empty string")
    if _HEX_PREFIX_RE.match(value):
        raise _number_parse_error(value, _HEX_REASON)
    if len(value) > _MAX_INT_STR_DIGITS:
        raise ExpressionError(
            f"$convert (decimal) input string of {len(value)} chars "
            f"exceeds the {_MAX_INT_STR_DIGITS}-char cap"
        )
    if not _STRICT_FLOAT_RE.match(value):
        raise _number_parse_error(value, "Failed to parse string to decimal")
    try:
        return Decimal128(value)
    except (InvalidOperation, ValueError) as exc:
        raise _number_parse_error(value, "Failed to parse string to decimal") from exc


def _op_to_int(arg: Any, ctx: _Ctx) -> Any:
    """``$toInt: <expr>`` is exactly ``$convert`` to int.

    Delegates rather than repeating the conversion. The two copies HAD drifted:
    the ``$toX`` side answered its own overflow and unsupported-type messages,
    missed mongod's separate NaN and infinity cases, and one path reached
    ``int(Decimal("Infinity"))`` whose ``OverflowError`` escaped as
    ``1 internal server error`` (found and fixed 2026-09-01).
    """
    value = _eval(arg, ctx)
    if value is None:
        return None
    return _convert_value(value, "int")


def _op_to_long(arg: Any, ctx: _Ctx) -> Any:
    """``$toLong: <expr>`` is exactly ``$convert`` to long.

    Delegates rather than repeating the conversion. The two copies HAD drifted:
    the ``$toX`` side answered its own overflow and unsupported-type messages,
    missed mongod's separate NaN and infinity cases, and one path reached
    ``int(Decimal("Infinity"))`` whose ``OverflowError`` escaped as
    ``1 internal server error`` (found and fixed 2026-09-01).
    """
    value = _eval(arg, ctx)
    if value is None:
        return None
    return _convert_value(value, "long")


def _op_to_double(arg: Any, ctx: _Ctx) -> Any:
    """``$toDouble: <expr>`` is exactly ``$convert`` to double.

    Delegates rather than repeating the conversion. The two copies HAD drifted:
    the ``$toX`` side answered its own overflow and unsupported-type messages,
    missed mongod's separate NaN and infinity cases, and one path reached
    ``int(Decimal("Infinity"))`` whose ``OverflowError`` escaped as
    ``1 internal server error`` (found and fixed 2026-09-01).
    """
    value = _eval(arg, ctx)
    if value is None:
        return None
    return _convert_value(value, "double")


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
        # Every string is true, the empty one included -- `{$toBool: ""}` is
        # true on mongod (probed 8.2.11). This used Python's own truthiness.
        return True
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


#: The target-type NAME mongod uses in an "Unsupported conversion" message,
#: keyed by the numeric BSON type code the target resolves to. A caller may
#: have written the code rather than the name, and mongod always answers with
#: the name.
_CONVERT_TARGET_NAMES = {
    1: "double",
    2: "string",
    7: "objectId",
    8: "bool",
    9: "date",
    16: "int",
    18: "long",
    19: "decimal",
}


def _render_number(value: Any) -> str:
    """A numeric value as mongod prints it in a conversion-overflow message.

    Three different renderings, all probed on 8.2.11 (2026-09-01):

    * a **double** is always ``%g`` -- six significant digits, two-digit
      exponent (``3e+09``, ``2.14748e+09``, ``1.23457e+12``, ``1e+300``). The
      ``abs(value) < 1e16`` guard this used to carry sent every ordinary
      overflow through ``repr`` instead, so ``$toInt: 1e10`` named
      ``10000000000.0`` where mongod names ``1e+10``.
    * an **int64** names NOTHING -- mongod's message ends at the colon and a
      space. Naming the number looked more helpful and was simply not what the
      server says.
    * a **Decimal128** keeps its own rendering (``1E+10``).
    """
    if isinstance(value, float):
        return _fmt_double(value)
    if isinstance(value, int) and not isinstance(value, bool):
        return ""
    return str(value)


def _overflow_error(rendered: str | None = None) -> ExpressionError:
    """mongod's overflow message. The value is named only when it is a NUMBER;
    a string overflow goes through :func:`_number_parse_error` instead."""
    if rendered is None:
        return ExpressionError(_OVERFLOW_MSG, code=241, code_name="ConversionFailure")
    return ExpressionError(
        f"{_OVERFLOW_MSG} with no onError value: {rendered}",
        code=241,
        code_name="ConversionFailure",
    )


def _nan_to_integer_error() -> ExpressionError:
    return ExpressionError(
        "Attempt to convert NaN value to integer type in $convert with no onError value",
        code=241,
        code_name="ConversionFailure",
    )


def _infinity_to_integer_error() -> ExpressionError:
    return ExpressionError(
        "Attempt to convert infinity value to integer type in $convert with no onError value",
        code=241,
        code_name="ConversionFailure",
    )


def _epoch_millis_to_date(millis: float) -> _dt.datetime:
    """Epoch milliseconds -> a naive UTC datetime, the way BSON dates decode."""
    return _dt.datetime.fromtimestamp(millis / 1000.0, tz=_dt.timezone.utc).replace(tzinfo=None)


def _parse_date_string(value: str) -> _dt.datetime:
    """mongod's string -> date conversion.

    The VALUE rules are reproduced (ISO-8601 with an optional time, an optional
    fractional second truncated to milliseconds, ``Z`` or an offset, and
    surrounding whitespace tolerated). The failure TEXT is not: mongod's parser
    reports a per-character diagnosis (``Error parsing date string '20'; 0:
    Unexpected character '2'; 1: Unexpected character '0'``) that depends on how
    far its own state machine got, and a half-right imitation of that would look
    authoritative while being wrong. Every failure here answers the code (241)
    and the general wording mongod uses for a string it cannot start to read.
    """
    text = value.strip()
    if not text:
        raise ExpressionError(
            # The character in mongod's message is a literal NUL, not a space.
            f"Error parsing date string '{value}'; 0: Empty string '\x00'",
            code=241,
            code_name="ConversionFailure",
        )
    candidate = text[:-1] + "+00:00" if text.endswith("Z") else text
    # mongod truncates a sub-millisecond fraction rather than rejecting it;
    # `fromisoformat` accepts only 3 or 6 fractional digits.
    if "." in candidate:
        head, _, tail = candidate.partition(".")
        digits = ""
        while tail and tail[0].isdigit():
            digits, tail = digits + tail[0], tail[1:]
        if digits:
            # BSON dates hold MILLISECONDS, so mongod truncates the fraction to
            # three digits rather than rejecting a longer one:
            # ``...00.1234567Z`` is 123 ms, not 123456 us.
            candidate = f"{head}.{digits[:3].ljust(3, '0')}000{tail}"
    for form in (candidate, f"{candidate}-01" if len(candidate) == 7 else candidate):
        try:
            parsed = _dt.datetime.fromisoformat(form)
        except ValueError:
            continue
        if parsed.tzinfo is not None:
            parsed = parsed.astimezone(_dt.timezone.utc).replace(tzinfo=None)
        return parsed
    raise ExpressionError(
        f'an incomplete date/time string has been found, with elements missing: "{value}"',
        code=241,
        code_name="ConversionFailure",
    )


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
            return _parse_float_string(value)
        if isinstance(value, _dt.datetime):
            return value.timestamp() * 1000.0
    elif code == 2:
        # ``str()`` is Python's rendering, not BSON's: it prints ``True`` for a
        # bool and ``inf`` / ``nan`` for the non-finite doubles, and it happily
        # stringifies an array or a document that mongod refuses outright
        # (probed 8.2.11).
        if isinstance(value, _dt.datetime):
            return value.isoformat()
        if isinstance(value, bool):
            return "true" if value else "false"
        if isinstance(value, float):
            if math.isnan(value):
                return "NaN"
            if math.isinf(value):
                return "Infinity" if value > 0 else "-Infinity"
            return _render_number(value)
        if isinstance(value, (str, int, Decimal128, ObjectId)):
            return str(value)
    elif code == 7:
        if isinstance(value, _ObjectId):
            return value
        if isinstance(value, str):
            try:
                return _ObjectId(value)
            except Exception as exc:
                # mongod reports the LENGTH only when the length is actually
                # wrong; a 24-character string with a non-hex character in it
                # names that CHARACTER instead (probed 8.2.11, 2026-09-01 --
                # `"z" * 24` says "Invalid character found in hex string: z").
                # Reporting "expected 24 but found 24" was a nonsense sentence.
                if len(value) == 24:
                    bad = next(c for c in value if c not in _HEX_DIGITS)
                    reason = f"Invalid character found in hex string: {bad}"
                else:
                    reason = (
                        f"Invalid string length for parsing to OID, expected 24 "
                        f"but found {len(value)}"
                    )
                raise ExpressionError(
                    f"Failed to parse objectId '{value}' in $convert with no onError "
                    f"value: {reason}",
                    code=241,
                    code_name="ConversionFailure",
                ) from exc
    elif code == 8:
        if isinstance(value, bool):
            return value
        if isinstance(value, (int, float)):
            return value != 0
        if isinstance(value, Decimal128):
            return value.to_decimal() != Decimal(0)
        if isinstance(value, str):
            # EVERY string is true, the empty one included (probed 8.2.11) --
            # this is BSON truthiness, not Python's.
            return True
        return True
    elif code == 9:
        # bool and int32 are BOTH rejected (probed 8.2.11): only a LONG is
        # epoch milliseconds. We accepted a plain int, so `{$toDate: 1}`
        # answered 1970-01-01T00:00:00.001Z where mongod refuses the
        # conversion outright -- a wrong value, not a wrong message.
        if isinstance(value, bool):
            pass  # falls through to the unsupported-conversion tail
        elif isinstance(value, _dt.datetime):
            return value
        elif isinstance(value, ObjectId):
            return value.generation_time.replace(tzinfo=None)
        elif isinstance(value, Timestamp):
            return _dt.datetime.fromtimestamp(value.time, tz=_dt.timezone.utc).replace(tzinfo=None)
        elif isinstance(value, int):
            # A LONG is epoch milliseconds; an int32 is not convertible at all
            # (probed 8.2.11). The test is the BSON width, not the Python type:
            # a plain Python ``int`` too large for int32 IS a long on the wire,
            # and ``_bson_type_name`` already calls it one.
            if not isinstance(value, Int64) and _INT32_MIN <= value <= _INT32_MAX:
                pass  # falls through to the unsupported-conversion tail
            else:
                return _epoch_millis_to_date(int(value))
        elif isinstance(value, float):
            return _epoch_millis_to_date(value)
        elif isinstance(value, Decimal128):
            return _epoch_millis_to_date(float(value.to_decimal()))
        elif isinstance(value, str):
            return _parse_date_string(value)
    elif code in (16, 18):
        # 16 = int32, 18 = int64. Wrap as ``Int64`` for code 18 so the
        # result matches ``$type: "long"`` downstream — the bson decoder
        # preserves the int32/int64 distinction by type, and ``$convert``
        # must respect the requested target type.
        lo, hi = (_INT64_MIN, _INT64_MAX) if code == 18 else (_INT32_MIN, _INT32_MAX)

        def _wrap(n: int, rendered: str | None = None) -> int:
            if not lo <= n <= hi:
                raise _overflow_error(rendered)
            return Int64(n) if code == 18 else int(n)

        if isinstance(value, _dt.datetime) and code == 18:
            # A date is its epoch milliseconds -- but only for the LONG target.
            # `$toInt` of a date is `241 Unsupported conversion from date to
            # int`, so this cannot be one "numeric" arm (probed 8.2.11,
            # 2026-09-02, where `$toLong` of a date answered 241 here).
            return _wrap(int(value.replace(tzinfo=_dt.timezone.utc).timestamp() * 1000))
        if isinstance(value, bool):
            return _wrap(1 if value else 0)
        if isinstance(value, int):
            return _wrap(int(value), _render_number(value))
        if isinstance(value, float):
            # mongod separates the three ways a float refuses to be an integer:
            # NaN, infinity, and merely out of range each get their own message
            # (probed 8.2.11). One ``_OVERFLOW_MSG`` covered all three.
            if math.isnan(value):
                raise _nan_to_integer_error()
            if math.isinf(value):
                raise _infinity_to_integer_error()
            return _wrap(int(value), _render_number(value))
        if isinstance(value, Decimal128):
            dec = value.to_decimal()
            if dec.is_nan():
                raise _nan_to_integer_error()
            if dec.is_infinite():
                # This used to reach ``int(Decimal("Infinity"))``, whose
                # ``OverflowError`` escaped the handler and answered the client
                # ``1 internal server error``.
                raise _infinity_to_integer_error()
            return _wrap(int(dec), _render_number(value))
        if isinstance(value, str):
            n = _parse_int_string(value)
            if not lo <= n <= hi:
                # From a STRING, mongod reports the overflow as a parse
                # failure, with the original text -- not as the conversion
                # overflow a numeric input gets.
                raise _number_parse_error(value, "Overflow")
            return Int64(n) if code == 18 else int(n)
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
            return _parse_decimal_string(value)
        if isinstance(value, _dt.datetime):
            # Epoch milliseconds, like the long and double targets. `$toInt` of
            # a date is still 241 -- so this is not one "numeric" arm.
            return Decimal128(
                Decimal(int(value.replace(tzinfo=_dt.timezone.utc).timestamp() * 1000))
            )
    raise ExpressionError(
        f"Unsupported conversion from {_bson_type_name(value)} to "
        f"{_CONVERT_TARGET_NAMES.get(code, target)} in $convert with no onError value",
        code=241,
        code_name="ConversionFailure",
    )


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
    """``$toDecimal: <expr>`` is exactly ``$convert`` to decimal.

    Delegates rather than repeating the conversion. The two copies HAD drifted:
    the ``$toX`` side answered its own overflow and unsupported-type messages,
    missed mongod's separate NaN and infinity cases, and one path reached
    ``int(Decimal("Infinity"))`` whose ``OverflowError`` escaped as
    ``1 internal server error`` (found and fixed 2026-09-01).
    """
    value = _eval(arg, ctx)
    if value is None:
        return None
    return _convert_value(value, "decimal")


def _op_to_object_id(arg: Any, ctx: _Ctx) -> Any:
    """``$toObjectId: <expr>`` is ``$convert: {input: <expr>, to: "objectId"}``.

    It was simply MISSING -- the whole operator answered ``Unrecognized
    expression '$toObjectId'`` (168), which is what mongod says for an operator
    that does not exist rather than one it ships (found 2026-09-01).
    """
    value = _eval(arg, ctx)
    if value is None:
        return None
    return _convert_value(value, "objectId")


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


# The set operators mongod requires at least two arguments for, checked before
# it looks at their types.
# Only `$setEquals`. Its siblings accept a single array and report a
# non-array operand without counting arguments first (probed 8.2.11).
_SET_OPS_NEEDING_TWO = frozenset({"$setEquals"})


# The set operators for which a NULL operand makes the whole expression null,
# rather than being reported as a wrong type.
#
# Not uniform, and not guessable: `$setUnion` / `$setIntersection` /
# `$setDifference` answer null, while `$setEquals` and `$setIsSubset` refuse it
# by type (`{$setIsSubset: [null, [1]]}` is 17046 naming `null`). Operands are
# scanned LEFT TO RIGHT, so `{$setUnion: [null, 1]}` is null while
# `{$setUnion: [1, null]}` raises on the int -- the order decides which rule
# fires. Probed 8.2.11 (2026-09-02); before this every one of them raised.
_SET_OPS_NULL_IS_NULL = frozenset({"$setUnion", "$setIntersection", "$setDifference"})

#: `(first, second)` Location codes for the two-operand set operators, which
#: report a wrong-typed operand under a DIFFERENT code per position.
_SET_OPS_BY_POSITION = {
    "$setDifference": (17048, 17049),
    "$setIsSubset": (17046, 17042),
}


def _set_arrays(op: str, arg: Any, ctx: _Ctx, *, n: int | None = None) -> list[list[Any]] | None:
    """Evaluate a set operator's array arguments, validating each is an array
    (mongod's per-operator Location code, not a generic TypeMismatch).

    ``None`` when a null operand makes the whole expression null."""
    vals = _eval_args(arg, ctx)
    if n is not None and len(vals) != n:
        raise ExpressionError(f"{op} requires {n} arguments")
    # ARITY first: `{$setEquals: [[1]]}` is "needs at least two arguments", not
    # a complaint about the one array it did get (probed 8.2.11).
    if op in _SET_OPS_NEEDING_TWO and len(vals) < 2:
        raise ExpressionError(
            f"{op} needs at least two arguments had: {len(vals)}",
            code=17045,
            code_name="Location17045",
        )
    code = _SET_OP_CODES.get(op, 14)
    for i, v in enumerate(vals):
        if v is None and op in _SET_OPS_NULL_IS_NULL:
            return None
        if not isinstance(v, list):
            if op in _SET_OPS_BY_POSITION:
                # One code per POSITION, not one per operator: `$setDifference`
                # is 17048 for the first operand and 17049 for the second,
                # `$setIsSubset` 17046 and 17042. Probed 8.2.11 (2026-09-02);
                # both used to report the first-argument code either way.
                first_code, second_code = _SET_OPS_BY_POSITION[op]
                code = first_code if i == 0 else second_code
                which = "First" if i == 0 else "Second"
                msg = (
                    f"both operands of {op} must be arrays. {which} argument is of type: "
                    f"{_bson_type_name(v)}"
                )
            else:
                # `$setEquals` NUMBERS the argument (1-based) and uses its own
                # 5887502; `$setUnion` / `$setIntersection` say "One argument"
                # and keep their own codes. Again: not one family.
                if op == "$setEquals":
                    raise ExpressionError(
                        f"All operands of {op} must be arrays. {i + 1}-th argument is of "
                        f"type: {_bson_type_name(v)}",
                        code=5887502,
                        code_name="Location5887502",
                    )
                msg = (
                    f"All operands of {op} must be arrays. One argument is of type: "
                    f"{_bson_type_name(v)}"
                )
            raise ExpressionError(msg, code=code, code_name=f"Location{code}")
    return vals


def _op_set_union(arg: Any, ctx: _Ctx) -> list[Any] | None:
    arrays = _set_arrays("$setUnion", arg, ctx)
    if arrays is None:
        return None
    all_elems: list[Any] = []
    for v in arrays:
        all_elems.extend(v)
    return _set_dedup_sorted(all_elems)


def _op_set_intersection(arg: Any, ctx: _Ctx) -> list[Any] | None:
    arrays = _set_arrays("$setIntersection", arg, ctx)
    if arrays is None:
        return None
    if not arrays:
        return []
    result = [
        x for x in arrays[0] if all(any(_set_eq(x, y) for y in other) for other in arrays[1:])
    ]
    return _set_dedup_sorted(result)


def _op_set_difference(arg: Any, ctx: _Ctx) -> list[Any] | None:
    arrays = _set_arrays("$setDifference", arg, ctx, n=2)
    if arrays is None:
        return None
    a, b = arrays
    out: list[Any] = []
    for x in a:  # first-array order, deduplicated
        if not any(_set_eq(x, y) for y in b) and not any(_set_eq(x, y) for y in out):
            out.append(x)
    return out


def _op_set_equals(arg: Any, ctx: _Ctx) -> bool:
    arrays = _set_arrays("$setEquals", arg, ctx) or []
    base = _set_dedup_sorted(arrays[0]) if arrays else []
    for other in arrays[1:]:
        o = _set_dedup_sorted(other)
        if len(o) != len(base) or any(not _set_eq(base[i], o[i]) for i in range(len(base))):
            return False
    return True


def _op_set_is_subset(arg: Any, ctx: _Ctx) -> bool:
    a, b = _set_arrays("$setIsSubset", arg, ctx, n=2)  # type: ignore[misc]
    return all(any(_set_eq(x, y) for y in b) for x in a)


def _op_all_elements_true(arg: Any, ctx: _Ctx) -> bool:
    # `_eval` on the single operand, not `_eval_args(..)[0]`: `_apply_op`
    # already unwraps the one-element list form, so the array arrives
    # directly and `_eval_args` would iterate ITS elements instead.
    arr = _eval(arg, ctx)
    if not isinstance(arr, list):
        raise ExpressionError(
            f"$allElementsTrue's argument must be an array, but is {_bson_type_name(arr)}",
            code=17040,
            code_name="Location17040",
        )
    return all(_bool(x) for x in arr)


def _op_any_element_true(arg: Any, ctx: _Ctx) -> bool:
    # `_eval` on the single operand, not `_eval_args(..)[0]`: `_apply_op`
    # already unwraps the one-element list form, so the array arrives
    # directly and `_eval_args` would iterate ITS elements instead.
    arr = _eval(arg, ctx)
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

    a, b = _cmp_pair(arg, ctx)
    if a is _MISSING_RANK or b is _MISSING_RANK:
        if a is b:
            return 0
        return -1 if a is _MISSING_RANK else 1
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
    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$degreesToRadians")
    if _has_decimal(v):
        return _decimal_result(lambda d: d * _PI / _decimal.Decimal(180), v)
    # `x * (pi/180)`, not `x * pi / 180`: mongod multiplies by a single
    # precomputed constant, and the two associations differ in the last bit
    # (1.5 degrees -> 0.026179938779914945, not ...94). Probed 8.2.11.
    return float(v) * _RADIANS_PER_DEGREE


def _op_radians_to_degrees(arg: Any, ctx: _Ctx) -> Any:
    v = _eval(arg, ctx)
    if v is None:
        return None
    _require_math_numeric(v, "$radiansToDegrees")
    if _has_decimal(v):
        return _decimal_result(lambda d: d * _decimal.Decimal(180) / _PI, v)
    # One precomputed constant, as `$degreesToRadians` above.
    return float(v) * _DEGREES_PER_RADIAN


def _bit_operand(op: str, v: Any) -> tuple[int, bool]:
    """Coerce a ``$bit*`` operand to ``(value, is_long)``. mongod's bitwise
    operators accept only int (32-bit) and long (64-bit) — a bool, double,
    decimal, or anything else raises. ``bson.Int64`` marks a long; a plain ``int``
    is a 32-bit int (``bson`` widens on encode only when out of int32 range)."""
    # A bool is not an operand for any of them, but the four do NOT agree on
    # how to say so, and this raised one sentence for all of them:
    # `{$bitOr: [1, true]}` is the fold family's bare 14 "only supports int and
    # long operands." with NO type named, while `$bitNot` calls a bool
    # non-numeric (28765). Probed 8.2.11 (2026-09-02). Checked before the `int`
    # arm below because `isinstance(True, int)` is true in Python.
    if isinstance(v, bool):
        if op != "$bitNot":
            raise ExpressionError(f"{op} only supports int and long operands.")
        raise ExpressionError(
            f"{op} only supports numeric types, not bool",
            code=28765,
            code_name="Location28765",
        )
    if isinstance(v, Int64):
        return int(v), True
    if isinstance(v, int):
        return v, False
    # `$bitNot` alone splits by whether the operand is a NUMBER at all: a
    # non-numeric type is 28765 "only supports numeric types, not string", while
    # a double or decimal is the bare 14 "only supports int and long, not:
    # double." (trailing period included). Its three siblings name NO type at
    # all and always answer 14 "only supports int and long operands." -- the
    # family looks uniform and is not (probed 8.2.11, all four).
    if op != "$bitNot":
        raise ExpressionError(f"{op} only supports int and long operands.")
    if isinstance(v, (float, Decimal128)):
        raise ExpressionError(f"{op} only supports int and long, not: {_bson_type_name(v)}.")
    raise ExpressionError(
        f"{op} only supports numeric types, not {_bson_type_name(v)}",
        code=28765,
        code_name="Location28765",
    )


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
    values = [x for x in _expr_acc_values(arg, ctx) if _expr_is_number(x)]
    if not values:
        return None
    # A Decimal128 anywhere makes the whole average decimal128: `total += x`
    # raised a TypeError against a `Decimal128`, which surfaced as an
    # `internal server error`.
    if _has_decimal(*values):
        return _decimal_result(
            lambda *ds: sum(ds[1:], ds[0]) / _decimal.Decimal(len(values)), *values
        )
    total: Any = 0
    for x in values:
        total += x
    return total / len(values)


def _op_expr_std_dev_pop(arg: Any, ctx: _Ctx) -> Any:
    return _expr_std_dev(arg, ctx, pop=True)


def _op_expr_std_dev_samp(arg: Any, ctx: _Ctx) -> Any:
    return _expr_std_dev(arg, ctx, pop=False)


def _expr_std_dev(arg: Any, ctx: _Ctx, *, pop: bool) -> Any:
    """``$stdDevPop`` / ``$stdDevSamp`` in EXPRESSION position.

    The accumulator forms shipped long ago; the expression forms -- over an
    array argument in ``$project`` / ``$addFields`` -- did not, and answered
    ``Unknown expression`` where mongod computes (probed 8.2.11, 2026-09-01:
    ``{$stdDevPop: [1, 2, 3]}`` is ``0.816496580927726`` and ``$stdDevSamp`` is
    ``1.0``). Shares ``aggregate._std_dev`` with the accumulators so the two
    forms cannot answer different numbers, and non-numeric members are dropped
    exactly as the accumulator drops them.
    """
    from secantus.aggregate import _std_dev, _std_dev_operand

    values = [
        _std_dev_operand(x)
        for x in _expr_acc_values(arg, ctx)
        if _expr_is_number(x) and not isinstance(x, bool)
    ]
    return _std_dev(values, pop=pop)


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
    "$stdDevPop": _op_expr_std_dev_pop,
    "$stdDevSamp": _op_expr_std_dev_samp,
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
    "$toObjectId": _op_to_object_id,
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


_MISSING_PROPAGATING.update(
    {
        "$cond": _op_cond,
        "$switch": _op_switch,
        "$let": _op_let,
        "$ifNull": _op_if_null,
    }
)
