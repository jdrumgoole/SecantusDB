from __future__ import annotations

import datetime as _dt
import re
from collections.abc import Callable, Mapping
from decimal import Decimal, InvalidOperation
from functools import lru_cache
from typing import Any

from bson import Binary, Decimal128, Int64, ObjectId, Regex

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
    pass


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
    if key == "$and":
        return all(matches(doc, c, vars=vars, collation=collation) for c in condition)
    if key == "$or":
        return any(matches(doc, c, vars=vars, collation=collation) for c in condition)
    if key == "$nor":
        return not any(matches(doc, c, vars=vars, collation=collation) for c in condition)
    if key == "$expr":
        from secantus.expressions import evaluate

        return _truthy(evaluate(condition, doc, vars))
    if key == "$comment":
        return True
    if key == "$jsonSchema":
        return _validate_json_schema(doc, condition)
    if key.startswith("$"):
        raise QueryError(f"unsupported top-level operator: {key}")
    return _field_matches(_resolve_path(doc, key), condition, collation)


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
        # Route through _compile_regex so the pattern-length cap fires.
        if "pattern" in schema and not _compile_regex(schema["pattern"], 0).search(value):
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


def _field_matches(values: list[Any], condition: Any, collation: Collation | None = None) -> bool:
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
        _SIBLING_MODIFIERS = frozenset(("$options", "$maxDistance", "$minDistance"))
        has_near = "$near" in condition or "$nearSphere" in condition
        for op, arg in condition.items():
            if op in _SIBLING_MODIFIERS:
                continue
            if op == "$regex":
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
            elif not _op_matches(values, op, arg, collation):
                return False
        return True
    return _eq_with_array(values, condition, collation)


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
    a2, b2 = _coerce_numeric(a, b)
    if a2 is a:
        return False
    return a2 == b2


def _op_matches(values: list[Any], op: str, arg: Any, collation: Collation | None = None) -> bool:
    if op == "$eq":
        return _eq_with_array(values, arg, collation)
    if op == "$ne":
        return not _eq_with_array(values, arg, collation)
    if op == "$gt":
        return _cmp(values, arg, lambda a, b: a > b, collation)
    if op == "$gte":
        return _cmp(values, arg, lambda a, b: a >= b, collation)
    if op == "$lt":
        return _cmp(values, arg, lambda a, b: a < b, collation)
    if op == "$lte":
        return _cmp(values, arg, lambda a, b: a <= b, collation)
    if op == "$in":
        return any(_eq_with_array(values, candidate, collation) for candidate in arg)
    if op == "$nin":
        return not any(_eq_with_array(values, candidate, collation) for candidate in arg)
    if op == "$exists":
        present = any(v is not MISSING for v in values)
        return present == bool(arg)
    if op == "$not":
        return not _field_matches(values, arg, collation)
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
    if op == "$geoWithin":
        return _op_geo_within(values, arg)
    if op == "$geoIntersects":
        return _op_geo_intersects(values, arg)
    if op == "$near":
        return _op_geo_near(values, arg, default_spherical=False)
    if op == "$nearSphere":
        return _op_geo_near(values, arg, default_spherical=True)
    raise QueryError(f"unsupported query operator: {op}")


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
                _opt_number(arg.get("$maxDistance"), "$maxDistance"),
                _opt_number(arg.get("$minDistance"), "$minDistance"),
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
                sibling_max = _opt_number(siblings["$maxDistance"], "$maxDistance")
                if sibling_max is not None:
                    max_d = sibling_max
            if "$minDistance" in siblings:
                min_d = _opt_number(siblings["$minDistance"], "$minDistance")
        # Legacy spec keeps the bound in its raw unit (input units for
        # ``$near``, radians-on-unit-sphere for ``$nearSphere``).
        # Conversion to the comparison currency (meters for spherical,
        # planar for 2d-index picker) happens at the consumer — see
        # ``_op_geo_near`` for the matcher side and
        # ``Storage._geo_query_cells`` for the index picker.
        return ((cx, cy), max_d, min_d, default_spherical, True)
    raise QueryError("$near must be a GeoJSON-shaped doc or a coordinate pair")


def _opt_number(value: Any, label: str) -> float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise QueryError(f"{label} must be a number")
    return float(value)


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


def _try_cmp(
    a: Any, b: Any, op: Callable[[Any, Any], bool], collation: Collation | None = None
) -> bool:
    if collation is not None and isinstance(a, str) and isinstance(b, str):
        # Route through collation-aware compare. ``op`` operates on the
        # numeric result of ``compare_keys`` interpreted as the sign of
        # ``a - b``.
        c = _coll_compare(a, b, collation)
        try:
            return bool(op(c, 0))
        except TypeError:
            return False
    a, b = _coerce_numeric(a, b)
    try:
        return bool(op(a, b))
    except TypeError:
        return False


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
        if ad is not None and bd is not None and ad.is_finite() and bd.is_finite():
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


def _op_regex(values: list[Any], pattern: Any, options: Any) -> bool:
    flags = _re_flags(options)
    if isinstance(pattern, Regex):
        regex_pattern = pattern.pattern
        flags |= _re_flags(pattern.flags)
    else:
        regex_pattern = pattern
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
    16: _is_int32,
    "int": _is_int32,
    18: _is_int64,
    "long": _is_int64,
    19: lambda v: isinstance(v, Decimal128),
    "decimal": lambda v: isinstance(v, Decimal128),
    "number": _is_bson_number,
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
        return elem == r

    for v in values:
        if isinstance(v, list) and all(
            any(_elem_matches_required(elem, r) for elem in v) for r in required
        ):
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
