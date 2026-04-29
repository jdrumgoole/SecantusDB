from __future__ import annotations

import datetime as _dt
import re
from collections.abc import Callable, Mapping
from typing import Any

from bson import Binary, Decimal128, ObjectId, Regex


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
    pass


def matches(
    doc: Mapping[str, Any],
    query: Mapping[str, Any],
    *,
    vars: dict[str, Any] | None = None,
) -> bool:
    if not query:
        return True
    return all(_match_clause(doc, k, v, vars) for k, v in query.items())


def _match_clause(
    doc: Mapping[str, Any],
    key: str,
    condition: Any,
    vars: dict[str, Any] | None,
) -> bool:
    if key == "$and":
        return all(matches(doc, c, vars=vars) for c in condition)
    if key == "$or":
        return any(matches(doc, c, vars=vars) for c in condition)
    if key == "$nor":
        return not any(matches(doc, c, vars=vars) for c in condition)
    if key == "$expr":
        from secantus.expressions import evaluate

        return _truthy(evaluate(condition, doc, vars))
    if key == "$comment":
        return True
    if key == "$jsonSchema":
        return _validate_json_schema(doc, condition)
    if key.startswith("$"):
        raise QueryError(f"unsupported top-level operator: {key}")
    return _field_matches(_resolve_path(doc, key), condition)


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
        if "minimum" in schema and value < schema["minimum"]:
            return False
        if "maximum" in schema and value > schema["maximum"]:
            return False
        if "exclusiveMinimum" in schema and value <= schema["exclusiveMinimum"]:
            return False
        if "exclusiveMaximum" in schema and value >= schema["exclusiveMaximum"]:
            return False
    if isinstance(value, str):
        if "minLength" in schema and len(value) < schema["minLength"]:
            return False
        if "maxLength" in schema and len(value) > schema["maxLength"]:
            return False
        if "pattern" in schema and not re.search(schema["pattern"], value):
            return False
    if isinstance(value, list):
        if "minItems" in schema and len(value) < schema["minItems"]:
            return False
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            return False
        if "items" in schema:
            for item in value:
                if not _validate_json_schema(item, schema["items"]):
                    return False
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
    return True


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


def _field_matches(values: list[Any], condition: Any) -> bool:
    if isinstance(condition, Regex):
        return _op_regex(values, condition.pattern, condition.flags)
    if isinstance(condition, Mapping) and condition and all(k.startswith("$") for k in condition):
        for op, arg in condition.items():
            if op == "$options":
                continue
            if op == "$regex":
                if not _op_regex(values, arg, condition.get("$options", "")):
                    return False
            elif not _op_matches(values, op, arg):
                return False
        return True
    return _eq_with_array(values, condition)


def _eq_with_array(values: list[Any], expected: Any) -> bool:
    for v in values:
        if v is MISSING:
            if expected is None:
                return True
            continue
        if v == expected:
            return True
        if isinstance(v, list) and any(e == expected for e in v):
            return True
    return False


def _op_matches(values: list[Any], op: str, arg: Any) -> bool:
    if op == "$eq":
        return _eq_with_array(values, arg)
    if op == "$ne":
        return not _eq_with_array(values, arg)
    if op == "$gt":
        return _cmp(values, arg, lambda a, b: a > b)
    if op == "$gte":
        return _cmp(values, arg, lambda a, b: a >= b)
    if op == "$lt":
        return _cmp(values, arg, lambda a, b: a < b)
    if op == "$lte":
        return _cmp(values, arg, lambda a, b: a <= b)
    if op == "$in":
        return any(_eq_with_array(values, candidate) for candidate in arg)
    if op == "$nin":
        return not any(_eq_with_array(values, candidate) for candidate in arg)
    if op == "$exists":
        present = any(v is not MISSING for v in values)
        return present == bool(arg)
    if op == "$not":
        return not _field_matches(values, arg)
    if op == "$type":
        return _op_type(values, arg)
    if op == "$size":
        return _op_size(values, arg)
    if op == "$all":
        return _op_all(values, arg)
    if op == "$mod":
        return _op_mod(values, arg)
    if op == "$elemMatch":
        return _op_elem_match(values, arg)
    if op == "$bitsAllSet":
        return _op_bitwise(values, arg, lambda v, m: (v & m) == m)
    if op == "$bitsAnySet":
        return _op_bitwise(values, arg, lambda v, m: (v & m) != 0)
    if op == "$bitsAllClear":
        return _op_bitwise(values, arg, lambda v, m: (v & m) == 0)
    if op == "$bitsAnyClear":
        return _op_bitwise(values, arg, lambda v, m: (v & m) != m)
    raise QueryError(f"unsupported query operator: {op}")


def _resolve_bitmask(arg: Any) -> int:
    if isinstance(arg, bool):
        raise QueryError("bitwise mask cannot be a boolean")
    if isinstance(arg, int):
        return arg
    if isinstance(arg, list):
        mask = 0
        for bit in arg:
            if not isinstance(bit, int) or isinstance(bit, bool):
                raise QueryError("bitwise mask positions must be integers")
            mask |= 1 << bit
        return mask
    raise QueryError("bitwise operator requires an int or list of bit positions")


def _op_bitwise(values: list[Any], arg: Any, predicate: Callable[[int, int], bool]) -> bool:
    mask = _resolve_bitmask(arg)
    for v in values:
        if isinstance(v, int) and not isinstance(v, bool) and predicate(v, mask):
            return True
    return False


def _cmp(values: list[Any], target: Any, op: Callable[[Any, Any], bool]) -> bool:
    for v in values:
        if v is MISSING:
            continue
        if _try_cmp(v, target, op):
            return True
        if isinstance(v, list):
            for elem in v:
                if _try_cmp(elem, target, op):
                    return True
    return False


def _try_cmp(a: Any, b: Any, op: Callable[[Any, Any], bool]) -> bool:
    try:
        return bool(op(a, b))
    except TypeError:
        return False


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


def _op_regex(values: list[Any], pattern: Any, options: Any) -> bool:
    flags = _re_flags(options)
    if isinstance(pattern, Regex):
        regex_pattern = pattern.pattern
        flags |= _re_flags(pattern.flags)
    else:
        regex_pattern = pattern
    try:
        compiled = re.compile(regex_pattern, flags)
    except re.error as exc:
        raise QueryError(f"invalid regex: {exc}") from exc
    for v in values:
        if v is MISSING:
            continue
        if isinstance(v, str) and compiled.search(v):
            return True
        if isinstance(v, list):
            for elem in v:
                if isinstance(elem, str) and compiled.search(elem):
                    return True
    return False


def _is_bson_int(v: Any, *, ranged: tuple[int, int] | None = None) -> bool:
    if not isinstance(v, int) or isinstance(v, bool):
        return False
    if ranged is not None:
        lo, hi = ranged
        return lo <= v <= hi
    return True


_INT32_RANGE = (-(2**31), 2**31 - 1)


_TYPE_PREDS: dict[Any, Callable[[Any], bool]] = {
    1: lambda v: isinstance(v, float),
    "double": lambda v: isinstance(v, float),
    2: lambda v: isinstance(v, str),
    "string": lambda v: isinstance(v, str),
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
    16: lambda v: _is_bson_int(v, ranged=_INT32_RANGE),
    "int": lambda v: _is_bson_int(v, ranged=_INT32_RANGE),
    18: lambda v: _is_bson_int(v) and not _is_bson_int(v, ranged=_INT32_RANGE),
    "long": lambda v: _is_bson_int(v) and not _is_bson_int(v, ranged=_INT32_RANGE),
    19: lambda v: isinstance(v, Decimal128),
    "decimal": lambda v: isinstance(v, Decimal128),
    "number": lambda v: isinstance(v, (float, Decimal128)) or _is_bson_int(v),
}


def _matches_type(value: Any, type_spec: Any) -> bool:
    pred = _TYPE_PREDS.get(type_spec)
    return bool(pred(value)) if pred else False


def _op_type(values: list[Any], type_spec: Any) -> bool:
    types = type_spec if isinstance(type_spec, list) else [type_spec]
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
    if not isinstance(size, int):
        raise QueryError("$size requires an integer")
    return any(isinstance(v, list) and len(v) == size for v in values)


def _op_all(values: list[Any], required: Any) -> bool:
    if not isinstance(required, list):
        raise QueryError("$all requires an array")
    for v in values:
        if isinstance(v, list) and all(any(elem == r for elem in v) for r in required):
            return True
    return False


def _op_mod(values: list[Any], mod_spec: Any) -> bool:
    if not (isinstance(mod_spec, (list, tuple)) and len(mod_spec) == 2):
        raise QueryError("$mod requires [divisor, remainder]")
    divisor, remainder = mod_spec
    for v in values:
        if v is MISSING:
            continue
        if _try_mod(v, divisor, remainder):
            return True
        if isinstance(v, list):
            for elem in v:
                if _try_mod(elem, divisor, remainder):
                    return True
    return False


def _try_mod(v: Any, divisor: Any, remainder: Any) -> bool:
    try:
        return bool(v % divisor == remainder)
    except (TypeError, ZeroDivisionError):
        return False


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
