"""``errInfo.details`` for a document that failed its collection's validator.

mongod explains a 121 ``Document failed validation`` with a structure that
names every rule the document broke -- per keyword for ``$jsonSchema``, per
clause for a query-operator validator. Drivers' ``errorResponse`` tests read
it, and it is the only thing that tells a user WHY a write was refused. This
server sent ``{operatorName: "$jsonSchema"}`` and nothing else for every
``$jsonSchema`` failure, and only the first failing field clause otherwise
(measured 8.2.11, 2026-10-07).

The shapes are transcribed from mongod 8.2's ``doc_validation_error.cpp``, and
the ORDER of ``$jsonSchema`` rules from ``json_schema_parser.cpp``'s ``_parse``,
which builds one AND of: the scalar keywords (``pattern``, ``maxLength``,
``minLength``, ``multipleOf``, ``maximum``, ``minimum``), the array keywords
(``minItems``, ``maxItems``, ``uniqueItems``, ``items``, ``additionalItems``),
the object keywords (``properties``, ``patternProperties`` /
``additionalProperties``, ``required``, ``minProperties``, ``maxProperties``,
``dependencies``), the logical ones (``allOf``, ``anyOf``, ``oneOf``, ``not``,
``enum``), and -- below the top level -- the type restriction LAST. Every
scalar / array / object keyword is a restriction on its own type: it cannot
fail for a value of another type.

Which document FAILS is still decided by the matcher; this module only explains
a failure already established. ``tools/probes/validation_error_details.py`` and
``tools/probes/remaining_shapes.py`` compare the output with mongod exactly.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Any

from bson import Decimal128, Int64, Regex

from secantus.ordering import bson_equal
from secantus.query import bson_type_name, matches

# --- query-operator validators ----------------------------------------------


@dataclass
class _Node:
    kind: str  # "and" | "or" | "nor" | "leaf" | "not" | "expr" | "schema"
    children: list[_Node] = field(default_factory=list)
    path: str = ""
    op: str = ""
    spec: dict[str, Any] = field(default_factory=dict)
    filter: dict[str, Any] = field(default_factory=dict)
    value: Any = None


def explain(validator: Mapping[str, Any], doc: Mapping[str, Any]) -> dict[str, Any]:
    """The explanation for ``doc`` failing ``validator``."""
    node = _parse_filter(validator)
    explained = _explain_node(node, doc, False)
    return explained if explained is not None else {"operatorName": "validator"}


def _parse_filter(flt: Mapping[str, Any]) -> _Node:
    """A filter as mongod's match-expression tree: each top-level clause -- and
    each operator of a multi-operator field -- is its own child of an implicit
    ``$and``, which a single child replaces."""
    clauses: list[_Node] = []
    for key, value in flt.items():
        if key in ("$and", "$or", "$nor"):
            children = [_parse_filter(c) for c in value if isinstance(c, Mapping)]
            clauses.append(_Node({"$and": "and", "$or": "or", "$nor": "nor"}[key], children))
        elif key == "$expr":
            clauses.append(_Node("expr", value=value))
        elif key == "$jsonSchema":
            if isinstance(value, Mapping):
                clauses.append(_Node("schema", value=dict(value)))
        elif key == "$comment":
            continue
        else:
            clauses.extend(_parse_field(key, value))
    return clauses[0] if len(clauses) == 1 else _Node("and", clauses)


def _is_operator_doc(value: Any) -> bool:
    return (
        isinstance(value, Mapping)
        and bool(value)
        and all(isinstance(k, str) and k.startswith("$") for k in value)
    )


def _leaf(fld: str, op: str, spec: dict[str, Any]) -> _Node:
    return _Node("leaf", path=fld, op=op, spec=spec, filter=spec)


def _parse_field(fld: str, value: Any) -> list[_Node]:
    if not _is_operator_doc(value):
        op = "$regex" if isinstance(value, Regex) or _is_re(value) else "$eq"
        return [_leaf(fld, op, {fld: value})]
    out: list[_Node] = []
    for op, arg in value.items():
        if op == "$options":
            # `$options` belongs to `$regex`, and is echoed with it.
            continue
        if op == "$regex":
            inner: dict[str, Any] = {"$regex": arg}
            if "$options" in value:
                inner["$options"] = value["$options"]
            out.append(_leaf(fld, "$regex", {fld: inner}))
        elif op == "$not":
            nodes = _parse_field(fld, arg)
            inner_node = nodes[0] if len(nodes) == 1 else _Node("and", nodes)
            out.append(_Node("not", [inner_node], filter={fld: {"$not": arg}}))
        else:
            out.append(_leaf(fld, op, {fld: {op: arg}}))
    return out


def _is_re(value: Any) -> bool:
    import re

    return isinstance(value, re.Pattern)


def _filter_matches(doc: Mapping[str, Any], flt: Mapping[str, Any]) -> bool:
    try:
        return matches(doc, flt)
    except Exception:  # noqa: BLE001 -- an unmatchable clause explains as failing
        return False


def _node_matches(node: _Node, doc: Mapping[str, Any]) -> bool:
    if node.kind == "and":
        return all(_node_matches(n, doc) for n in node.children)
    if node.kind == "or":
        return any(_node_matches(n, doc) for n in node.children)
    if node.kind == "nor":
        return not any(_node_matches(n, doc) for n in node.children)
    if node.kind in ("leaf", "not"):
        return _filter_matches(doc, node.filter)
    if node.kind == "expr":
        return _filter_matches(doc, {"$expr": node.value})
    return _filter_matches(doc, {"$jsonSchema": node.value})


def _explain_node(node: _Node, doc: Mapping[str, Any], inverted: bool) -> dict[str, Any] | None:
    """Why ``node`` fails for ``doc`` -- or, when ``inverted``, why it MATCHES
    (the child of a ``$nor`` / ``$not``). ``None`` when there is nothing to
    explain."""
    if _node_matches(node, doc) != inverted:
        return None
    if node.kind in ("and", "or"):
        key = "clausesSatisfied" if inverted else "clausesNotSatisfied"
        items = _indexed(_explain_node(c, doc, inverted) for c in node.children)
        return {"operatorName": "$" + node.kind, key: items}
    if node.kind == "nor":
        key = "clausesNotSatisfied" if inverted else "clausesSatisfied"
        items = _indexed(_explain_node(c, doc, not inverted) for c in node.children)
        return {"operatorName": "$nor", key: items}
    if node.kind == "not":
        d: dict[str, Any] = {"operatorName": "$not"}
        child = _explain_node(node.children[0], doc, not inverted)
        if child is not None:
            d["details"] = child
        return d
    if node.kind == "leaf":
        return _leaf_details(doc, node.path, node.op, node.spec, inverted)
    if node.kind == "expr":
        from secantus.expressions import evaluate

        try:
            result = evaluate(node.value, doc)
        except Exception:  # noqa: BLE001 -- mongod reports the failed result as null
            result = None
        return {
            "operatorName": "$expr",
            "specifiedAs": {"$expr": node.value},
            "reason": "expression did match" if inverted else "expression did not match",
            "expressionResult": result,
        }
    return _explain_json_schema(node.value, doc)


def _indexed(items: Any) -> list[dict[str, Any]]:
    return [{"index": i, "details": d} for i, d in enumerate(items) if d is not None]


#: ``(normal, inverted)`` reasons, from ``doc_validation_error.cpp``.
_LEAF_REASONS: dict[str, tuple[str, str]] = {
    "$in": ("no matching value found in array", "matching value found in array"),
    "$nin": ("matching value found in array", "no matching value found in array"),
    "$ne": ("comparison succeeded", "comparison failed"),
    "$regex": ("regular expression did not match", "regular expression did match"),
    "$size": ("array length was not equal to given size", "array length was equal to given size"),
    "$all": (
        "array did not contain all specified values",
        "array did contain all specified values",
    ),
    "$mod": (
        "$mod did not evaluate to expected remainder",
        "$mod did evaluate to expected remainder",
    ),
    "$elemMatch": (
        "array did not satisfy the child predicate",
        "array did satisfy the child predicate",
    ),
    "$type": ("type did not match", "type did match"),
}
_BITS = ("bitwise operator failed to match", "bitwise operator matched successfully")
for _op in ("$bitsAllSet", "$bitsAllClear", "$bitsAnySet", "$bitsAnyClear"):
    _LEAF_REASONS[_op] = _BITS


def _leaf_details(
    doc: Mapping[str, Any], fld: str, op: str, spec: dict[str, Any], inverted: bool
) -> dict[str, Any]:
    d: dict[str, Any] = {"operatorName": op, "specifiedAs": spec}
    if op == "$exists":
        arg = spec.get(fld, {}).get("$exists") if isinstance(spec.get(fld), Mapping) else None
        wants = not (arg is False or arg is None or (type(arg) is int and arg == 0))
        # `$exists: true` fails on a missing path; `false` on a present one.
        d["reason"] = "path does not exist" if wants != inverted else "path does exist"
        return d
    # The array-shaped operators consider the array itself; the rest the
    # elements of an array they traverse.
    whole = op in ("$size", "$all", "$elemMatch")
    values = _path_values(doc, fld, not whole)
    if values is None:
        d["reason"] = "field was missing"
        return d
    normal, inv = _LEAF_REASONS.get(op, ("comparison failed", "comparison succeeded"))
    d["reason"] = inv if inverted else normal
    _append_considered(d, values)
    if op == "$type":
        _append_considered_types(d, values)
    return d


def _append_considered(d: dict[str, Any], values: list[Any]) -> None:
    if len(values) == 1:
        d["consideredValue"] = values[0]
    else:
        d["consideredValues"] = list(values)


def _append_considered_types(d: dict[str, Any], values: list[Any]) -> None:
    types = sorted({bson_type_name(v) for v in values})
    if len(types) == 1:
        d["consideredType"] = types[0]
    elif types:
        d["consideredTypes"] = types


def _path_values(doc: Mapping[str, Any], path: str, traverse: bool) -> list[Any] | None:
    """The values a leaf considers at ``path``, or ``None`` when the path does
    not exist. With ``traverse``, an array at the end of the path contributes
    its ELEMENTS (mongod's ``kTraverseOmitArray``); arrays along the way are
    walked into their document elements either way."""
    out: list[Any] = []
    found = False

    def walk(v: Any, parts: list[str]) -> None:
        nonlocal found
        if not parts:
            found = True
            if traverse and isinstance(v, list):
                out.extend(v)
            else:
                out.append(v)
            return
        if isinstance(v, Mapping):
            if parts[0] in v:
                walk(v[parts[0]], parts[1:])
        elif isinstance(v, list):
            for e in v:
                if isinstance(e, Mapping):
                    walk(e, parts)

    walk(doc, path.split("."))
    return out if found else None


# --- $jsonSchema ------------------------------------------------------------


def _explain_json_schema(schema: Mapping[str, Any], doc: Mapping[str, Any]) -> dict[str, Any]:
    # A top-level type that excludes "object" matches nothing at all; mongod
    # reports the whole schema rather than a rule.
    for kw in ("bsonType", "type"):
        if kw in schema:
            if not _type_matches(kw, schema[kw], doc):
                return {
                    "operatorName": "$jsonSchema",
                    "specifiedAs": {"$jsonSchema": dict(schema)},
                    "reason": "expression always evaluates to false",
                }
            break
    return {
        "operatorName": "$jsonSchema",
        "schemaRulesNotSatisfied": _explain_value(schema, doc, True),
    }


def _is_number(v: Any) -> bool:
    return isinstance(v, (int, float, Int64, Decimal128)) and not isinstance(v, bool)


def _as_float(v: Any) -> float | None:
    if isinstance(v, Decimal128):
        try:
            return float(v.to_decimal())
        except (InvalidOperation, ValueError):
            return None
    if _is_number(v):
        return float(v)
    return None


def _as_count(v: Any) -> int | None:
    if isinstance(v, bool):
        return None
    if isinstance(v, (int, Int64)):
        return int(v)
    if isinstance(v, float) and v.is_integer():
        return int(v)
    return None


def _type_matches(kw: str, spec: Any, value: Any) -> bool:
    """Whether ``value`` has the type a ``type`` / ``bsonType`` keyword names."""
    if isinstance(spec, str):
        names = [spec]
    elif isinstance(spec, list):
        names = [n for n in spec if isinstance(n, str)]
    else:
        return True
    for n in names:
        alias = "bool" if kw == "type" and n == "boolean" else n
        if _filter_matches({"v": value}, {"v": {"$type": alias}}) and not (
            isinstance(value, list) and alias != "array"
        ):
            return True
    return False


def _schema_matches(schema: Mapping[str, Any], value: Any, top: bool) -> bool:
    return not _explain_value(schema, value, top)


def _sub_schemas(schema: Mapping[str, Any], kw: str) -> list[Mapping[str, Any]]:
    v = schema.get(kw)
    return [s for s in v if isinstance(s, Mapping)] if isinstance(v, list) else []


def _indexed_schemas(
    schemas: list[Mapping[str, Any]],
    value: Any,
    top: bool,
    keep: Callable[[list[Any]], bool],
) -> list[dict[str, Any]]:
    out = []
    for i, s in enumerate(schemas):
        d = _explain_value(s, value, top)
        if keep(d):
            out.append({"index": i, "details": d})
    return out


def _explain_value(schema: Mapping[str, Any], value: Any, top: bool) -> list[Any]:
    """Every rule of ``schema`` that ``value`` breaks, in mongod's order. ``top``
    is the root schema, which has no path and so no type restriction of its
    own."""
    out: list[Any] = []
    push = out.append

    # Scalar keywords -- strings and numbers only.
    if isinstance(value, str):
        p = schema.get("pattern")
        if isinstance(p, str) and not _filter_matches({"v": value}, {"v": {"$regex": p}}):
            push(
                {
                    "operatorName": "pattern",
                    "specifiedAs": {"pattern": p},
                    "reason": "regular expression did not match",
                    "consideredValue": value,
                }
            )
        length = len(value)
        for kw, too_long in (("maxLength", True), ("minLength", False)):
            n = _as_count(schema.get(kw))
            if n is not None and ((too_long and length > n) or (not too_long and length < n)):
                push(
                    {
                        "operatorName": kw,
                        "specifiedAs": {kw: schema[kw]},
                        "reason": "specified string length was not satisfied",
                        "consideredValue": value,
                    }
                )
    if _is_number(value):
        m, v = _as_float(schema.get("multipleOf")), _as_float(value)
        if m is not None and v is not None and m != 0.0 and (v / m) % 1 != 0:
            push(
                {
                    "operatorName": "multipleOf",
                    "specifiedAs": {"multipleOf": schema["multipleOf"]},
                    "reason": "considered value is not a multiple of the specified value",
                    "consideredValue": value,
                }
            )
        for kw, excl, op, op_excl in (
            ("maximum", "exclusiveMaximum", "$lte", "$lt"),
            ("minimum", "exclusiveMinimum", "$gte", "$gt"),
        ):
            if kw not in schema:
                continue
            exclusive = schema.get(excl) is True
            cmp_op = op_excl if exclusive else op
            if not _filter_matches({"v": value}, {"v": {cmp_op: schema[kw]}}):
                spec: dict[str, Any] = {kw: schema[kw]}
                if excl in schema:
                    spec[excl] = schema[excl]
                push(
                    {
                        "operatorName": kw,
                        "specifiedAs": spec,
                        "reason": "comparison failed",
                        "consideredValue": value,
                    }
                )

    # Array keywords.
    if isinstance(value, list):
        n_items = len(value)
        for kw, too_many in (("minItems", False), ("maxItems", True)):
            limit = _as_count(schema.get(kw))
            if limit is not None and (
                (too_many and n_items > limit) or (not too_many and n_items < limit)
            ):
                push(
                    {
                        "operatorName": kw,
                        "specifiedAs": {kw: schema[kw]},
                        "reason": "array did not match specified length",
                        "consideredValue": value,
                        "numberOfItems": n_items,
                    }
                )
        if schema.get("uniqueItems") is True:
            dup = _MISSING
            for i, a in enumerate(value):
                if any(_value_equal(a, b) for b in value[:i]):
                    dup = a
                    break
            if dup is not _MISSING:
                push(
                    {
                        "operatorName": "uniqueItems",
                        "specifiedAs": {"uniqueItems": True},
                        "reason": "found a duplicate item",
                        "consideredValue": value,
                        "duplicatedValue": dup,
                    }
                )
        items = schema.get("items")
        if isinstance(items, Mapping):
            for i, it in enumerate(value):
                d = _explain_value(items, it, False)
                if d:
                    push(
                        {
                            "operatorName": "items",
                            "reason": "At least one item did not match the sub-schema",
                            "itemIndex": i,
                            "details": d,
                        }
                    )
                    break
        elif isinstance(items, list):
            failing = []
            for i, (s, it) in enumerate(zip(items, value, strict=False)):
                if isinstance(s, Mapping):
                    d = _explain_value(s, it, False)
                    if d:
                        failing.append({"itemIndex": i, "details": d})
            if failing:
                push({"operatorName": "items", "details": failing})
            extra = value[len(items) :]
            if extra:
                add = schema.get("additionalItems")
                if add is False:
                    push(
                        {
                            "operatorName": "additionalItems",
                            "specifiedAs": {"additionalItems": False},
                            "reason": "found additional items",
                            "additionalItems": list(extra),
                        }
                    )
                elif isinstance(add, Mapping):
                    for i, it in enumerate(extra):
                        d = _explain_value(add, it, False)
                        if d:
                            push(
                                {
                                    "operatorName": "additionalItems",
                                    "reason": (
                                        "At least one additional item did not match the sub-schema"
                                    ),
                                    "itemIndex": i + len(items),
                                    "details": d,
                                }
                            )
                            break

    # Object keywords.
    if isinstance(value, Mapping):
        _explain_object(schema, value, push)

    # Logical keywords, then `enum`.
    all_of = _sub_schemas(schema, "allOf")
    if all_of:
        failing_all = _indexed_schemas(all_of, value, top, bool)
        if failing_all:
            push({"operatorName": "allOf", "schemasNotSatisfied": failing_all})
    any_of = _sub_schemas(schema, "anyOf")
    if any_of and not any(_schema_matches(s, value, top) for s in any_of):
        push(
            {
                "operatorName": "anyOf",
                "schemasNotSatisfied": _indexed_schemas(any_of, value, top, lambda _d: True),
            }
        )
    one_of = _sub_schemas(schema, "oneOf")
    if one_of:
        matching = [i for i, s in enumerate(one_of) if _schema_matches(s, value, top)]
        if not matching:
            push(
                {
                    "operatorName": "oneOf",
                    "schemasNotSatisfied": _indexed_schemas(one_of, value, top, lambda _d: True),
                }
            )
        elif len(matching) > 1:
            push(
                {
                    "operatorName": "oneOf",
                    "reason": "more than one subschema matched",
                    "matchingSchemaIndexes": matching,
                }
            )
    nt = schema.get("not")
    if isinstance(nt, Mapping) and _schema_matches(nt, value, top):
        push({"operatorName": "not", "reason": "child expression matched"})
    options = schema.get("enum")
    if isinstance(options, list) and not any(_value_equal(o, value) for o in options):
        push(
            {
                "operatorName": "enum",
                "specifiedAs": {"enum": options},
                "reason": "value was not found in enum",
                "consideredValue": value,
            }
        )

    # The type restriction, last -- and only below the root.
    if not top:
        for kw in ("bsonType", "type"):
            if kw in schema and not _type_matches(kw, schema[kw], value):
                push(
                    {
                        "operatorName": kw,
                        "specifiedAs": {kw: schema[kw]},
                        "reason": "type did not match",
                        "consideredValue": value,
                        "consideredType": bson_type_name(value),
                    }
                )
    return out


_MISSING = object()


def _value_equal(a: Any, b: Any) -> bool:
    """BSON equality for ``enum`` / ``uniqueItems``: numbers compare by value
    across their types, everything else must be the same type and equal."""
    if _is_number(a) and _is_number(b):
        try:
            return _to_decimal(a) == _to_decimal(b)
        except (InvalidOperation, ValueError):
            return False
    if type(a) is not type(b):
        return False
    return bson_equal(a, b)


def _to_decimal(v: Any) -> Decimal:
    if isinstance(v, Decimal128):
        return v.to_decimal()
    return Decimal(repr(v)) if isinstance(v, float) else Decimal(int(v))


def _explain_object(
    schema: Mapping[str, Any], obj: Mapping[str, Any], push: Callable[[Any], None]
) -> None:
    properties = schema.get("properties") if isinstance(schema.get("properties"), Mapping) else None
    if properties is not None:
        failing = []
        for name, sub in properties.items():
            if isinstance(sub, Mapping) and name in obj:
                d = _explain_value(sub, obj[name], False)
                if d:
                    failing.append({"propertyName": name, "details": d})
        if failing:
            push({"operatorName": "properties", "propertiesNotSatisfied": failing})
    patterns = (
        schema.get("patternProperties")
        if isinstance(schema.get("patternProperties"), Mapping)
        else None
    )
    if patterns is not None:
        failing = []
        for name, v in obj.items():
            for pattern, sub in patterns.items():
                if not isinstance(sub, Mapping):
                    continue
                if _filter_matches({"k": name}, {"k": {"$regex": pattern}}):
                    d = _explain_value(sub, v, False)
                    if d:
                        failing.append(
                            {"propertyName": name, "regexMatched": pattern, "details": d}
                        )
        if failing:
            push({"operatorName": "patternProperties", "details": failing})
    if "additionalProperties" in schema:
        additional = schema["additionalProperties"]

        def is_additional(name: str) -> bool:
            if properties is not None and name in properties:
                return False
            return patterns is None or not any(
                _filter_matches({"k": name}, {"k": {"$regex": pat}}) for pat in patterns
            )

        if additional is False:
            extra = [k for k in obj if is_additional(k)]
            if extra:
                push(
                    {
                        "operatorName": "additionalProperties",
                        "specifiedAs": {"additionalProperties": False},
                        "additionalProperties": extra,
                    }
                )
        elif isinstance(additional, Mapping):
            for k, v in obj.items():
                if not is_additional(k):
                    continue
                d = _explain_value(additional, v, False)
                if d:
                    push(
                        {
                            "operatorName": "additionalProperties",
                            "reason": (
                                "at least one additional property did not match the subschema"
                            ),
                            "failingProperty": k,
                            "details": d,
                        }
                    )
                    break
    required = schema.get("required")
    if isinstance(required, list):
        missing = [r for r in required if isinstance(r, str) and r not in obj]
        if missing:
            push(
                {
                    "operatorName": "required",
                    "specifiedAs": {"required": required},
                    "missingProperties": missing,
                }
            )
    n_props = len(obj)
    for kw, too_many in (("minProperties", False), ("maxProperties", True)):
        limit = _as_count(schema.get(kw))
        if limit is not None and (
            (too_many and n_props > limit) or (not too_many and n_props < limit)
        ):
            push(
                {
                    "operatorName": kw,
                    "specifiedAs": {kw: schema[kw]},
                    "reason": "specified number of properties was not satisfied",
                    "numberOfProperties": n_props,
                }
            )
    deps = schema.get("dependencies")
    if isinstance(deps, Mapping):
        failing = []
        for prop, dep in deps.items():
            if prop not in obj:
                continue
            if isinstance(dep, list):
                missing = [n for n in dep if isinstance(n, str) and n not in obj]
                if missing:
                    failing.append({"conditionalProperty": prop, "missingProperties": missing})
            elif isinstance(dep, Mapping):
                d = _explain_value(dep, obj, True)
                if d:
                    failing.append({"conditionalProperty": prop, "details": d})
        if failing:
            push({"operatorName": "dependencies", "failingDependencies": failing})
