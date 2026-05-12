from __future__ import annotations

import copy
import datetime as _dt
from collections.abc import Mapping
from typing import Any

from secantus.paths import get_path, has_path, set_path, unset_path


class UpdateError(Exception):
    pass


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
            for concrete in _expand(doc, path, array_filters, positional_matches):
                current = get_path(doc, concrete, default=0)
                if current is None:
                    current = 0
                set_path(doc, concrete, current + delta)
    elif op == "$mul":
        for path, factor in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                current = get_path(doc, concrete, default=0)
                if current is None:
                    current = 0
                set_path(doc, concrete, current * factor)
    elif op == "$min":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                current = get_path(doc, concrete, default=None)
                if current is None or value < current:
                    set_path(doc, concrete, value)
    elif op == "$max":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                current = get_path(doc, concrete, default=None)
                if current is None or value > current:
                    set_path(doc, concrete, value)
    elif op == "$push":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if arr is None:
                    set_path(doc, concrete, [value])
                elif isinstance(arr, list):
                    arr.append(value)
                else:
                    raise UpdateError(f"$push on non-array at {concrete!r}")
    elif op == "$addToSet":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if arr is None:
                    set_path(doc, concrete, [value])
                elif isinstance(arr, list):
                    if value not in arr:
                        arr.append(value)
                else:
                    raise UpdateError(f"$addToSet on non-array at {concrete!r}")
    elif op == "$pull":
        for path, criterion in payload.items():
            for concrete in _expand(doc, path, array_filters, positional_matches):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list):
                    arr[:] = [e for e in arr if e != criterion]
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
            if not isinstance(ops, Mapping) or len(ops) != 1:
                raise UpdateError("$bit requires a single-op document per field")
            (bit_op,) = ops.keys()
            mask = ops[bit_op]
            if not isinstance(mask, int) or isinstance(mask, bool):
                raise UpdateError("$bit mask must be an integer")
            for concrete in _expand(doc, path, array_filters, positional_matches):
                current = get_path(doc, concrete, default=0) or 0
                if not isinstance(current, int) or isinstance(current, bool):
                    raise UpdateError(f"$bit on non-integer at {concrete!r}")
                if bit_op == "and":
                    set_path(doc, concrete, current & mask)
                elif bit_op == "or":
                    set_path(doc, concrete, current | mask)
                elif bit_op == "xor":
                    set_path(doc, concrete, current ^ mask)
                else:
                    raise UpdateError(f"$bit unsupported sub-op: {bit_op}")
    else:
        raise UpdateError(f"unsupported update operator: {op}")
