from __future__ import annotations

import copy
import datetime as _dt
import re
from collections.abc import Mapping
from typing import Any

from secantus.numerics import bson_add, bson_mul
from secantus.paths import get_path, has_path, set_path, unset_path

_ARRAY_FILTER_TOKEN = re.compile(r"\$\[([^\]]*)\]")
# An arrayFilter identifier: begins with a lowercase letter, then alphanumeric.
_ARRAY_FILTER_IDENT = re.compile(r"^[a-z][a-zA-Z0-9]*$")
# Logical operators whose sub-clauses an arrayFilter identifier can nest inside.
_ARRAY_FILTER_LOGICAL = ("$and", "$or", "$nor")


def _extract_af_identifiers(f: Mapping[str, Any]) -> tuple[list[str], bool]:
    """The ordered, de-duplicated arrayFilter identifiers referenced by a filter
    — the top-level field name (before the first ``.``) of each non-``$`` key,
    recursing through ``$and`` / ``$or`` / ``$nor`` sub-clauses — plus whether a
    ``$expr`` was seen (which mongod rejects in this context). Mirrors mongod's
    single-identifier extraction."""
    idents: list[str] = []
    seen: set[str] = set()
    saw_expr = False

    def walk(m: Mapping[str, Any]) -> None:
        nonlocal saw_expr
        for key, value in m.items():
            if key == "$expr":
                saw_expr = True
            elif key in _ARRAY_FILTER_LOGICAL:
                if isinstance(value, list):
                    for sub in value:
                        if isinstance(sub, Mapping):
                            walk(sub)
            elif not key.startswith("$"):
                ident = key.split(".", 1)[0]
                if ident not in seen:
                    seen.add(ident)
                    idents.append(ident)

    walk(f)
    return idents, saw_expr


class UpdateError(Exception):
    def __init__(self, message: str, *, code: int | None = None) -> None:
        super().__init__(message)
        self.code = code


def _is_inc_numeric(v: Any) -> bool:
    """Whether `$inc` / `$mul` may operate on an existing field value.

    mongod's numeric type for arithmetic is int32 / int64 / double / Decimal128.
    `bool` is deliberately excluded even though Python treats it as an int —
    mongod answers TypeMismatch (14) for `$inc` against `true`.
    """
    from bson import Decimal128, Int64

    if isinstance(v, bool):
        return False
    return isinstance(v, (int, float, Int64, Decimal128))


def _addtoset_equal(a: Any, b: Any) -> bool:
    """mongod's `$addToSet` membership test: exact, field-order-sensitive.

    Delegates to the query matcher's equality so the two can never drift — the
    same rule decides whether `{a: <doc>}` matches and whether `$addToSet` treats
    the value as already present.
    """
    from secantus.query import _eq_numeric_aware

    return _eq_numeric_aware(a, b)


def _bson_type_name(v: Any) -> str:
    """mongod's type vocabulary for update parse-error messages."""
    from bson import Decimal128, Int64

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
    if v is None:
        return "null"
    if isinstance(v, Mapping):
        return "object"
    if isinstance(v, (list, tuple)):
        return "array"
    return type(v).__name__


def _render_bson_scalar(v: Any) -> str:
    """A mongod-ish rendering of a scalar for an error message: ``true`` /
    ``false`` / ``null`` lowercase, strings double-quoted, ObjectId in its
    constructor form, else ``str()``."""
    from bson import ObjectId

    if v is True:
        return "true"
    if v is False:
        return "false"
    if v is None:
        return "null"
    if isinstance(v, str):
        return f'"{v}"'
    if isinstance(v, ObjectId):
        # `str(ObjectId)` is the bare hex; mongod prints `ObjectId('…')`, and
        # this is the *default* `_id` type, so it's the common case in the
        # `$inc`/`$mul` type-error message below.
        return f"ObjectId('{v}')"
    return str(v)


def _render_doc_id(doc: Mapping[str, Any]) -> str:
    """The ``{_id: …}`` prefix mongod puts in an `$inc`/`$mul` type error.

    Probed vs mongod 6.0.16: the braces hold the *document's `_id`*, not the
    field being incremented — `{_id: 1} has the field 'n' …`. We used to render
    the field path there (`{n} has the field 'n' …`), which is the right code
    (14) attached to a message no real server ever emits.
    """
    if "_id" not in doc:
        return "{}"
    return f"{{_id: {_render_bson_scalar(doc['_id'])}}}"


def _require_numeric_operand(verb: str, path: str, value: Any) -> None:
    """mongod rejects ``$inc``/``$mul`` by a non-number with code 14, e.g.
    ``Cannot increment with non-numeric argument: {n: true}``. bool is NOT a
    number here (Python's ``bool`` is an ``int`` subclass, so ``5 + True`` would
    otherwise compute); string / null / etc. also error rather than raising a
    raw ``ValueError``/``TypeError`` from the arithmetic. Probed vs mongod
    7.0.12."""
    from bson import Decimal128

    if isinstance(value, bool) or not isinstance(value, (int, float, Decimal128)):
        raise UpdateError(
            f"Cannot {verb} with non-numeric argument: {{{path}: {_render_bson_scalar(value)}}}",
            code=14,
        )


# The update modifiers ``_apply_op`` knows how to apply. Used by
# ``validate_update_doc`` to reject an unknown modifier at parse time
# (mongod validates the update before matching any documents, so an
# unknown operator errors even against an empty collection).
_KNOWN_UPDATE_OPS = frozenset(
    {
        "$set",
        "$setOnInsert",
        "$unset",
        "$currentDate",
        "$inc",
        "$mul",
        "$min",
        "$max",
        "$push",
        "$addToSet",
        "$pull",
        "$pullAll",
        "$pop",
        "$rename",
        "$bit",
    }
)


def _is_each_modifier(value: Any) -> bool:
    """Whether a `$push` / `$addToSet` value is the `{$each: [...], ...}` modifier
    form (vs a plain value to append)."""
    return isinstance(value, Mapping) and "$each" in value


def _apply_push(arr: list[Any], value: Any) -> list[Any]:
    """Apply one `$push` to `arr` (a fresh copy). A plain value is appended; the
    `{$each: [...]}` modifier form appends each element, honouring `$position`
    (insert index, negative counts from the end), then `$sort` (whole-element
    `1`/`-1` or a `{field: dir}` doc, in BSON order), then `$slice` (keep the
    first N for N≥0, the last |N| for N<0, empty for 0) — mongod's modifier order.
    """
    if not _is_each_modifier(value):
        arr.append(value)
        return arr
    allowed = {"$each", "$position", "$slice", "$sort"}
    unknown = set(value) - allowed
    if unknown:
        raise UpdateError(f"Unrecognized $push modifier: {next(iter(unknown))!r}")
    each = value["$each"]
    if not isinstance(each, list):
        raise UpdateError("$each must be an array")
    position = value.get("$position")
    if position is not None:
        if not isinstance(position, int) or isinstance(position, bool):
            raise UpdateError(
                "The value for $position must be an integer value, not of type: "
                f"{_bson_type_name(position)}",
                code=2,
            )
        idx = position if position >= 0 else max(len(arr) + position, 0)
        arr[idx:idx] = each
    else:
        arr.extend(each)
    if "$sort" in value:
        arr = _push_sort(arr, value["$sort"])
    if "$slice" in value:
        arr = _push_slice(arr, value["$slice"])
    return arr


def _push_sort(arr: list[Any], spec: Any) -> list[Any]:
    """`$push` `$sort`: `1`/`-1` sorts whole elements (BSON order); a `{field: dir}`
    document sorts (stably, field-by-field) by those paths. mongod validates the
    spec: a numeric whole-element sort must be exactly ±1 (a whole double is
    accepted), each document direction must be ±1, and anything else (string,
    bool, array) is rejected."""
    from secantus.storage import _SortKey

    if not isinstance(spec, bool) and isinstance(spec, (int, float)):
        if spec != 1 and spec != -1:
            raise UpdateError("The $sort element value must be either 1 or -1", code=2)
        return sorted(arr, key=_SortKey, reverse=(spec == -1))
    if isinstance(spec, Mapping):
        for direction in spec.values():
            if (
                isinstance(direction, bool)
                or not isinstance(direction, (int, float))
                or (direction != 1 and direction != -1)
            ):
                raise UpdateError("The sort element value must be either 1 or -1", code=2)
        result = list(arr)
        for field, direction in reversed(list(spec.items())):
            result.sort(
                key=lambda e, f=field: _SortKey(get_path(e, f) if isinstance(e, Mapping) else e),
                reverse=(direction == -1),
            )
        return result
    raise UpdateError(
        "The $sort is invalid: use 1/-1 to sort the whole element, or "
        "{field:1/-1} to sort embedded fields",
        code=2,
    )


def _push_slice(arr: list[Any], n: Any) -> list[Any]:
    """`$push` `$slice`: keep the first `n` (n≥0), the last `|n|` (n<0), or none."""
    if not isinstance(n, int) or isinstance(n, bool):
        raise UpdateError(
            "The value for $slice must be an integer value but was given "
            f"type: {_bson_type_name(n)}",
            code=2,
        )
    if n == 0:
        return []
    return arr[:n] if n > 0 else arr[n:]


def _pull_matches(matches: Any, element: Any, criterion: Any) -> bool:
    """Whether an array element should be removed by ``$pull`` under mongod's
    query semantics (verified three-way vs mongod 6.0):

    - a criterion of only ``$``-operators (``{$gte: 10}``) is an **element-value
      predicate** — each element is tested as the value;
    - any other document criterion (``{x: {$gte: 5}}``, ``{y: "b"}``, ``{b.c: 2}``)
      is a **sub-document match** against the element (a scalar element never
      matches, so it stays);
    - a scalar criterion is equality (BSON-aware, via the same query engine).
    """
    if isinstance(criterion, Mapping):
        if criterion and all(isinstance(k, str) and k.startswith("$") for k in criterion):
            return matches({"__e": element}, {"__e": criterion})
        return matches(element, criterion) if isinstance(element, Mapping) else False
    return matches({"__e": element}, {"__e": criterion})


def validate_update_doc(update: Any) -> None:
    """Parse-time validation of an update document's top-level operators.

    Raises ``UpdateError`` for an unknown modifier or a mix of operators
    and replacement fields. Does NOT apply the update (so positional /
    arrayFilter operators don't need a match context here). Pipeline
    (list) updates and pure replacements are accepted — their own
    validation happens elsewhere.
    """
    if isinstance(update, list) or not isinstance(update, Mapping):
        return
    keys = list(update)
    if not any(k.startswith("$") for k in keys):
        return  # replacement-style update
    for op in keys:
        if not op.startswith("$"):
            raise UpdateError("update document cannot mix operators with replacement fields")
        if op not in _KNOWN_UPDATE_OPS:
            raise UpdateError(
                f"Unknown modifier: {op}. Expected a valid update modifier "
                "(e.g. $set, $unset, $inc, ...)"
            )


def apply_update_batch(
    docs: list[dict[str, Any]],
    update: Mapping[str, Any] | list[Mapping[str, Any]],
    *,
    is_upsert: bool = False,
) -> list[dict[str, Any]]:
    """Apply one operator/replacement ``update`` to every doc in ``docs``.

    A thin convenience over the per-doc ``apply_update``; pipeline /
    array-filter / positional updates are applied per doc the same way.
    """
    return [apply_update(d, update, is_upsert=is_upsert) for d in docs]


def apply_update(
    doc: dict[str, Any],
    update: Mapping[str, Any] | list[Mapping[str, Any]],
    *,
    is_upsert: bool = False,
    array_filters: list[Mapping[str, Any]] | None = None,
    positional_matches: Mapping[str, int] | None = None,
    let: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if isinstance(update, list):
        return _apply_pipeline_update(doc, update, let=let)
    if not update:
        return copy.deepcopy(doc)
    keys = list(update.keys())
    has_op = any(k.startswith("$") for k in keys)
    _validate_array_filters(array_filters or [], update)
    filter_map = _index_array_filters(array_filters or [])
    pos = dict(positional_matches) if positional_matches else {}
    if has_op:
        if not all(k.startswith("$") for k in keys):
            raise UpdateError("update document cannot mix operators with replacement fields")
        result = copy.deepcopy(doc)
        for op, payload in update.items():
            if op == "$setOnInsert" and not is_upsert:
                continue
            _apply_op(result, op, payload, filter_map, pos)
        # ``_id`` is immutable in every server version — ``$set:
        # {_id: ...}`` and friends are rejected post-apply.
        # Mongo-go-driver's
        # ``TestCollection/bulk_write/update_write_errors`` test
        # asserts mongod's error code 66 (ImmutableField) when an
        # operator update tries to change ``_id``.
        if "_id" in doc and result.get("_id") != doc.get("_id"):
            raise UpdateError(
                "Performing an update on the path '_id' would modify the immutable field '_id'"
            )
        return result
    new = copy.deepcopy(dict(update))
    if "_id" in doc:
        if "_id" in new and new["_id"] != doc["_id"]:
            raise UpdateError(
                "Performing an update on the path '_id' would modify the immutable field '_id'"
            )
        # ``_id`` leads the stored document, as it does in mongod. Assigning
        # into ``new`` would APPEND it when the replacement omits ``_id``,
        # and BSON preserves field order on the wire — so the client gets
        # back a document that differs from mongod's byte for byte.
        # mongo-php-library's CodecCollectionFunctionalTest compares the raw
        # BSON of a findOneAndReplace result and caught exactly that.
        # Applied unconditionally, not just when the replacement omits
        # ``_id``: mongod always stores it first, whatever position the
        # client put it in.
        rest = {k: v for k, v in new.items() if k != "_id"}
        return {"_id": doc["_id"], **rest}
    return new


def find_positional_matches(doc: Mapping[str, Any], filter_: Mapping[str, Any]) -> dict[str, int]:
    from secantus.query import matches as _matches

    out: dict[str, int] = {}
    array_paths: dict[str, dict[str, Any]] = {}
    for key, value in filter_.items():
        if key.startswith("$") or "." not in key:
            continue
        top, _, rest = key.partition(".")
        if isinstance(doc.get(top), list):
            array_paths.setdefault(top, {})[rest] = value
    for path, sub_filter in array_paths.items():
        arr = doc.get(path)
        if not isinstance(arr, list):
            continue
        for i, elem in enumerate(arr):
            elem_doc = elem if isinstance(elem, Mapping) else {"_": elem}
            if _matches(elem_doc, sub_filter):
                out[path] = i
                break
    return out


def _array_filter_referenced_identifiers(update: Mapping[str, Any]) -> set[str]:
    """Every arrayFilter identifier referenced by a ``$[<id>]`` token in any
    update-operator field path (e.g. ``a.$[x].b`` references ``x``)."""
    out: set[str] = set()
    for payload in update.values():
        if isinstance(payload, Mapping):
            for path in payload:
                out.update(_ARRAY_FILTER_TOKEN.findall(path))
    return out


def _render_update_for_error(update: Mapping[str, Any]) -> str:
    """Render an operator update the way mongod prints it in the unused-array-filter
    error, e.g. ``{ $set: { a.$[x]: 9 } }``."""

    def val(v: Any) -> str:
        if isinstance(v, bool):
            return "true" if v else "false"
        if isinstance(v, str):
            return f'"{v}"'
        if v is None:
            return "null"
        return str(v)

    parts: list[str] = []
    for op, payload in update.items():
        if isinstance(payload, Mapping):
            inner = ", ".join(f"{k}: {val(x)}" for k, x in payload.items())
            parts.append(f"{op}: {{ {inner} }}")
        else:
            parts.append(f"{op}: {val(payload)}")
    return "{ " + ", ".join(parts) + " }"


def _validate_array_filters(filters: list[Mapping[str, Any]], update: Mapping[str, Any]) -> None:
    """mongod validates arrayFilters before applying: each must be an object
    (14) referencing exactly one identifier — the top-level field name, which may
    nest inside ``$and`` / ``$or`` / ``$nor`` (none → 9 / 224 for ``$expr``; two
    or more distinct → 9); the name must be alphanumeric beginning with a
    lowercase letter (2), unique across filters (9), and used by a ``$[<id>]``
    path in the update (9)."""
    if not filters:
        return
    seen: set[str] = set()
    identifiers: list[str] = []
    for i, f in enumerate(filters):
        if not isinstance(f, Mapping):
            raise UpdateError(
                f"BSON field 'update.updates.arrayFilters.{i}' is the wrong type "
                f"'{_bson_type_name(f)}', expected type 'object'",
                code=14,
            )
        found, saw_expr = _extract_af_identifiers(f)
        if not found:
            if saw_expr:
                raise UpdateError(
                    "Error parsing array filter :: caused by :: $expr is not allowed "
                    "in this context",
                    code=224,
                )
            raise UpdateError(
                "Cannot use an expression without a top-level field name in arrayFilters",
                code=9,
            )
        if len(found) > 1:
            raise UpdateError(
                "Error parsing array filter :: caused by :: Expected a single top-level "
                f"field name, found '{found[0]}' and '{found[1]}'",
                code=9,
            )
        ident = found[0]
        if not _ARRAY_FILTER_IDENT.match(ident):
            raise UpdateError(
                "Error parsing array filter :: caused by :: The top-level field name "
                "must be an alphanumeric string beginning with a lowercase letter, "
                f"found '{ident}'",
                code=2,
            )
        if ident in seen:
            raise UpdateError(
                f"Found multiple array filters with the same top-level field name {ident}",
                code=9,
            )
        seen.add(ident)
        identifiers.append(ident)
    referenced = _array_filter_referenced_identifiers(update)
    for ident in identifiers:
        if ident not in referenced:
            raise UpdateError(
                f"The array filter for identifier '{ident}' was not used in the update "
                f"{_render_update_for_error(update)}",
                code=9,
            )


def _index_array_filters(
    filters: list[Mapping[str, Any]],
) -> dict[str, Mapping[str, Any]]:
    out: dict[str, Mapping[str, Any]] = {}
    for f in filters:
        if not isinstance(f, Mapping):
            raise UpdateError("each arrayFilter must be a document")
        # The identifier may nest inside $and/$or/$nor (validated already).
        for name in _extract_af_identifiers(f)[0]:
            out.setdefault(name, f)
    return out


def _expand_path(
    doc: Mapping[str, Any] | list[Any],
    path: str,
    array_filters: dict[str, Mapping[str, Any]],
    positional_matches: Mapping[str, int],
) -> list[str]:
    parts = path.split(".")
    if not any(_is_positional_token(p) for p in parts):
        return [path]
    out: list[str] = []
    _walk_positional(doc, parts, [], out, array_filters, positional_matches)
    return out


def _is_positional_token(part: str) -> bool:
    return part == "$" or part == "$[]" or (part.startswith("$[") and part.endswith("]"))


def _walk_positional(
    cur: Any,
    remaining: list[str],
    prefix: list[str],
    out: list[str],
    array_filters: dict[str, Mapping[str, Any]],
    positional_matches: Mapping[str, int],
) -> None:
    if not remaining:
        out.append(".".join(prefix))
        return
    head, *rest = remaining
    if head == "$":
        if not isinstance(cur, list):
            return
        path_so_far = ".".join(prefix)
        idx = positional_matches.get(path_so_far)
        if idx is None or not (0 <= idx < len(cur)):
            raise UpdateError(
                f"$ positional update for {path_so_far!r} could not resolve a matched index"
            )
        _walk_positional(
            cur[idx], rest, prefix + [str(idx)], out, array_filters, positional_matches
        )
        return
    if head == "$[]":
        if not isinstance(cur, list):
            return
        for i, elem in enumerate(cur):
            _walk_positional(elem, rest, prefix + [str(i)], out, array_filters, positional_matches)
        return
    if head.startswith("$[") and head.endswith("]"):
        name = head[2:-1]
        if not isinstance(cur, list):
            return
        sub_filter = array_filters.get(name)
        if sub_filter is None:
            raise UpdateError(f"arrayFilters has no entry for identifier {name!r}")
        from secantus.query import matches as _matches

        for i, elem in enumerate(cur):
            if _matches({name: elem}, sub_filter):
                _walk_positional(
                    elem, rest, prefix + [str(i)], out, array_filters, positional_matches
                )
        return
    if isinstance(cur, Mapping):
        _walk_positional(
            cur.get(head), rest, prefix + [head], out, array_filters, positional_matches
        )
    elif isinstance(cur, list) and head.isdigit():
        idx = int(head)
        if 0 <= idx < len(cur):
            _walk_positional(
                cur[idx], rest, prefix + [head], out, array_filters, positional_matches
            )


_PIPELINE_UPDATE_STAGES = {
    "$set",
    "$addFields",
    "$unset",
    "$project",
    "$replaceRoot",
    "$replaceWith",
}


def _apply_pipeline_update(
    doc: dict[str, Any],
    pipeline: list[Mapping[str, Any]],
    *,
    let: dict[str, Any] | None = None,
) -> dict[str, Any]:
    for stage in pipeline:
        if not isinstance(stage, Mapping) or len(stage) != 1:
            raise UpdateError("each pipeline stage must be a single-key document")
        (name,) = stage.keys()
        if name not in _PIPELINE_UPDATE_STAGES:
            raise UpdateError(f"stage {name} not allowed in pipeline updates")
    from secantus.aggregate import PipelineContext, apply_pipeline

    # Thread ``let`` user-vars into the pipeline context so
    # ``$$varname`` references inside pipeline-update stages
    # (e.g. ``{$set: {x: "$$x"}}``) resolve via the let map.
    ctx = PipelineContext(vars=dict(let) if let else {})
    result = apply_pipeline([doc], list(pipeline), ctx)
    if not result:
        return copy.deepcopy(doc)
    new = result[0]
    if "_id" in doc:
        # ``$replaceRoot`` and ``$replaceWith`` can drop ``_id`` from
        # the result. Real mongod preserves the original ``_id`` in
        # that case — only an explicit *change* to a different
        # ``_id`` is rejected. Mongo-java-driver's
        # ``updateOne-pipeline`` and ``bulkWrite-updateOne-pipeline``
        # tests rely on this (the pipeline reroots a sub-document
        # that has no ``_id``).
        if "_id" not in new:
            new["_id"] = doc["_id"]
        elif new["_id"] != doc["_id"]:
            raise UpdateError(
                "Performing an update on the path '_id' would modify the immutable field '_id'"
            )
    return new


def _expand(
    doc: dict[str, Any],
    path: str,
    array_filters: dict[str, Mapping[str, Any]],
    positional_matches: Mapping[str, int],
) -> list[str]:
    return _expand_path(doc, path, array_filters, positional_matches)


def _rename_same_path(a: str, b: str) -> bool:
    """True if two dotted `$rename` paths are equal or one is an ancestor of the
    other (mongod: source and target must not be on the same path)."""
    ap, bp = a.split("."), b.split(".")
    n = min(len(ap), len(bp))
    return ap[:n] == bp[:n]


def _rename_traverses_array(doc: dict[str, Any], path: str) -> bool:
    """True if the *literal* `path` indexes into an array with a numeric index
    (e.g. `arr.0`) — the "array element" mongod forbids in a $rename source /
    destination (it silently corrupted the array here). A positional token
    (`$` / `$[]` / `$[id]`) into an array is NOT flagged: those are a SecantusDB
    $rename extension resolved element-wise elsewhere."""
    cur: Any = doc
    for part in path.split("."):
        if isinstance(cur, list):
            return part.isdigit()
        if isinstance(cur, Mapping) and part in cur:
            cur = cur[part]
        else:
            return False
    return False


def _apply_op(
    doc: dict[str, Any],
    op: str,
    payload: Mapping[str, Any],
    array_filters: dict[str, Mapping[str, Any]],
    positional_matches: Mapping[str, int],
) -> None:
    if op == "$set" or op == "$setOnInsert":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                set_path(doc, concrete, value)
    elif op == "$unset":
        for path in payload:
            for concrete in _expand(doc, path, array_filters, positional_matches):
                unset_path(doc, concrete)
    elif op == "$currentDate":
        for path, opts in payload.items():
            # mongod: the argument is a boolean (true OR false — both set the
            # current Date) or a `{$type: "date"|"timestamp"}` object. A non-bool
            # scalar and a bad/missing $type are both code 2.
            if isinstance(opts, bool):
                stamp: Any = _dt.datetime.now(_dt.timezone.utc)
            elif isinstance(opts, Mapping):
                kind = opts.get("$type")
                if kind == "date":
                    stamp = _dt.datetime.now(_dt.timezone.utc)
                elif kind == "timestamp":
                    import time as _time

                    import bson as _bson

                    stamp = _bson.Timestamp(int(_time.time()), 0)
                else:
                    raise UpdateError(
                        "The '$type' string field is required to be 'date' or 'timestamp': "
                        "{$currentDate: {field : {$type: 'date'}}}",
                        code=2,
                    )
            else:
                raise UpdateError(
                    f"{_bson_type_name(opts)} is not valid type for $currentDate. Please use "
                    "a boolean ('true') or a $type expression ({$type: 'timestamp/date'}).",
                    code=2,
                )
            for concrete in _expand(doc, path, array_filters, positional_matches):
                set_path(doc, concrete, stamp)
    elif op == "$inc":
        for path, delta in payload.items():
            _require_numeric_operand("increment", path, delta)
            for concrete in _expand(doc, path, array_filters, positional_matches):
                # A missing field is treated as 0 (mongod applies the delta),
                # but a field present with an explicit ``null`` (or any other
                # non-numeric value) is a TypeMismatch (code 14) — the field
                # exists and is not numeric, so mongod refuses to coerce it.
                if not has_path(doc, concrete):
                    current: Any = 0
                else:
                    current = get_path(doc, concrete)
                    # Every non-numeric type is refused, not just null. This used
                    # to check `is None` alone, so a string field reached
                    # `bson_add` and raised a bare `ValueError: invalid literal
                    # for int()` that escaped as "internal server error" (code 1),
                    # and a bool silently incremented — `{n: true}` became `n: 2`
                    # where mongod refuses. bool is checked explicitly because
                    # Python makes it a subclass of int.
                    if not _is_inc_numeric(current):
                        raise UpdateError(
                            f"Cannot apply $inc to a value of non-numeric type. "
                            f"{_render_doc_id(doc)} has the field '{concrete.split('.')[-1]}' "
                            f"of non-numeric type {_bson_type_name(current)}",
                            code=14,
                        )
                # bson_add preserves the BSON numeric type (mongod widens
                # int32 < int64 < double < decimal128) — Int64(5) + 3 → Int64(8),
                # not a bare int that narrows to int32 on the wire.
                set_path(doc, concrete, bson_add(current, delta))
    elif op == "$mul":
        for path, factor in payload.items():
            _require_numeric_operand("multiply", path, factor)
            for concrete in _expand(doc, path, array_filters, positional_matches):
                if not has_path(doc, concrete):
                    current = 0
                else:
                    current = get_path(doc, concrete)
                    # Same defect as `$inc` had: checking only `is None` let a
                    # string reach `bson_mul` (bare ValueError -> "internal server
                    # error") and silently multiplied a bool.
                    if not _is_inc_numeric(current):
                        raise UpdateError(
                            f"Cannot apply $mul to a value of non-numeric type. "
                            f"{_render_doc_id(doc)} has the field '{concrete.split('.')[-1]}' "
                            f"of non-numeric type {_bson_type_name(current)}",
                            code=14,
                        )
                set_path(doc, concrete, bson_mul(current, factor))
    elif op == "$min":
        # A missing field is set unconditionally; otherwise compare by MongoDB's
        # BSON cross-type order (`_bson_lt`), not Python `<` — so a cross-type
        # pair (e.g. a string vs a number) orders like mongod instead of raising
        # a TypeError, and an explicit-null current is a real value (rank 2), not
        # "no current".
        from secantus.ordering import _bson_lt

        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                if not has_path(doc, concrete) or _bson_lt(value, get_path(doc, concrete)):
                    set_path(doc, concrete, value)
    elif op == "$max":
        from secantus.ordering import _bson_lt

        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                if not has_path(doc, concrete) or _bson_lt(get_path(doc, concrete), value):
                    set_path(doc, concrete, value)
    elif op == "$push":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if arr is None:
                    arr = []
                elif isinstance(arr, list):
                    arr = list(arr)
                else:
                    raise UpdateError(f"$push on non-array at {concrete!r}")
                set_path(doc, concrete, _apply_push(arr, value))
    elif op == "$addToSet":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if arr is None:
                    arr = []
                elif isinstance(arr, list):
                    arr = list(arr)
                else:
                    raise UpdateError(f"$addToSet on non-array at {concrete!r}")
                # `$each` adds each element (deduped); otherwise the value itself.
                to_add = value["$each"] if _is_each_modifier(value) else [value]
                if _is_each_modifier(value) and not isinstance(value["$each"], list):
                    raise UpdateError("$each must be an array")
                for elem in to_add:
                    # `elem not in arr` uses Python `==`, which compares dicts
                    # ORDER-INSENSITIVELY. mongod does not: `{y: 2, x: 1}` is a
                    # different value from `{x: 1, y: 2}` and gets added as a
                    # separate element. Probed against 6.0.16, where the reordered
                    # doc yields `[{x:1,y:2}, {y:2,x:1}]` and a query for the
                    # reordered form matches nothing. Our query matcher already
                    # gets this right (`query._eq_numeric_aware` walks pairs in
                    # order); `$addToSet` was the odd one out, silently dropping
                    # an element mongod keeps.
                    if not any(_addtoset_equal(elem, existing) for existing in arr):
                        arr.append(elem)
                set_path(doc, concrete, arr)
    elif op == "$pull":
        from secantus.query import matches

        for path, criterion in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list):
                    arr[:] = [e for e in arr if not _pull_matches(matches, e, criterion)]
                elif has_path(doc, concrete):
                    # mongod: a present but non-array target errors; a missing
                    # field is a silent no-op.
                    raise UpdateError("Cannot apply $pull to a non-array value", code=2)
    elif op == "$pullAll":
        for path, values in payload.items():
            if not isinstance(values, list):
                raise UpdateError("$pullAll requires an array argument")
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list):
                    arr[:] = [e for e in arr if not any(e == v for v in values)]
                elif has_path(doc, concrete):
                    raise UpdateError("Cannot apply $pull to a non-array value", code=2)
    elif op == "$pop":
        for path, direction in payload.items():
            # mongod validates the $pop argument (probed 7.0.12): a bool is
            # "not a number" (code 9), and a number other than ±1 is
            # "$pop expects 1 or -1" (code 9). Python's bool-is-int would treat
            # `True` as `1` (pop last) without this guard.
            if isinstance(direction, bool) or not isinstance(direction, (int, float)):
                raise UpdateError(
                    f"Expected a number in: {path}: {_render_bson_scalar(direction)}", code=9
                )
            if direction not in (1, -1):
                raise UpdateError(
                    f"$pop expects 1 or -1, found: {_render_bson_scalar(direction)}", code=9
                )
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list) and arr:
                    if direction == 1:
                        arr.pop()
                    else:  # direction == -1
                        arr.pop(0)
    elif op == "$rename":
        for old, new in payload.items():
            # mongod validates the whole $rename spec before touching the doc —
            # otherwise several of these silently corrupt data or leak a raw
            # Python exception (e.g. a non-string target hit `new.split`).
            if not isinstance(new, str):
                tgt = "true" if new is True else "false" if new is False else str(new)
                raise UpdateError(
                    f"The 'to' field for $rename must be a string: {old}: {tgt}", code=2
                )
            if old == "" or new == "":
                raise UpdateError("An empty update path is not valid.", code=56)
            if old == new:
                raise UpdateError(
                    f'The source and target field for $rename must differ: {old}: "{new}"',
                    code=2,
                )
            if _rename_same_path(old, new):
                raise UpdateError(
                    "The source and target field for $rename must not be on the same "
                    f'path: {old}: "{new}"',
                    code=2,
                )
            if _rename_traverses_array(doc, old):
                raise UpdateError(
                    f"The source field cannot be an array element, '{old}' in doc "
                    f"with _id: {doc.get('_id')} has an array field",
                    code=2,
                )
            if _rename_traverses_array(doc, new):
                raise UpdateError(
                    f"The destination field cannot be an array element, '{new}' in doc "
                    f"with _id: {doc.get('_id')} has an array field",
                    code=2,
                )
            old_paths = _expand(doc, old, array_filters, positional_matches)
            new_paths = _expand(doc, new, array_filters, positional_matches)
            if len(old_paths) != len(new_paths):
                raise UpdateError(
                    "$rename source and target positional expansions must produce "
                    "the same number of concrete paths"
                )
            for op_path, np_path in zip(old_paths, new_paths, strict=True):
                # `_id` is immutable in mongod (error code 66
                # ImmutableField). $rename targeting (or sourcing from)
                # _id would silently overwrite it without this guard.
                if np_path == "_id" or op_path == "_id":
                    raise UpdateError(
                        "Performing an update on the path '_id' would modify "
                        "the immutable field '_id' (mongod code 66 ImmutableField)"
                    )
                if has_path(doc, op_path):
                    value = get_path(doc, op_path)
                    unset_path(doc, op_path)
                    set_path(doc, np_path, value)
    elif op == "$bit":
        for path, ops in payload.items():
            # mongod applies every listed operation to the field in order
            # (e.g. {and: X, or: Y} is (v & X) | Y), not just a single op.
            if not isinstance(ops, Mapping) or not ops:
                raise UpdateError("$bit requires a document with at least one bitwise operation")
            parsed_ops: list[tuple[str, int]] = []
            for bit_op, mask in ops.items():
                if bit_op not in ("and", "or", "xor"):
                    raise UpdateError(f"$bit unsupported sub-op: {bit_op}")
                if not isinstance(mask, int) or isinstance(mask, bool):
                    raise UpdateError(
                        "The $bit modifier field must be an Integer(32/64 bit); a "
                        f"'{_bson_type_name(mask)}' is not supported here.",
                        code=2,
                    )
                parsed_ops.append((bit_op, mask))
            for concrete in _expand(doc, path, array_filters, positional_matches):
                current = get_path(doc, concrete, default=0) or 0
                if not isinstance(current, int) or isinstance(current, bool):
                    raise UpdateError(f"$bit on non-integer at {concrete!r}")
                for bit_op, mask in parsed_ops:
                    if bit_op == "and":
                        current = current & mask
                    elif bit_op == "or":
                        current = current | mask
                    else:
                        current = current ^ mask
                set_path(doc, concrete, current)
    else:
        raise UpdateError(f"unsupported update operator: {op}")
