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
) -> dict[str, Any]:
    if isinstance(update, list):
        return _apply_pipeline_update(doc, update)
    if not update:
        return copy.deepcopy(doc)
    keys = list(update.keys())
    has_op = any(k.startswith("$") for k in keys)
    filter_map = _index_array_filters(array_filters or [])
    if has_op:
        if not all(k.startswith("$") for k in keys):
            raise UpdateError("update document cannot mix operators with replacement fields")
        result = copy.deepcopy(doc)
        for op, payload in update.items():
            if op == "$setOnInsert" and not is_upsert:
                continue
            _apply_op(result, op, payload, filter_map)
        return result
    new = copy.deepcopy(dict(update))
    if "_id" in doc:
        if "_id" in new and new["_id"] != doc["_id"]:
            raise UpdateError("cannot change the _id of a document")
        new["_id"] = doc["_id"]
    return new


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
) -> list[str]:
    parts = path.split(".")
    if not any(_is_positional_token(p) for p in parts):
        return [path]
    out: list[str] = []
    _walk_positional(doc, parts, [], out, array_filters)
    return out


def _is_positional_token(part: str) -> bool:
    return part == "$[]" or (part.startswith("$[") and part.endswith("]"))


def _walk_positional(
    cur: Any,
    remaining: list[str],
    prefix: list[str],
    out: list[str],
    array_filters: dict[str, Mapping[str, Any]],
) -> None:
    if not remaining:
        out.append(".".join(prefix))
        return
    head, *rest = remaining
    if head == "$[]":
        if not isinstance(cur, list):
            return
        for i, elem in enumerate(cur):
            _walk_positional(elem, rest, prefix + [str(i)], out, array_filters)
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
                _walk_positional(elem, rest, prefix + [str(i)], out, array_filters)
        return
    if isinstance(cur, Mapping):
        _walk_positional(cur.get(head), rest, prefix + [head], out, array_filters)
    elif isinstance(cur, list) and head.isdigit():
        idx = int(head)
        if 0 <= idx < len(cur):
            _walk_positional(cur[idx], rest, prefix + [head], out, array_filters)


_PIPELINE_UPDATE_STAGES = {
    "$set",
    "$addFields",
    "$unset",
    "$project",
    "$replaceRoot",
    "$replaceWith",
}


def _apply_pipeline_update(
    doc: dict[str, Any], pipeline: list[Mapping[str, Any]]
) -> dict[str, Any]:
    for stage in pipeline:
        if not isinstance(stage, Mapping) or len(stage) != 1:
            raise UpdateError("each pipeline stage must be a single-key document")
        (name,) = stage.keys()
        if name not in _PIPELINE_UPDATE_STAGES:
            raise UpdateError(f"stage {name} not allowed in pipeline updates")
    from secantus.aggregate import apply_pipeline

    result = apply_pipeline([doc], list(pipeline))
    if not result:
        return copy.deepcopy(doc)
    new = result[0]
    if "_id" in doc and new.get("_id") != doc.get("_id"):
        raise UpdateError("pipeline update cannot change the _id of a document")
    return new


def _expand(
    doc: dict[str, Any], path: str, array_filters: dict[str, Mapping[str, Any]]
) -> list[str]:
    return _expand_path(doc, path, array_filters)


def _apply_op(
    doc: dict[str, Any],
    op: str,
    payload: Mapping[str, Any],
    array_filters: dict[str, Mapping[str, Any]],
) -> None:
    if op == "$set" or op == "$setOnInsert":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters):
                set_path(doc, concrete, value)
    elif op == "$unset":
        for path in payload:
            for concrete in _expand(doc, path, array_filters):
                unset_path(doc, concrete)
    elif op == "$currentDate":
        for path, opts in payload.items():
            for concrete in _expand(doc, path, array_filters):
                if opts is True:
                    set_path(doc, concrete, _dt.datetime.now(_dt.UTC))
                    continue
                if isinstance(opts, Mapping):
                    kind = opts.get("$type")
                    if kind == "date":
                        set_path(doc, concrete, _dt.datetime.now(_dt.UTC))
                        continue
                    if kind == "timestamp":
                        import time as _time

                        import bson as _bson

                        set_path(doc, concrete, _bson.Timestamp(int(_time.time()), 0))
                        continue
                raise UpdateError(f"$currentDate option for {path!r} not understood")
    elif op == "$inc":
        for path, delta in payload.items():
            for concrete in _expand(doc, path, array_filters):
                current = get_path(doc, concrete, default=0)
                if current is None:
                    current = 0
                set_path(doc, concrete, current + delta)
    elif op == "$mul":
        for path, factor in payload.items():
            for concrete in _expand(doc, path, array_filters):
                current = get_path(doc, concrete, default=0)
                if current is None:
                    current = 0
                set_path(doc, concrete, current * factor)
    elif op == "$min":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters):
                current = get_path(doc, concrete, default=None)
                if current is None or value < current:
                    set_path(doc, concrete, value)
    elif op == "$max":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters):
                current = get_path(doc, concrete, default=None)
                if current is None or value > current:
                    set_path(doc, concrete, value)
    elif op == "$push":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters):
                arr = get_path(doc, concrete, default=None)
                if arr is None:
                    set_path(doc, concrete, [value])
                elif isinstance(arr, list):
                    arr.append(value)
                else:
                    raise UpdateError(f"$push on non-array at {concrete!r}")
    elif op == "$addToSet":
        for path, value in payload.items():
            for concrete in _expand(doc, path, array_filters):
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
            for concrete in _expand(doc, path, array_filters):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list):
                    arr[:] = [e for e in arr if e != criterion]
    elif op == "$pop":
        for path, direction in payload.items():
            for concrete in _expand(doc, path, array_filters):
                arr = get_path(doc, concrete, default=None)
                if isinstance(arr, list) and arr:
                    if direction == 1:
                        arr.pop()
                    elif direction == -1:
                        arr.pop(0)
    elif op == "$rename":
        for old, new in payload.items():
            if has_path(doc, old):
                value = get_path(doc, old)
                unset_path(doc, old)
                set_path(doc, new, value)
    else:
        raise UpdateError(f"unsupported update operator: {op}")
