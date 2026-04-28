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
) -> dict[str, Any]:
    if isinstance(update, list):
        return _apply_pipeline_update(doc, update)
    if not update:
        return copy.deepcopy(doc)
    keys = list(update.keys())
    has_op = any(k.startswith("$") for k in keys)
    if has_op:
        if not all(k.startswith("$") for k in keys):
            raise UpdateError("update document cannot mix operators with replacement fields")
        result = copy.deepcopy(doc)
        for op, payload in update.items():
            if op == "$setOnInsert" and not is_upsert:
                continue
            _apply_op(result, op, payload)
        return result
    new = copy.deepcopy(dict(update))
    if "_id" in doc:
        if "_id" in new and new["_id"] != doc["_id"]:
            raise UpdateError("cannot change the _id of a document")
        new["_id"] = doc["_id"]
    return new


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


def _apply_op(doc: dict[str, Any], op: str, payload: Mapping[str, Any]) -> None:
    if op == "$set" or op == "$setOnInsert":
        for path, value in payload.items():
            set_path(doc, path, value)
    elif op == "$unset":
        for path in payload:
            unset_path(doc, path)
    elif op == "$currentDate":
        for path, opts in payload.items():
            if opts is True:
                set_path(doc, path, _dt.datetime.now(_dt.UTC))
                continue
            if isinstance(opts, Mapping):
                kind = opts.get("$type")
                if kind == "date":
                    set_path(doc, path, _dt.datetime.now(_dt.UTC))
                    continue
                if kind == "timestamp":
                    import bson as _bson
                    import time as _time

                    set_path(doc, path, _bson.Timestamp(int(_time.time()), 0))
                    continue
            raise UpdateError(f"$currentDate option for {path!r} not understood")
    elif op == "$inc":
        for path, delta in payload.items():
            current = get_path(doc, path, default=0)
            if current is None:
                current = 0
            set_path(doc, path, current + delta)
    elif op == "$mul":
        for path, factor in payload.items():
            current = get_path(doc, path, default=0)
            if current is None:
                current = 0
            set_path(doc, path, current * factor)
    elif op == "$min":
        for path, value in payload.items():
            current = get_path(doc, path, default=None)
            if current is None or value < current:
                set_path(doc, path, value)
    elif op == "$max":
        for path, value in payload.items():
            current = get_path(doc, path, default=None)
            if current is None or value > current:
                set_path(doc, path, value)
    elif op == "$push":
        for path, value in payload.items():
            arr = get_path(doc, path, default=None)
            if arr is None:
                set_path(doc, path, [value])
            elif isinstance(arr, list):
                arr.append(value)
            else:
                raise UpdateError(f"$push on non-array at {path!r}")
    elif op == "$addToSet":
        for path, value in payload.items():
            arr = get_path(doc, path, default=None)
            if arr is None:
                set_path(doc, path, [value])
            elif isinstance(arr, list):
                if value not in arr:
                    arr.append(value)
            else:
                raise UpdateError(f"$addToSet on non-array at {path!r}")
    elif op == "$pull":
        for path, criterion in payload.items():
            arr = get_path(doc, path, default=None)
            if isinstance(arr, list):
                arr[:] = [e for e in arr if e != criterion]
    elif op == "$pop":
        for path, direction in payload.items():
            arr = get_path(doc, path, default=None)
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
