from __future__ import annotations

import datetime as _dt
import math
import re
from collections.abc import Callable, Mapping
from decimal import Decimal, InvalidOperation
from functools import lru_cache
from typing import Any

from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp

from secantus.bsontypes import Int64CoercionError, coerce_int64_argument
from secantus.bsontypes import bson_value_repr as _mongo_bson_repr
from secantus.bsontypes import fmt_double_g as _fmt_g
from secantus.collation import Collation
from secantus.collation import compare_keys as _coll_compare
from secantus.collation import equal as _coll_equal


class _Missing:
    _instance: _Missing | None = None

    def __new__(cls) -> _Missing:
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def __repr__(self) -> str:
        return "MISSING"


MISSING: Any = _Missing()


class QueryError(Exception):
    """An invalid query the server rejects at parse time.

    ``code`` / ``code_name`` default to mongod's generic ``2 BadValue``;
    operators with a documented distinct code (``$jsonSchema``'s keyword
    validation: 9 FailedToParse / 14 TypeMismatch) set their own.
    """

    def __init__(self, message: str, *, code: int = 2, code_name: str = "BadValue") -> None:
        super().__init__(message)
        self.code = code
        self.code_name = code_name


def matches(
    doc: Mapping[str, Any],
    query: Mapping[str, Any],
    *,
    vars: dict[str, Any] | None = None,
    collation: Collation | None = None,
) -> bool:
    if not query:
        return True
    return all(_match_clause(doc, k, v, vars, collation) for k, v in query.items())


def matches_batch(
    docs: list[Mapping[str, Any]],
    query: Mapping[str, Any],
    *,
    vars: dict[str, Any] | None = None,
    collation: Collation | None = None,
) -> list[bool]:
    """Filter ``docs`` against ``query`` in one shot."""
    if not query:
        return [True] * len(docs)
    return [matches(d, query, vars=vars, collation=collation) for d in docs]


def _match_clause(
    doc: Mapping[str, Any],
    key: str,
    condition: Any,
    vars: dict[str, Any] | None,
    collation: Collation | None,
) -> bool:
    if key in ("$and", "$or", "$nor"):
        # mongod requires a non-empty array of sub-documents. A non-list (or a
        # non-document element) is a parse error — BadValue (2) — NOT an
        # unhandled iteration crash. Without this guard ``for c in condition``
        # raised ``TypeError: '<type>' object is not iterable`` for e.g.
        # ``{$or: true}``, which leaked out of the QueryError catch and
        # surfaced as a generic InternalError (1) instead of mongod's BadValue.
        if not isinstance(condition, list):
            raise QueryError(f"{key} argument must be an array")
        if not condition:
            raise QueryError(f"{key} argument must be a non-empty array")
        for c in condition:
            if not isinstance(c, Mapping):
                raise QueryError(f"{key} argument's entries must be objects")
        if key == "$and":
            return all(matches(doc, c, vars=vars, collation=collation) for c in condition)
        if key == "$or":
            return any(matches(doc, c, vars=vars, collation=collation) for c in condition)
        return not any(matches(doc, c, vars=vars, collation=collation) for c in condition)
    if key == "$expr":
        from secantus.expressions import evaluate

        return _truthy(evaluate(condition, doc, vars))
    if key == "$comment":
        return True
    if key == "$jsonSchema":
        # Both the keyword check and the validation recurse into every
        # sub-schema (properties / items / allOf / anyOf / oneOf / not / ...),
        # so a pathologically deeply-nested schema can exhaust the recursion
        # limit. Translate that into a typed FailedToParse the dispatch layer
        # turns into a clean error reply, rather than letting the RecursionError
        # escape to the blanket handler as a generic InternalError. (security
        # review 2026-07-20, I21.)
        try:
            _check_json_schema_keywords(condition)
            return _validate_json_schema(doc, condition)
        except RecursionError as exc:
            raise QueryError(
                "$jsonSchema nesting is too deep",
                code=9,
                code_name="FailedToParse",
            ) from exc
    if key.startswith("$"):
        raise QueryError(
            f"unknown top level operator: {key}. If you have a field name that "
            "starts with a '$' symbol, consider using $getField or $setField."
        )
    return _field_matches(_resolve_path(doc, key), condition, collation, field=key)


def _unique_items_key(value: Any) -> Any:
    """A hashable canonical key for ``uniqueItems`` duplicate detection.

    Numerics collapse to a common ``("n", Decimal)`` form so int / long /
    double / Decimal128 compare by value; sub-documents and sub-arrays recurse
    so nested cross-type-equal numerics collide too.
    """
    if isinstance(value, bool):
        return ("b", value)
    if isinstance(value, (int, float)):
        return ("n", Decimal(str(value)))
    if isinstance(value, Decimal128):
        return ("n", value.to_decimal())
    if isinstance(value, Mapping):
        return ("d", tuple((k, _unique_items_key(v)) for k, v in value.items()))
    if isinstance(value, (list, tuple)):
        return ("a", tuple(_unique_items_key(v) for v in value))
    if isinstance(value, bytes):
        return ("y", bytes(value))
    return ("s", type(value).__name__, value)


# Every $jsonSchema keyword mongod 7.0 accepts (title / description are
# accepted-and-ignored metadata). Verified against a real mongod probe
# (2026-07-17): an unknown keyword is `9 FailedToParse "Unknown $jsonSchema
# keyword: <kw>"`; a known-but-unsupported JSON-Schema keyword is
# `9 FailedToParse "$jsonSchema keyword '<kw>' is not currently supported"`.
_JSON_SCHEMA_KEYWORDS = frozenset(
    {
        "additionalItems",
        "additionalProperties",
        "allOf",
        "anyOf",
        "bsonType",
        "dependencies",
        "description",
        "enum",
        "exclusiveMaximum",
        "exclusiveMinimum",
        "items",
        "maxItems",
        "maxLength",
        "maxProperties",
        "maximum",
        "minItems",
        "minLength",
        "minProperties",
        "minimum",
        "multipleOf",
        "not",
        "oneOf",
        "pattern",
        "patternProperties",
        "properties",
        "required",
        "title",
        "type",
        "uniqueItems",
    }
)
_JSON_SCHEMA_UNSUPPORTED = frozenset({"$ref", "$schema", "default", "definitions", "format", "id"})


def _check_json_schema_keywords(schema: Any) -> None:
    """Parse-time $jsonSchema validation, recursing into every sub-schema —
    mongod rejects the whole query before matching a single document. Codes and
    messages are verbatim from a mongod 7.0 probe (including the grammar quirk
    in the exclusive-bound message)."""
    if not isinstance(schema, Mapping):
        raise QueryError("$jsonSchema must be an object", code=14, code_name="TypeMismatch")
    for kw, arg in schema.items():
        if kw in _JSON_SCHEMA_UNSUPPORTED:
            raise QueryError(
                f"$jsonSchema keyword '{kw}' is not currently supported",
                code=9,
                code_name="FailedToParse",
            )
        if kw not in _JSON_SCHEMA_KEYWORDS:
            raise QueryError(
                f"Unknown $jsonSchema keyword: {kw}", code=9, code_name="FailedToParse"
            )
        if kw in ("title", "description") and not isinstance(arg, str):
            raise QueryError(
                f"$jsonSchema keyword '{kw}' must be of type string",
                code=14,
                code_name="TypeMismatch",
            )
        if kw == "multipleOf":
            if isinstance(arg, bool) or not isinstance(arg, (int, float)):
                raise QueryError(
                    "$jsonSchema keyword 'multipleOf' must be a number",
                    code=14,
                    code_name="TypeMismatch",
                )
            if arg <= 0:
                raise QueryError(
                    "$jsonSchema keyword 'multipleOf' must have a positive value",
                    code=9,
                    code_name="FailedToParse",
                )
        if kw in ("exclusiveMinimum", "exclusiveMaximum"):
            if not isinstance(arg, bool):
                raise QueryError(
                    f"$jsonSchema keyword '{kw}' must be a boolean",
                    code=14,
                    code_name="TypeMismatch",
                )
            bound = "minimum" if kw == "exclusiveMinimum" else "maximum"
            if bound not in schema:
                raise QueryError(
                    f"$jsonSchema keyword '{bound}' must be a present if {kw} is present",
                    code=9,
                    code_name="FailedToParse",
                )
        # Recurse into sub-schemas.
        if kw in ("properties", "patternProperties") and isinstance(arg, Mapping):
            for sub in arg.values():
                _check_json_schema_keywords(sub)
        elif kw in ("additionalProperties", "additionalItems"):
            if isinstance(arg, Mapping):
                _check_json_schema_keywords(arg)
        elif kw == "not":
            _check_json_schema_keywords(arg)
        elif kw == "items":
            for sub in arg if isinstance(arg, list) else [arg]:
                _check_json_schema_keywords(sub)
        elif kw in ("allOf", "anyOf", "oneOf") and isinstance(arg, list):
            for sub in arg:
                _check_json_schema_keywords(sub)
        elif kw == "dependencies" and isinstance(arg, Mapping):
            for dep in arg.values():
                if isinstance(dep, Mapping):
                    _check_json_schema_keywords(dep)


def _validate_json_schema(value: Any, schema: Any) -> bool:
    if not isinstance(schema, Mapping):
        return False
    if "bsonType" in schema:
        types = schema["bsonType"]
        if not isinstance(types, list):
            types = [types]
        if not any(_matches_type(value, t) for t in types):
            return False
    if "type" in schema:
        types = schema["type"]
        if not isinstance(types, list):
            types = [types]
        if not any(_matches_json_type(value, t) for t in types):
            return False
    if "enum" in schema and value not in schema["enum"]:
        return False
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        # Draft-4 semantics, matching mongod: exclusiveMinimum / exclusiveMaximum
        # are BOOLEANS that sharpen minimum / maximum to a strict bound (the
        # draft-6 numeric form is rejected at parse time).
        if "minimum" in schema:
            m = schema["minimum"]
            if value < m or (schema.get("exclusiveMinimum") is True and value == m):
                return False
        if "maximum" in schema:
            m = schema["maximum"]
            if value > m or (schema.get("exclusiveMaximum") is True and value == m):
                return False
        if "multipleOf" in schema and math.fmod(value, schema["multipleOf"]) != 0:
            return False
    if isinstance(value, str):
        if "minLength" in schema and len(value) < schema["minLength"]:
            return False
        if "maxLength" in schema and len(value) > schema["maxLength"]:
            return False
        # Route through _compile_regex so the pattern-length cap fires.
        if "pattern" in schema and not _compile_regex(schema["pattern"], 0).search(value):
            return False
    if isinstance(value, list):
        if "minItems" in schema and len(value) < schema["minItems"]:
            return False
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            return False
        if "items" in schema:
            items = schema["items"]
            if isinstance(items, list):
                # Tuple validation: position i validates against items[i];
                # elements past the tuple validate against additionalItems
                # (False = none allowed; a schema = each must match; absent =
                # anything goes). Matches the mongod probe.
                for i, item in enumerate(value):
                    if i < len(items):
                        if not _validate_json_schema(item, items[i]):
                            return False
                    else:
                        extra = schema.get("additionalItems")
                        if extra is False:
                            return False
                        if isinstance(extra, Mapping) and not _validate_json_schema(item, extra):
                            return False
            else:
                for item in value:
                    if not _validate_json_schema(item, items):
                        return False
        if schema.get("uniqueItems") is True:
            # Every element must be distinct under MongoDB value equality, which
            # bridges cross-type-equal numerics (int/long/double/Decimal128 by
            # value) recursively inside sub-documents and sub-arrays — so
            # ``{a: 1}`` and ``{a: 1.0}`` collide, matching real mongod.
            seen: set[Any] = set()
            for item in value:
                key = _unique_items_key(item)
                if key in seen:
                    return False
                seen.add(key)
    if isinstance(value, Mapping):
        if "required" in schema:
            for required_key in schema["required"]:
                if required_key not in value:
                    return False
        if "properties" in schema:
            for prop, prop_schema in schema["properties"].items():
                if prop in value and not _validate_json_schema(value[prop], prop_schema):
                    return False
        if "minProperties" in schema and len(value) < schema["minProperties"]:
            return False
        if "maxProperties" in schema and len(value) > schema["maxProperties"]:
            return False
        pattern_props = schema.get("patternProperties")
        pattern_res = []
        if isinstance(pattern_props, Mapping):
            for pat, sub_schema in pattern_props.items():
                rx = _compile_regex(pat, 0)  # search semantics; length cap fires
                pattern_res.append(rx)
                for k, v in value.items():
                    if rx.search(k) and not _validate_json_schema(v, sub_schema):
                        return False
        if "additionalProperties" in schema:
            # "Additional" = a key not named in ``properties`` and not matching any
            # ``patternProperties`` regex.
            ap = schema["additionalProperties"]
            named = set(schema.get("properties", {}))
            extras = [
                k for k in value if k not in named and not any(rx.search(k) for rx in pattern_res)
            ]
            if ap is False and extras:
                return False
            if isinstance(ap, Mapping):
                for k in extras:
                    if not _validate_json_schema(value[k], ap):
                        return False
        deps = schema.get("dependencies")
        if isinstance(deps, Mapping):
            for prop, dep in deps.items():
                if prop not in value:
                    continue
                if isinstance(dep, list):
                    if any(req not in value for req in dep):  # property dependency
                        return False
                elif isinstance(dep, Mapping) and not _validate_json_schema(value, dep):
                    return False  # schema dependency
    # Logical combinators apply to the value regardless of its type.
    if "allOf" in schema and not all(_validate_json_schema(value, s) for s in schema["allOf"]):
        return False
    if "anyOf" in schema and not any(_validate_json_schema(value, s) for s in schema["anyOf"]):
        return False
    if "oneOf" in schema and sum(_validate_json_schema(value, s) for s in schema["oneOf"]) != 1:
        return False
    return not ("not" in schema and _validate_json_schema(value, schema["not"]))


def _matches_json_type(value: Any, json_type: str) -> bool:
    if json_type == "string":
        return isinstance(value, str)
    if json_type == "number":
        return isinstance(value, (int, float, Decimal128)) and not isinstance(value, bool)
    if json_type == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if json_type == "boolean":
        return isinstance(value, bool)
    if json_type == "null":
        return value is None
    if json_type == "array":
        return isinstance(value, list)
    if json_type == "object":
        return isinstance(value, Mapping)
    return False


def _truthy(value: Any) -> bool:
    if value is None:
        return False
    if isinstance(value, (bool, int, float)):
        return bool(value)
    if isinstance(value, Decimal128):
        # mongod reads a Decimal128 NUMERICALLY here, so `$exists:
        # Decimal128("0")` is falsy and matches nothing (probed 8.2.11,
        # 2026-09-01). Falling through to the catch-all made every Decimal128
        # truthy, including zero.
        try:
            dec = value.to_decimal()
        except (InvalidOperation, ValueError):
            return True
        return dec.is_nan() or bool(dec)
    return True


def _resolve_path(doc: Any, path: str) -> list[Any]:
    parts = path.split(".")
    current: list[Any] = [doc]
    for part in parts:
        nxt: list[Any] = []
        for cur in current:
            if isinstance(cur, Mapping):
                nxt.append(cur.get(part, MISSING))
            elif isinstance(cur, list):
                if part.isdigit():
                    idx = int(part)
                    nxt.append(cur[idx] if 0 <= idx < len(cur) else MISSING)
                else:
                    for elem in cur:
                        if isinstance(elem, Mapping):
                            nxt.append(elem.get(part, MISSING))
            else:
                nxt.append(MISSING)
        current = nxt
    return current


# Operator-clause keys that ride alongside another operator rather than
# being operators themselves (hoisted: this used to be rebuilt per doc
# per clause inside _field_matches).
_SIBLING_MODIFIERS = frozenset(("$options", "$maxDistance", "$minDistance"))

#: Every field-level operator `_op_matches` dispatches, plus the sibling
#: modifiers that tune one. `$not` validates its inner document against this:
#: mongod requires those keys to be operators, and a document of ordinary field
#: names used to degrade to an equality match that `$not` then negated.
_KNOWN_FIELD_OPERATORS = _SIBLING_MODIFIERS | frozenset(
    (
        "$eq",
        "$ne",
        "$gt",
        "$gte",
        "$lt",
        "$lte",
        "$in",
        "$nin",
        "$exists",
        "$not",
        "$type",
        "$size",
        "$all",
        "$mod",
        "$elemMatch",
        "$regex",
        "$bitsAllSet",
        "$bitsAnySet",
        "$bitsAllClear",
        "$bitsAnyClear",
        "$geoWithin",
        "$geoIntersects",
        "$near",
        "$nearSphere",
    )
)


def _field_matches(
    values: list[Any],
    condition: Any,
    collation: Collation | None = None,
    *,
    field: str = "",
) -> bool:
    if isinstance(condition, Regex):
        return _op_regex(values, condition.pattern, condition.flags)
    if isinstance(condition, Mapping) and condition and all(k.startswith("$") for k in condition):
        # Sibling-modifier ops: keys that aren't standalone operators but
        # tune another operator in the same condition dict. ``$options``
        # tunes ``$regex``; ``$maxDistance`` / ``$minDistance`` tune the
        # legacy mongod 2d form of ``$near`` / ``$nearSphere`` where the
        # bound lives at the sibling level (mongod's
        # ``{geo: {$near: [x, y], $maxDistance: r}}``). Skip them when
        # iterating; pull them in below when their parent op runs.
        has_near = "$near" in condition or "$nearSphere" in condition
        # mongod validates the $regex / $options pair at parse time (BadValue /
        # 51108), before matching. $options is a sibling modifier of $regex.
        if "$options" in condition:
            if "$regex" not in condition:
                raise QueryError("$options needs a $regex")
            _validate_regex_options(condition["$options"])
        for op, arg in condition.items():
            if op in _SIBLING_MODIFIERS:
                continue
            if op == "$regex":
                _validate_regex_pattern(arg)
                if not _op_regex(values, arg, condition.get("$options", "")):
                    return False
            elif op in ("$near", "$nearSphere") and has_near:
                # Pass the WHOLE condition dict so the parser can read
                # sibling ``$maxDistance`` / ``$minDistance``.
                if not _op_geo_near(
                    values,
                    arg,
                    default_spherical=(op == "$nearSphere"),
                    siblings=condition,
                ):
                    return False
            elif not _op_matches(values, op, arg, collation, field=field):
                return False
        return True
    return _eq_with_array(values, condition, collation)


def _validate_not_arg(arg: Any) -> None:
    """mongod's ``$not`` argument must be a regex or a non-empty document of
    operators (BadValue): a scalar / array / bool is "$not argument must be a
    regex or an object", an empty document is "$not argument must be a non-empty
    object". Without this a bare ``{$not: 5}`` silently degrades to "not equal
    to 5". Wording probed 8.2.11, 2026-09-01.

    A ``Code`` is NOT an object here -- it subclasses ``str``, so it takes the
    scalar branch, which is what mongod does with it too."""
    if isinstance(arg, Regex):
        return
    if not isinstance(arg, Mapping):
        raise QueryError("$not argument must be a regex or an object", code=2)
    if not arg:
        raise QueryError("$not argument must be a non-empty object", code=2)
    # Every key inside `$not` must be a known operator. Without this a document
    # of ordinary field names degraded to an equality match and the `$not`
    # NEGATED it, so `{n: {$not: {a: 1}}}` returned the document -- a wrong
    # answer, where mongod refuses the query.
    for key in arg:
        if key not in _KNOWN_FIELD_OPERATORS:
            raise QueryError(f"unknown operator: {key}", code=2)


def _validate_in_arg(op: str, arg: Any) -> None:
    """mongod parse-time validation for ``$in`` / ``$nin`` (BadValue, code 2): the
    argument must be an array, and no element may be a document with a
    ``$``-prefixed key (``{$regex: …}`` / ``{$x: 1}`` — "cannot nest $ under $in").
    A BSON ``Regex`` literal is fine. Without this a non-array leaks a Python
    ``TypeError`` and a nested-``$`` doc silently matches nothing."""
    if not isinstance(arg, list):
        raise QueryError(f"{op} needs an array")
    for element in arg:
        if isinstance(element, Mapping) and any(str(k).startswith("$") for k in element):
            raise QueryError("cannot nest $ under $in")


def _in_candidate_matches(
    values: list[Any], candidate: Any, collation: Collation | None = None
) -> bool:
    """A single `$in` / `$nin` candidate: a regex candidate matches string values
    by pattern (mongod semantics — the old bare-equality path silently matched
    nothing); everything else is array-aware, collation-aware equality."""
    if isinstance(candidate, Regex):
        return _op_regex(values, candidate.pattern, candidate.flags)
    return _eq_with_array(values, candidate, collation)


def _eq_with_array(values: list[Any], expected: Any, collation: Collation | None = None) -> bool:
    for v in values:
        if v is MISSING:
            if expected is None:
                return True
            continue
        if _eq_numeric_aware(v, expected, collation):
            return True
        if isinstance(v, list) and any(_eq_numeric_aware(e, expected, collation) for e in v):
            return True
    return False


def _eq_numeric_aware(a: Any, b: Any, collation: Collation | None = None) -> bool:
    """Equality that bridges int / float / Decimal128 but keeps bool distinct.

    MongoDB treats int / float / Decimal128 as a single numeric type for
    equality but ranks bool as a separate type — ``{x: 1}`` does not match
    ``x: true``. Python's ``True == 1`` and ``Decimal128.__eq__`` are both
    wrong for our purposes, so we have to handle both directions here.

    String-vs-string equality routes through ``collation.equal`` when a
    collation is in effect — ``strength 2`` makes ``"ping" == "PING"``
    so case-insensitive find / update / delete tests pass.
    """
    # Embedded documents compare for equality field-ORDER-sensitively
    # and exactly (no subset), recursively — a mongod gotcha that
    # Python's order-insensitive ``dict ==`` would get wrong. Arrays
    # compare positionally with numeric-aware leaves.
    a_map = isinstance(a, Mapping)
    b_map = isinstance(b, Mapping)
    if a_map or b_map:
        if not (a_map and b_map) or len(a) != len(b):
            return False
        return all(
            ka == kb and _eq_numeric_aware(va, vb, collation)
            for (ka, va), (kb, vb) in zip(a.items(), b.items(), strict=True)
        )
    a_list = isinstance(a, list)
    b_list = isinstance(b, list)
    if a_list or b_list:
        if not (a_list and b_list) or len(a) != len(b):
            return False
        return all(_eq_numeric_aware(x, y, collation) for x, y in zip(a, b, strict=True))

    a_bool = isinstance(a, bool)
    b_bool = isinstance(b, bool)
    if a_bool != b_bool:
        return False
    if collation is not None and isinstance(a, str) and isinstance(b, str):
        return _coll_equal(a, b, collation)
    if a == b:
        return True
    # tz-aware / tz-naive datetimes of the same instant compare equal: a BSON date
    # decodes tz-naive UTC while a SQL ``timestamptz`` literal arrives tz-aware UTC,
    # and ``naive == aware`` is always False in Python. Treat naive as UTC (the same
    # convention pymongo's BSON encoder uses), so ``WHERE ts = '...+00:00'`` matches
    # the stored value — mirroring the range operators' ``_coerce_datetime``.
    if isinstance(a, _dt.datetime) and isinstance(b, _dt.datetime):
        a2, b2 = _coerce_datetime(a, b)
        return a2 == b2
    # NaN equals NaN for query purposes. IEEE says otherwise and Python follows
    # IEEE, but mongod matches `{x: NaN}` against a stored NaN — probed against
    # 6.0.16. Without this a document inserted with `_id: NaN` can never be found
    # by its own `_id` again, which is the worst form of the bug: the write is
    # accepted and the row is then unreachable by key. Checked BEFORE the
    # `_coerce_numeric` early return below, which bails for two same-type floats
    # and so never reached this.
    if _is_nan(a) and _is_nan(b):
        return True
    a2, b2 = _coerce_numeric(a, b)
    if a2 is a:
        return False
    return a2 == b2


def _is_nan(v: Any) -> bool:
    """True for a float or Decimal128 NaN, False for anything else."""
    if isinstance(v, float):
        return math.isnan(v)
    to_dec = getattr(v, "to_decimal", None)
    if to_dec is not None:
        try:
            return to_dec().is_nan()
        except (ValueError, ArithmeticError):
            return False
    return False


#: The range operators, which mongod refuses to take a regex for -- a regex is
#: only meaningful under equality, where it MATCHES rather than compares.
_RANGE_OPS = frozenset({"$gt", "$gte", "$lt", "$lte"})


def _is_nan(v: Any) -> bool:
    """A NaN bound, `float` or `Decimal128`.

    mongod's comparison order treats NaN as EQUAL to NaN -- which is why
    `find({x: NaN})` matches -- so an INCLUSIVE range bound matches it and a
    strict one does not (probed 8.2.11, 2026-09-06)::

        {x: {$gte: NaN}}  ->  the NaN document
        {x: {$lte: NaN}}  ->  the NaN document
        {x: {$gt:  NaN}}  ->  nothing
        {x: {$lt:  NaN}}  ->  nothing

    Both servers answered nothing for all four, because IEEE says every NaN
    comparison is false. Same shape as the NaN gate on partial indexes that
    `CLAUDE.md` records: NaN sits inside the numeric bracket, and the two
    orders mongod maintains disagree about it.
    """
    if isinstance(v, float):
        return math.isnan(v)
    if isinstance(v, Decimal128):
        try:
            return v.to_decimal().is_nan()
        except (InvalidOperation, ValueError):
            return False
    return False


def _op_matches(
    values: list[Any],
    op: str,
    arg: Any,
    collation: Collation | None = None,
    *,
    field: str = "",
) -> bool:
    # mongod rejects this at parse time rather than matching nothing (probed
    # 8.2.11, 2026-09-01). Answering an empty result set instead hid a
    # malformed query -- the caller cannot tell "no documents match" from
    # "that predicate is meaningless".
    if isinstance(arg, Regex) and op in _RANGE_OPS:
        raise QueryError(
            f"Can't have RegEx as arg to non-equality predicate over field '{field}'.",
            code=2,
            code_name="BadValue",
        )
    if isinstance(arg, Regex) and op == "$ne":
        raise QueryError("Can't have regex as arg to $ne.", code=2, code_name="BadValue")
    if op == "$eq":
        return _eq_with_array(values, arg, collation)
    if op == "$ne":
        return not _eq_with_array(values, arg, collation)
    if op == "$gt":
        return _cmp(values, arg, lambda a, b: a > b, collation)
    if op == "$gte":
        # `$gte: null` (like `$lte: null`) matches null and missing — the same
        # set as `$eq: null` — because null only orders equal to null. `$gt`/`$lt`
        # null match nothing (a value is never strictly above/below null).
        if arg is None:
            return _eq_with_array(values, None, collation)
        if _is_nan(arg):
            return _eq_with_array(values, arg, collation)
        return _cmp(values, arg, lambda a, b: a >= b, collation)
    if op == "$lt":
        return _cmp(values, arg, lambda a, b: a < b, collation)
    if op == "$lte":
        if arg is None:
            return _eq_with_array(values, None, collation)
        if _is_nan(arg):
            return _eq_with_array(values, arg, collation)
        return _cmp(values, arg, lambda a, b: a <= b, collation)
    if op == "$in":
        _validate_in_arg("$in", arg)
        return any(_in_candidate_matches(values, candidate, collation) for candidate in arg)
    if op == "$nin":
        _validate_in_arg("$nin", arg)
        return not any(_in_candidate_matches(values, candidate, collation) for candidate in arg)
    if op == "$exists":
        # mongod uses its own truthiness for the argument (only false / 0 / null
        # are falsy — an empty string / array / document is truthy), NOT Python's.
        present = any(v is not MISSING for v in values)
        return present == _truthy(arg)
    if op == "$not":
        _validate_not_arg(arg)
        return not _field_matches(values, arg, collation)
    if op == "$type":
        return _op_type(values, arg, field)
    if op == "$size":
        return _op_size(values, arg)
    if op == "$all":
        return _op_all(values, arg)
    if op == "$mod":
        return _op_mod(values, arg)
    if op == "$elemMatch":
        if not isinstance(arg, Mapping):
            raise QueryError("$elemMatch needs an Object")
        return _op_elem_match(values, arg)
    if op == "$bitsAllSet":
        return _op_bitwise(values, arg, lambda v, m: (v & m) == m, op, field)
    if op == "$bitsAnySet":
        return _op_bitwise(values, arg, lambda v, m: (v & m) != 0, op, field)
    if op == "$bitsAllClear":
        return _op_bitwise(values, arg, lambda v, m: (v & m) == 0, op, field)
    if op == "$bitsAnyClear":
        return _op_bitwise(values, arg, lambda v, m: (v & m) != m, op, field)
    if op == "$geoWithin":
        return _op_geo_within(values, arg)
    if op == "$geoIntersects":
        return _op_geo_intersects(values, arg)
    if op == "$near":
        return _op_geo_near(values, arg, default_spherical=False)
    if op == "$nearSphere":
        return _op_geo_near(values, arg, default_spherical=True)
    raise QueryError(f"unknown operator: {op}")


def _op_geo_within(values: list[Any], arg: Any) -> bool:
    from secantus.geo import GeoError, geo_within, parse_doc_geometry, parse_query_geometry

    if not isinstance(arg, Mapping):
        raise QueryError("$geoWithin requires an object")
    try:
        query_geom, _spherical = parse_query_geometry(arg)
    except GeoError as exc:
        raise QueryError(str(exc)) from exc
    for v in values:
        if v is MISSING:
            continue
        doc_geom = parse_doc_geometry(v)
        if doc_geom is not None and geo_within(doc_geom, query_geom):
            return True
    return False


def _op_geo_intersects(values: list[Any], arg: Any) -> bool:
    from secantus.geo import GeoError, geo_intersects, parse_doc_geometry, parse_query_geometry

    if not isinstance(arg, Mapping):
        raise QueryError("$geoIntersects requires an object")
    if "$geometry" not in arg:
        # mongod restricts $geoIntersects to GeoJSON via $geometry; mirror.
        raise QueryError("$geoIntersects requires $geometry (GeoJSON)")
    try:
        query_geom, _spherical = parse_query_geometry(arg)
    except GeoError as exc:
        raise QueryError(str(exc)) from exc
    for v in values:
        if v is MISSING:
            continue
        doc_geom = parse_doc_geometry(v)
        if doc_geom is not None and geo_intersects(doc_geom, query_geom):
            return True
    return False


def _op_geo_near(
    values: list[Any],
    arg: Any,
    *,
    default_spherical: bool,
    siblings: Mapping[str, Any] | None = None,
) -> bool:
    """Match (without ranking) for ``$near`` / ``$nearSphere``.

    ``$near`` is a hybrid operator: real ``mongod`` *also* sorts results
    by distance and requires a geo index. In :mod:`matches`-only contexts
    we treat it purely as a containment test (within ``$maxDistance``,
    outside ``$minDistance``). Sort-by-distance is the caller's
    responsibility — handled in :mod:`commands` for top-level ``find``.

    ``siblings`` carries the parent condition dict so the legacy
    mongod 2d form ``{geo: {$near: [x, y], $maxDistance: r,
    $minDistance: r2}}`` works — the bound is at sibling level rather
    than nested inside ``$near``.
    """
    from shapely.geometry import Point

    from secantus.geo import EARTH_RADIUS_METERS, distance, parse_doc_geometry

    center, max_distance, min_distance, spherical, legacy_form = _parse_near_spec(
        arg, default_spherical=default_spherical, siblings=siblings
    )
    # Legacy + spherical: spec gives bound in radians on the unit
    # sphere; ``distance(spherical=True)`` returns meters. Convert.
    if legacy_form and spherical:
        if max_distance is not None:
            max_distance = max_distance * EARTH_RADIUS_METERS
        if min_distance is not None:
            min_distance = min_distance * EARTH_RADIUS_METERS
    center_geom = Point(center[0], center[1])
    for v in values:
        if v is MISSING:
            continue
        doc_geom = parse_doc_geometry(v)
        if doc_geom is None:
            continue
        d = distance(doc_geom, center_geom, spherical=spherical)
        if d is None:
            continue
        if max_distance is not None and d > max_distance:
            continue
        if min_distance is not None and d < min_distance:
            continue
        return True
    return False


def _parse_near_spec(
    arg: Any,
    *,
    default_spherical: bool,
    siblings: Mapping[str, Any] | None = None,
) -> tuple[tuple[float, float], float | None, float | None, bool, bool]:
    """Normalize ``$near`` arg into
    ``(center, max_distance, min_distance, spherical, legacy_form)``.

    Three shapes are accepted:

    * GeoJSON: ``{$geometry: {type:"Point", coordinates:[lng,lat]},
      $maxDistance: meters, $minDistance: meters}`` — always spherical.
    * Legacy 2-element list: ``[x, y]`` — distances in input units
      (planar for ``$near``, radians-on-unit-sphere for ``$nearSphere``).
    * Legacy 3-element list: ``[x, y, max]`` — same as above plus a
      max-distance bound.

    ``legacy_form`` is True when the arg used the list shape (with or
    without sibling ``$maxDistance`` / ``$minDistance``). Callers use
    it to know what unit ``max_distance`` is in: legacy+spherical →
    radians on unit sphere; legacy+planar → input units; GeoJSON →
    meters (always).

    When ``siblings`` is provided (the parent condition dict) and the
    arg is the list form, sibling ``$maxDistance`` / ``$minDistance``
    keys are pulled in — that's the mongod 2d legacy shape
    ``{geo: {$near: [x, y], $maxDistance: 0.1}}`` the Java driver's
    ``Filters.near(field, x, y, max, min)`` builds.
    """
    if isinstance(arg, Mapping):
        if "$geometry" in arg:
            geom = arg["$geometry"]
            if (
                not isinstance(geom, Mapping)
                or geom.get("type") != "Point"
                or not isinstance(geom.get("coordinates"), list)
                or len(geom["coordinates"]) != 2
            ):
                raise QueryError("$near $geometry must be a GeoJSON Point")
            cx, cy = geom["coordinates"]
            return (
                (float(cx), float(cy)),
                _opt_number(arg.get("$maxDistance", MISSING), "$maxDistance"),
                _opt_number(arg.get("$minDistance", MISSING), "$minDistance"),
                True,
                False,  # GeoJSON form — distances are already in meters
            )
        raise QueryError("$near requires $geometry or a coordinate pair")
    if isinstance(arg, list) and len(arg) in (2, 3):
        try:
            cx, cy = float(arg[0]), float(arg[1])
        except (TypeError, ValueError) as exc:
            raise QueryError("$near coordinate pair must be numeric") from exc
        max_d = float(arg[2]) if len(arg) == 3 else None
        min_d: float | None = None
        # Sibling-modifier overlay: legacy mongod 2d shape lifts
        # ``$maxDistance`` / ``$minDistance`` to the parent condition
        # level. List-form arg can't carry them itself, so pick them up
        # from the siblings dict.
        if siblings is not None:
            if "$maxDistance" in siblings:
                sibling_max = _opt_number(siblings["$maxDistance"], "$maxDistance", code=16895)
                if sibling_max is not None:
                    max_d = sibling_max
            if "$minDistance" in siblings:
                min_d = _opt_number(siblings["$minDistance"], "$minDistance", code=16893)
        # Legacy spec keeps the bound in its raw unit (input units for
        # ``$near``, radians-on-unit-sphere for ``$nearSphere``).
        # Conversion to the comparison currency (meters for spherical,
        # planar for 2d-index picker) happens at the consumer — see
        # ``_op_geo_near`` for the matcher side and
        # ``Storage._geo_query_cells`` for the index picker.
        return ((cx, cy), max_d, min_d, default_spherical, True)
    raise QueryError("$near must be a GeoJSON-shaped doc or a coordinate pair")


def _opt_number(value: Any, label: str, code: int = 2) -> float | None:
    """A ``$near`` distance bound, or ``None`` when the key is absent.

    ``MISSING`` means the key was not supplied. An explicit ``null`` is NOT the
    same thing -- probed on mongod 8.3.4, ``{$near: {..., $minDistance: null}}``
    is rejected with ``$minDistance must be a number`` (code 2), where we used to
    treat it as absent and run the query unbounded. Callers therefore pass
    ``arg.get(key, MISSING)`` rather than ``arg.get(key)``.

    Negative bounds are rejected too: mongod answers ``$minDistance must be
    non-negative``. Strings and bools were already refused.

    ``code`` differs by form, probed on mongod 8.3.4: the nested GeoJSON form
    (``{$near: {$geometry: ..., $minDistance: x}}``) uses the generic BadValue
    (2), while the legacy sibling form (``{geo: {$near: [x, y],
    $maxDistance: x}}``) has dedicated codes -- 16895 for ``$maxDistance`` and
    16893 for ``$minDistance``.
    """
    if value is MISSING:
        return None
    if value is None or isinstance(value, bool) or not isinstance(value, (int, float)):
        raise QueryError(f"{label} must be a number", code=code)
    if value < 0:
        raise QueryError(f"{label} must be non-negative", code=code)
    return float(value)


#: The widest integer mongod will build a `$bits*` mask from.
_INT64_MAX = 2**63 - 1
_INT64_MIN = -(2**63)


def bindata_to_bits(data: bytes) -> int:
    """A BinData value as a bit set: LITTLE-endian bytes, least-significant bit
    first within each byte, so ``b"\x00\x01"`` is bit 8.

    Probed against mongod 8.2.11 (2026-09-01) by asking for one bit position at
    a time. BinData is accepted both as a ``$bits*`` MASK and as a stored field
    VALUE; neither was supported before.
    """
    return int.from_bytes(data, "little")


def bit_source(value: Any) -> int | None:
    """The integer a stored value contributes to a ``$bits*`` test, or ``None``
    when the value is not bit-eligible at all.

    mongod accepts int32 / int64, a **whole finite double in int64 range**, a
    **whole Decimal128**, and **BinData** — and rejects strings, bools,
    fractional doubles, NaN / Infinity, and out-of-range doubles. This used to
    accept `int` and nothing else, so a double, a Decimal128 and a BinData value
    were silently skipped: `{v: {$bitsAllSet: 5}}` missed a document holding
    `5.0`.

    Negative values are two's complement with infinite sign extension, which is
    exactly what Python's arbitrary-precision `&` already does (`-1 & 5 == 5`),
    so they need no special handling here — only admission.
    """
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        if not math.isfinite(value) or not value.is_integer():
            return None
        return int(value) if _INT64_MIN <= value <= _INT64_MAX else None
    if isinstance(value, Decimal128):
        try:
            dec = value.to_decimal()
        except (ValueError, ArithmeticError):
            return None
        if not dec.is_finite() or dec != dec.to_integral_value():
            return None
        as_int = int(dec)
        return as_int if _INT64_MIN <= as_int <= _INT64_MAX else None
    if isinstance(value, (bytes, Binary, bytearray)):
        return bindata_to_bits(bytes(value))
    return None


def _resolve_bitmask(arg: Any, op: str, field: str = "") -> int:
    """Build the bitmask for a `$bits*` query, mongod-style. The argument is an
    array of bit positions, a non-negative integer / whole-double / whole-decimal
    mask, or a BinData mask. A fractional double, a bool, or a negative value is
    rejected — a bad *position* with code 2, a bad non-array *mask* with code 9
    (a bool mask with code 2). Codes verified vs mongod 7.0.12 and 8.2.11."""
    if isinstance(arg, (bytes, Binary, bytearray)):
        return bindata_to_bits(bytes(arg))
    if isinstance(arg, bool):
        raise QueryError(
            f"{field} takes an Array, a number, or a BinData but received: "
            f"{op}: {_mongo_bson_repr(arg)}",
            code=2,
        )
    if isinstance(arg, (int, float, Decimal128)):
        try:
            mask = coerce_int64_argument(arg, op)
        except Int64CoercionError as exc:
            raise QueryError(exc.message, code=exc.code, code_name="FailedToParse") from None
        if mask < 0:
            raise QueryError(
                f"Expected a non-negative number in: {op}: {_mongo_bson_repr(arg)}",
                code=9,
                code_name="FailedToParse",
            )
        return mask
    if isinstance(arg, list):
        mask = 0
        for i, bit in enumerate(arg):
            if isinstance(bit, bool):
                raise QueryError(
                    f"Failed to parse bit position. Expected a number in: {i}: "
                    f"{'true' if bit else 'false'}",
                    code=2,
                )
            if isinstance(bit, float):
                if not bit.is_integer():
                    raise QueryError(
                        f"Failed to parse bit position. Expected an integer: {i}: {bit!r}",
                        code=2,
                    )
                bit = int(bit)
            if not isinstance(bit, int):
                raise QueryError(
                    f"Failed to parse bit position. Expected a number in: {i}: {bit!r}",
                    code=2,
                )
            if bit < 0:
                raise QueryError(
                    f"Failed to parse bit position. Expected a non-negative number in: {i}: {bit}",
                    code=2,
                )
            mask |= 1 << bit
        return mask
    # The field name used to be the literal string "n" -- whatever field the
    # original probe happened to use -- so every one of these messages named the
    # wrong field. mongod also echoes the offending value.
    raise QueryError(
        f"{field} takes an Array, a number, or a BinData but received: "
        f"{op}: {_mongo_bson_repr(arg)}",
        code=2,
    )


def _op_bitwise(
    values: list[Any], arg: Any, predicate: Callable[[int, int], bool], op: str, field: str = ""
) -> bool:
    mask = _resolve_bitmask(arg, op, field)
    for v in values:
        source = bit_source(v)
        if source is not None and predicate(source, mask):
            return True
        # An ARRAY field is matched element-wise, one level deep -- the same
        # multikey rule the comparison operators follow. Without it a document
        # holding `[1, 4]` was skipped entirely.
        if isinstance(v, list):
            for elem in v:
                source = bit_source(elem)
                if source is not None and predicate(source, mask):
                    return True
    return False


def _cmp(
    values: list[Any],
    target: Any,
    op: Callable[[Any, Any], bool],
    collation: Collation | None = None,
) -> bool:
    for v in values:
        if v is MISSING:
            continue
        if _try_cmp(v, target, op, collation):
            return True
        if isinstance(v, list):
            for elem in v:
                if _try_cmp(elem, target, op, collation):
                    return True
    return False


def _same_type_bracket(value: Any, bound: Any) -> bool:
    """Whether a range operator may compare these two values at all.

    mongod's range operators (``$gt`` / ``$gte`` / ``$lt`` / ``$lte``) are
    **type-bracketed**: ``{v: {$gt: 3}}`` matches numbers greater than 3 and
    NOTHING else -- not the string "z", not a MaxKey, not a date. The one
    exception is a ``MinKey`` / ``MaxKey`` **bound**, which mongod compares
    against every type -- and only the bound: a document whose VALUE is a
    ``MaxKey`` is still bracketed out of ``{v: {$gt: 3}}``. Getting that
    direction wrong keeps the original bug, because the MaxKey is on the
    document side.

    Only three brackets were enforced before this (bool, document, array), and
    the gap was not a message-level nicety: a collection holding a ``MaxKey``
    returned that document for *every* ``$gt`` query, and ``pymongo``'s
    ``MaxKey.__gt__`` returning True unconditionally is what made it look like a
    match. Measured against 8.2.11 (2026-09-01), 96 of 112 (bound, operator,
    collation) shapes disagreed with mongod.

    Numbers share one bracket (int / long / double / Decimal128 compare across
    themselves), which is why this asks :func:`_bson_type_rank` rather than
    comparing Python types -- and JavaScript has its own rank there, which is
    what stops ``{v: {$gt: "ab"}}`` matching a ``Code`` document.
    """
    from secantus.ordering import _bson_type_rank

    if isinstance(bound, (MinKey, MaxKey)):
        return True
    return _bson_type_rank(value) == _bson_type_rank(bound)


def _try_cmp(
    a: Any, b: Any, op: Callable[[Any, Any], bool], collation: Collation | None = None
) -> bool:
    if not _same_type_bracket(a, b):
        return False
    if (
        collation is not None
        # `bson.Code` subclasses `str`; without this guard a JavaScript value
        # compared as a collated string against a string bound, and matched.
        and isinstance(a, str)
        and isinstance(b, str)
        and not isinstance(a, Code)
        and not isinstance(b, Code)
    ):
        # Route through collation-aware compare. ``op`` operates on the
        # numeric result of ``compare_keys`` interpreted as the sign of
        # ``a - b``.
        c = _coll_compare(a, b, collation)
        try:
            return bool(op(c, 0))
        except TypeError:
            return False
    # Two embedded documents order field-by-field under range operators
    # (mongod: first differing key compares as a string, else recurse into the
    # value, else the shorter document sorts first). Python's ``operator.gt``
    # raises ``TypeError`` on two dicts, which the ``except`` below swallows to
    # a silent no-match — so ``{a: {$gt: {x: 1}}}`` wrongly matched nothing.
    # Route through the BSON-order comparator (same one ``$sort`` uses).
    if isinstance(a, Mapping) and isinstance(b, Mapping):
        from secantus.ordering import _bson_lt

        c = -1 if _bson_lt(a, b) else (1 if _bson_lt(b, a) else 0)
        return bool(op(c, 0))
    # Two arrays order element-by-element under range operators, but each
    # element pair compares by *full* BSON order (type rank first) — mongod
    # ranks a string element above a number element, so `[1, "x"] > [1, 2]`.
    # Python's native `[..] < [..]` raises TypeError on such a cross-type
    # element pair (swallowed to a no-match), so route through `_bson_lt`.
    if isinstance(a, list) and isinstance(b, list):
        from secantus.ordering import _bson_lt

        c = -1 if _bson_lt(a, b) else (1 if _bson_lt(b, a) else 0)
        return bool(op(c, 0))
    a, b = _coerce_numeric(a, b)
    a, b = _coerce_datetime(a, b)
    try:
        return bool(op(a, b))
    except TypeError:
        return False


def _coerce_datetime(a: Any, b: Any) -> tuple[Any, Any]:
    """Bridge tz-aware / tz-naive datetimes so a range comparison never raises a
    ``TypeError`` (which would be swallowed and silently drop the row). BSON dates
    decode tz-naive UTC; a SQL ``timestamptz`` literal arrives tz-aware UTC — the
    same instant — so a naive datetime is treated as UTC before comparing."""
    if (
        isinstance(a, _dt.datetime)
        and isinstance(b, _dt.datetime)
        and (a.tzinfo is None) != (b.tzinfo is None)
    ):
        if a.tzinfo is None:
            a = a.replace(tzinfo=_dt.timezone.utc)
        if b.tzinfo is None:
            b = b.replace(tzinfo=_dt.timezone.utc)
    return a, b


def _coerce_numeric(a: Any, b: Any) -> tuple[Any, Any]:
    """Bridge numeric BSON types so $gt/$lt etc. compare across int/float/Decimal128.

    Bool is intentionally excluded — MongoDB ranks bools as a separate type
    from numbers and they should not silently bridge.
    """
    if (
        not isinstance(a, bool)
        and not isinstance(b, bool)
        and isinstance(a, (int, float, Decimal128))
        and isinstance(b, (int, float, Decimal128))
        and (isinstance(a, Decimal128) or isinstance(b, Decimal128))
    ):

        def _to_dec(v: Any) -> Decimal | None:
            if isinstance(v, Decimal128):
                try:
                    return v.to_decimal()
                except (InvalidOperation, ValueError):
                    return None
            if isinstance(v, float):
                try:
                    return Decimal(repr(v))
                except (InvalidOperation, ValueError):
                    return None
            return Decimal(int(v))

        ad, bd = _to_dec(a), _to_dec(b)
        # `is_finite()` excludes NaN *and* the infinities, and only NaN needs
        # excluding. `Decimal` orders +/-Infinity correctly, so bailing out on
        # them left `float > Decimal128` to raise `TypeError`, which
        # `_try_cmp` swallows into a silent no-match: `{x: {$gt: Decimal128("5")}}`
        # skipped a document holding `Infinity`, and `{x: {$lt: Infinity}}`
        # skipped one holding `Decimal128("5")` (measured against mongod
        # 8.2.11, 2026-09-06 -- six range shapes plus `$all`).
        if ad is not None and bd is not None and not ad.is_nan() and not bd.is_nan():
            return ad, bd
    return a, b


_MONGO_FLAG_MAP = {
    "i": re.IGNORECASE,
    "m": re.MULTILINE,
    "s": re.DOTALL,
    "x": re.VERBOSE,
}


def _re_flags(flags_input: Any) -> int:
    if isinstance(flags_input, int):
        return flags_input
    if isinstance(flags_input, bytes):
        flags_input = flags_input.decode()
    flags = 0
    for c in flags_input or "":
        flags |= _MONGO_FLAG_MAP.get(c, 0)
    return flags


# Hard cap on user-supplied regex pattern length. Python's `re` has no
# match timeout, so a catastrophic-backtracking pattern like `(a+)+$`
# applied to a long string hangs the worker thread permanently. Capping
# the pattern length is a coarse but effective mitigation — real-world
# legitimate patterns are well under this — and it sidesteps the worst
# ReDoS family without pulling in the `regex` package.
_MAX_REGEX_PATTERN_LEN = 1000


@lru_cache(maxsize=1024)
def _compile_regex(pattern: str | bytes, flags: int) -> re.Pattern:
    if hasattr(pattern, "__len__") and len(pattern) > _MAX_REGEX_PATTERN_LEN:
        raise QueryError(
            f"regex pattern of {len(pattern)} chars exceeds the {_MAX_REGEX_PATTERN_LEN}-char cap"
        )
    return re.compile(pattern, flags)


_VALID_REGEX_FLAGS = frozenset("imsxu")


def _validate_regex_options(options: Any) -> None:
    """mongod's ``$options`` validation: it must be a string (else BadValue) of
    only the flags ``imsxu`` (an unknown letter is Location51108)."""
    if not isinstance(options, str):
        raise QueryError("$options has to be a string")
    for c in options:
        if c not in _VALID_REGEX_FLAGS:
            raise QueryError(
                # The leading space is mongod's own -- it streams an empty slot before
                # the sentence. Verbatim, not a typo here.
                f" invalid flag in regex options: {c}",
                code=51108,
                code_name="Location51108",
            )


def _validate_regex_pattern(pattern: Any) -> None:
    """mongod's ``$regex`` value must be a string or a regex literal (else
    BadValue): a number / null / other type is rejected."""
    if not isinstance(pattern, (str, bytes, Regex)):
        raise QueryError("$regex has to be a string")


def _op_regex(values: list[Any], pattern: Any, options: Any) -> bool:
    flags = _re_flags(options)
    if isinstance(pattern, Regex):
        regex_pattern = pattern.pattern
        flags |= _re_flags(pattern.flags)
    elif isinstance(pattern, str) and not isinstance(pattern, Code):
        regex_pattern = pattern
    else:
        # mongod takes a string or a BSON regex and nothing else. A `Binary`
        # pattern used to compile as a BYTES regex and then raise
        # `TypeError: cannot use a bytes pattern on a string-like object` from
        # `search()` -- an unhandled exception, i.e. `1 internal server error`.
        # Every other type silently matched nothing.
        raise QueryError("$regex has to be a string", code=2)
    try:
        compiled = _compile_regex(regex_pattern, flags)
    except re.error as exc:
        raise QueryError(f"invalid regex: {exc}") from exc
    except TypeError as exc:
        # Unhashable pattern (e.g. a non-str/bytes input) — fall back to
        # an uncached compile so the caller still gets a real re.error.
        try:
            compiled = re.compile(regex_pattern, flags)
        except re.error as e:
            raise QueryError(f"invalid regex: {e}") from e
        except Exception as e:
            raise QueryError(f"invalid regex: {exc}") from e
    # mongod matches a regex against a STRING by pattern, and against a stored
    # REGEX by equality -- exact pattern, and options compared as a SET, so
    # `/ab/im` equals `/ab/mi` but `/ab/i` does not equal `/ab/mi` (probed
    # 8.2.11). `bson.Regex.__eq__` is that comparison: `flags` is already the
    # normalised int, so the set semantics come for free. Without the equality
    # arm `find({v: /ab/i})` missed every document whose `v` IS that regex.
    query_regex = Regex(regex_pattern, flags) if isinstance(regex_pattern, str) else None
    for v in values:
        if v is MISSING:
            continue
        if _regex_matches_value(v, compiled, query_regex):
            return True
        if isinstance(v, list) and any(
            _regex_matches_value(elem, compiled, query_regex) for elem in v
        ):
            return True
    return False


def _regex_matches_value(v: Any, compiled: re.Pattern, query_regex: Regex | None) -> bool:
    """One document value against a compiled query regex.

    ``_is_bson_string`` rather than ``isinstance(v, str)``: ``bson.Code``
    subclasses ``str``, so a JavaScript value used to be pattern-matched as
    though it were text. mongod does not apply a regex to code (probed 8.2.11)
    -- `find({v: /ab/})` over `{v: Code("ab")}` returns nothing there and used
    to return the document here.
    """
    if isinstance(v, Regex):
        return query_regex is not None and v == query_regex
    return _is_bson_string(v) and compiled.search(v) is not None


def _is_int32(v: Any) -> bool:
    """True for BSON int32 — plain Python int, not bool, not Int64.

    pymongo's BSON decoder preserves the int32 / int64 distinction:
    int32 values come back as plain ``int``, int64 values as
    :class:`bson.Int64` (a subclass of ``int``). The distinction is
    by *type*, not by *value range*: ``bson.Int64(5)`` matches
    ``$type: "long"`` even though its value fits in int32.
    """
    return isinstance(v, int) and not isinstance(v, (bool, Int64))


def _is_int64(v: Any) -> bool:
    """True for BSON int64. ``Int64`` is a subclass of ``int`` so
    ordinary numeric comparisons still work; this predicate keys on
    the BSON type tag preserved by the decoder."""
    return isinstance(v, Int64)


def _is_bson_number(v: Any) -> bool:
    """``$type: "number"`` — any BSON numeric (int32, int64, double,
    decimal). Excludes ``bool``, which mongod ranks as its own type."""
    return isinstance(v, (float, Decimal128)) or _is_int32(v) or _is_int64(v)


def _is_bson_string(v: Any) -> bool:
    """A BSON string. `bson.Code` subclasses `str` and is NOT one."""
    return isinstance(v, str) and not isinstance(v, Code)


def _is_javascript(v: Any) -> bool:
    """BSON type 13. A `Code` carrying a scope is type 15 instead -- probed
    8.2.11: `$type: "javascript"` matches only the scope-less one."""
    return isinstance(v, Code) and v.scope is None


def _is_javascript_with_scope(v: Any) -> bool:
    """BSON type 15."""
    return isinstance(v, Code) and v.scope is not None


_TYPE_PREDS: dict[Any, Callable[[Any], bool]] = {
    1: lambda v: isinstance(v, float),
    "double": lambda v: isinstance(v, float),
    # `bson.Code` subclasses `str`, so a bare `isinstance(v, str)` matched a
    # JavaScript value as a string -- and `javascript` matched nothing at all,
    # because the table had no entry for it.
    2: _is_bson_string,
    "string": _is_bson_string,
    3: lambda v: isinstance(v, dict),
    "object": lambda v: isinstance(v, dict),
    4: lambda v: isinstance(v, list),
    "array": lambda v: isinstance(v, list),
    5: lambda v: isinstance(v, (bytes, Binary)),
    "binData": lambda v: isinstance(v, (bytes, Binary)),
    7: lambda v: isinstance(v, ObjectId),
    "objectId": lambda v: isinstance(v, ObjectId),
    8: lambda v: isinstance(v, bool),
    "bool": lambda v: isinstance(v, bool),
    9: lambda v: isinstance(v, _dt.datetime),
    "date": lambda v: isinstance(v, _dt.datetime),
    10: lambda v: v is None,
    "null": lambda v: v is None,
    11: lambda v: isinstance(v, Regex),
    "regex": lambda v: isinstance(v, Regex),
    16: _is_int32,
    "int": _is_int32,
    18: _is_int64,
    "long": _is_int64,
    19: lambda v: isinstance(v, Decimal128),
    "decimal": lambda v: isinstance(v, Decimal128),
    # These five aliases VALIDATED but had no predicate, so `_matches_type`
    # fell through to `False` and the query silently matched nothing -- four
    # whole BSON types were unreachable by `$type` (probed 8.2.11, 2026-09-01).
    13: _is_javascript,
    "javascript": _is_javascript,
    15: _is_javascript_with_scope,
    "javascriptWithScope": _is_javascript_with_scope,
    17: lambda v: isinstance(v, Timestamp),
    "timestamp": lambda v: isinstance(v, Timestamp),
    -1: lambda v: isinstance(v, MinKey),
    "minKey": lambda v: isinstance(v, MinKey),
    127: lambda v: isinstance(v, MaxKey),
    "maxKey": lambda v: isinstance(v, MaxKey),
    "number": _is_bson_number,
}


def bson_type_name(v: Any) -> str:
    """mongod's BSON type-alias string for a decoded value.

    Used to fill ``consideredType`` in document-validation failure
    details (the C# CRUD-spec prose test pins ``"int"`` for a Python
    ``int``). Order matters: ``bool`` and ``Int64`` are ``int``
    subclasses and must be checked first.
    """
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, Int64):
        return "long"
    if isinstance(v, int):
        return "int"
    if isinstance(v, float):
        return "double"
    if isinstance(v, Decimal128):
        return "decimal"
    if isinstance(v, str):
        return "string"
    if isinstance(v, ObjectId):
        return "objectId"
    if isinstance(v, _dt.datetime):
        return "date"
    if v is None:
        return "null"
    if isinstance(v, Regex):
        return "regex"
    if isinstance(v, (bytes, Binary)):
        return "binData"
    if isinstance(v, list):
        return "array"
    if isinstance(v, dict):
        return "object"
    return "object"


def _matches_type(value: Any, type_spec: Any) -> bool:
    pred = _TYPE_PREDS.get(type_spec)
    return bool(pred(value)) if pred else False


# Every BSON type alias mongod's $type accepts (incl. the deprecated ones and the
# "number" meta-alias) and the valid numeric codes: 1..19, minKey (-1), maxKey (127).
_VALID_TYPE_ALIASES = frozenset(
    {
        "double",
        "string",
        "object",
        "array",
        "binData",
        "undefined",
        "objectId",
        "bool",
        "date",
        "null",
        "regex",
        "dbPointer",
        "javascript",
        "symbol",
        "javascriptWithScope",
        "int",
        "timestamp",
        "long",
        "decimal",
        "minKey",
        "maxKey",
        "number",
    }
)
_VALID_TYPE_CODES = frozenset({-1, 127} | set(range(1, 20)))


def _validate_type_arg(t: Any) -> None:
    """mongod's $type argument validation: a known string alias, or a numeric code
    in {-1, 1..19, 127} (a whole double is accepted). A bool / other type is
    TypeMismatch (14); an unknown alias or an out-of-range / fractional code is
    BadValue (2), with a special hint for code 0."""
    # `bson.Code` subclasses `str` AND is unhashable, so it reached the
    # `t not in _VALID_TYPE_ALIASES` set test below and raised
    # `TypeError: unhashable type` -- which the dispatcher turned into
    # `1 internal server error` for an ordinary bad argument. mongod answers
    # TypeMismatch here, like any other non-string non-number.
    if isinstance(t, (bool, Code)):
        raise QueryError(
            "type must be represented as a number or a string", code=14, code_name="TypeMismatch"
        )
    if isinstance(t, str):
        if t not in _VALID_TYPE_ALIASES:
            raise QueryError(f"Unknown type name alias: {t}")
        return
    if isinstance(t, (int, float, Decimal128)):
        if isinstance(t, Decimal128):
            dec = t.to_decimal()
            if not dec.is_finite() or dec != dec.to_integral_value():
                # Rendered through the DOUBLE form, like every other numeric
                # code: `Decimal128("NaN")` prints `nan`, not `NaN`.
                raise QueryError(f"Invalid numerical type code: {_fmt_g(float(dec))}")
            code = int(dec)
            if code == 0:
                # `%g` keeps the sign of negative zero, and mongod's message
                # does too -- `Decimal128("-0")` reports `-0`, not `0`.
                raise QueryError(
                    f"Invalid numerical type code: {_fmt_g(float(dec))}"
                    ". Instead use {$exists:false}."
                )
        elif isinstance(t, float):
            if not t.is_integer():
                raise QueryError(f"Invalid numerical type code: {_fmt_g(t)}")
            code = int(t)
        else:
            code = t
        if code not in _VALID_TYPE_CODES:
            suffix = ". Instead use {$exists:false}." if code == 0 else ""
            raise QueryError(f"Invalid numerical type code: {code}{suffix}")
        return
    raise QueryError(
        "type must be represented as a number or a string", code=14, code_name="TypeMismatch"
    )


def _op_type(values: list[Any], type_spec: Any, field: str = "") -> bool:
    types = type_spec if isinstance(type_spec, list) else [type_spec]
    if isinstance(type_spec, list) and not type_spec:
        # An empty alias list is a parse error, not "matches nothing" -- probed
        # 8.2.11 (2026-09-01), where this answered an empty result set.
        raise QueryError(f"{field} must match at least one type", code=9, code_name="FailedToParse")
    for t in types:
        _validate_type_arg(t)
    for v in values:
        if v is MISSING:
            continue
        if any(_matches_type(v, t) for t in types):
            return True
        if isinstance(v, list):
            for elem in v:
                if any(_matches_type(elem, t) for t in types):
                    return True
    return False


def _op_size(values: list[Any], size: Any) -> bool:
    """``$size`` takes a non-negative whole number.

    mongod validates it through the same numeric ladder as ``$pop`` and the
    ``$bits*`` mask -- NaN, out-of-range, fractional and non-integral
    Decimal128 each get their own sentence -- under a
    ``"Failed to parse $size. "`` prefix. A whole ``Decimal128`` is accepted.
    Probed 8.2.11 (2026-09-01).
    """
    prefix = "Failed to parse $size. "
    try:
        n = coerce_int64_argument(size, "$size")
    except TypeError:
        raise QueryError(f"{prefix}Expected a number in: $size: {_mongo_bson_repr(size)}") from None
    except Int64CoercionError as exc:
        raise QueryError(f"{prefix}{exc.message}") from None
    if n < 0:
        raise QueryError(
            f"{prefix}Expected a non-negative number in: $size: {_mongo_bson_repr(size)}"
        )
    return any(isinstance(v, list) and len(v) == n for v in values)


def _op_all(values: list[Any], required: Any) -> bool:
    if not isinstance(required, list):
        raise QueryError("$all needs an array")

    # mongod: if any element is a $-expression document, EVERY element must be a
    # {$elemMatch: …} clause (the all-$elemMatch form). Mixing $elemMatch with a
    # scalar, or using any other $-operator doc, is "no $ expressions in $all".
    def _is_elemmatch_clause(e: Any) -> bool:
        return isinstance(e, Mapping) and list(e.keys()) == ["$elemMatch"]

    def _has_dollar_key(e: Any) -> bool:
        return isinstance(e, Mapping) and any(str(k).startswith("$") for k in e)

    if any(_has_dollar_key(e) for e in required) and not all(
        _is_elemmatch_clause(e) for e in required
    ):
        raise QueryError("no $ expressions in $all")

    def _elem_matches_required(elem: Any, r: Any) -> bool:
        # Regex elements in the ``$all`` array match as patterns, not by
        # equality. Mongo-node-driver's ``Find should correctly find
        # documents by regExp`` test passes an array of regexes; if we
        # used bare ``==`` (which compares Regex objects by identity)
        # the find would silently return no docs.
        if isinstance(r, Regex):
            pattern = r.pattern
            flags = _re_flags(r.flags)
            if isinstance(elem, str):
                return _compile_regex(pattern, flags).search(elem) is not None
            return False
        if isinstance(r, re.Pattern):
            if isinstance(elem, str):
                return r.search(elem) is not None
            return False
        # The same numeric-bridging equality `$eq` uses, not a bare `==`:
        # mongod treats int / long / double / Decimal128 as one type for
        # equality (and keeps bool distinct), so `{x: {$all: [5]}}` matches a
        # document holding `Decimal128("5")`. `==` does not, and silently
        # dropped that document (measured 8.2.11, 2026-09-06). `$eq` on the same
        # value already agreed with mongod, which is what made the gap look like
        # a `$all` quirk rather than a missing bridge.
        return _eq_numeric_aware(elem, r)

    def _required_satisfied(v: Any, r: Any) -> bool:
        # A `{$elemMatch: {...}}` clause requires *some* element of the array to
        # match the sub-query (mongod's `$all` + `$elemMatch` form) — a scalar
        # field can never satisfy it. Every other clause matches if the field is
        # an array containing a matching element, OR a scalar that itself
        # equals / pattern-matches the clause (mongod treats a scalar field like
        # a one-element array for `$all`, verified against mongod 7.0.12).
        if isinstance(r, Mapping) and list(r.keys()) == ["$elemMatch"]:
            return isinstance(v, list) and _op_elem_match([v], r["$elemMatch"])
        if isinstance(v, list):
            return any(_elem_matches_required(elem, r) for elem in v)
        return _elem_matches_required(v, r)

    # `$all: []` matches nothing (mongod), not everything — guard the vacuous
    # `all(...)` that would otherwise be True for every value.
    if not required:
        return False
    return any(all(_required_satisfied(v, r) for r in required) for v in values)


def _mod_int(v: Any) -> int | None:
    """The integer a value contributes to ``$mod`` (truncated toward zero), or
    None if it isn't a `$mod`-eligible number. mongod (probed 7.0.12) truncates
    int / long / double / Decimal128 toward zero and **excludes bool** (bool is
    not a number for `$mod`, unlike Python where ``True % 2`` evaluates)."""
    if isinstance(v, bool):
        return None
    if isinstance(v, int):
        return v
    if isinstance(v, float):
        return None if (math.isnan(v) or math.isinf(v)) else int(v)  # trunc toward zero
    if isinstance(v, Decimal128):
        try:
            d = v.to_decimal()
        except (InvalidOperation, ValueError):
            return None
        return int(d) if d.is_finite() else None
    return None


def _op_mod(values: list[Any], mod_spec: Any) -> bool:
    # mongod separates "not an array at all" from "an array that is too short";
    # this answered the too-short message for both.
    if not isinstance(mod_spec, (list, tuple)):
        raise QueryError("malformed mod, needs to be an array")
    if len(mod_spec) < 2:
        raise QueryError("malformed mod, not enough elements")
    div = _mod_int(mod_spec[0])
    if div is None:
        raise QueryError("malformed mod, divisor not a number")
    if div == 0:
        raise QueryError("divisor cannot be 0")
    remainder = mod_spec[1]
    for v in values:
        if v is MISSING:
            continue
        if _try_mod(v, div, remainder):
            return True
        if isinstance(v, list):
            for elem in v:
                if _try_mod(elem, div, remainder):
                    return True
    return False


def _try_mod(v: Any, div: int, remainder: Any) -> bool:
    """``v`` (truncated to an int) mod ``div`` equals ``remainder``. Uses C-style
    truncated modulo (``math.fmod``, sign of the dividend) to match mongod —
    Python's ``%`` takes the sign of the divisor, so ``-5 % 2`` diverges."""
    iv = _mod_int(v)
    if iv is None:
        return False
    return int(math.fmod(iv, div)) == remainder


def _op_elem_match(values: list[Any], condition: Any) -> bool:
    if not isinstance(condition, Mapping):
        return False
    is_scalar_form = bool(condition) and all(k.startswith("$") for k in condition)
    for v in values:
        if not isinstance(v, list):
            continue
        for elem in v:
            if is_scalar_form:
                if _field_matches([elem], condition):
                    return True
            elif isinstance(elem, Mapping) and matches(elem, condition):
                return True
    return False
