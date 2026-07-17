from __future__ import annotations

import copy
import datetime as _dt
from collections.abc import Mapping
from typing import Any

from secantus.numerics import bson_add, bson_mul
from secantus.paths import get_path, has_path, set_path, unset_path


class UpdateError(Exception):
    def __init__(self, message: str, *, code: int | None = None) -> None:
        super().__init__(message)
        self.code = code


def _render_bson_scalar(v: Any) -> str:
    """A mongod-ish rendering of a scalar for an error message: ``true`` /
    ``false`` / ``null`` lowercase, strings double-quoted, else ``str()``."""
    if v is True:
        return "true"
    if v is False:
        return "false"
    if v is None:
        return "null"
    if isinstance(v, str):
        return f'"{v}"'
    return str(v)


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
            raise UpdateError("$position must be an integer")
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
    document sorts (stably, field-by-field) by those paths."""
    from secantus.storage import _SortKey

    if isinstance(spec, int) and not isinstance(spec, bool):
        return sorted(arr, key=_SortKey, reverse=(spec == -1))
    if isinstance(spec, Mapping):
        result = list(arr)
        for field, direction in reversed(list(spec.items())):
            result.sort(
                key=lambda e, f=field: _SortKey(get_path(e, f) if isinstance(e, Mapping) else e),
                reverse=(int(direction) == -1),
            )
        return result
    raise UpdateError("$sort requires 1, -1, or a document")


def _push_slice(arr: list[Any], n: Any) -> list[Any]:
    """`$push` `$slice`: keep the first `n` (n≥0), the last `|n|` (n<0), or none."""
    if not isinstance(n, int) or isinstance(n, bool):
        raise UpdateError("$slice must be an integer")
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
        new["_id"] = doc["_id"]
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


def _index_array_filters(
    filters: list[Mapping[str, Any]],
) -> dict[str, Mapping[str, Any]]:
    out: dict[str, Mapping[str, Any]] = {}
    for f in filters:
        if not isinstance(f, Mapping):
            raise UpdateError("each arrayFilter must be a document")
        for key in f:
            name = key.split(".", 1)[0]
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
            for concrete in _expand(doc, path, array_filters, positional_matches):
                if opts is True:
                    set_path(doc, concrete, _dt.datetime.now(_dt.timezone.utc))
                    continue
                if isinstance(opts, Mapping):
                    kind = opts.get("$type")
                    if kind == "date":
                        set_path(doc, concrete, _dt.datetime.now(_dt.timezone.utc))
                        continue
                    if kind == "timestamp":
                        import time as _time

                        import bson as _bson

                        set_path(doc, concrete, _bson.Timestamp(int(_time.time()), 0))
                        continue
                raise UpdateError(f"$currentDate option for {path!r} not understood")
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
                    if current is None:
                        raise UpdateError(
                            f"Cannot apply $inc to a value of non-numeric type. "
                            f"{{{concrete}}} has the field '{concrete.split('.')[-1]}' "
                            f"of non-numeric type null",
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
                    if current is None:
                        raise UpdateError(
                            f"Cannot apply $mul to a value of non-numeric type. "
                            f"{{{concrete}}} has the field '{concrete.split('.')[-1]}' "
                            f"of non-numeric type null",
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
                    if elem not in arr:
                        arr.append(elem)
                set_path(doc, concrete, arr)
    elif op == "$pull":
        from secantus.query import matches

        for path, criterion in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list):
                    arr[:] = [e for e in arr if not _pull_matches(matches, e, criterion)]
    elif op == "$pullAll":
        for path, values in payload.items():
            if not isinstance(values, list):
                raise UpdateError("$pullAll requires an array argument")
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list):
                    arr[:] = [e for e in arr if not any(e == v for v in values)]
    elif op == "$pop":
        for path, direction in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list) and arr:
                    if direction == 1:
                        arr.pop()
                    elif direction == -1:
                        arr.pop(0)
    elif op == "$rename":
        for old, new in payload.items():
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
                    raise UpdateError("$bit mask must be an integer")
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
