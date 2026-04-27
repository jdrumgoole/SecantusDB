from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import Any

from fongodb.paths import get_path, has_path, set_path, unset_path
from fongodb.query import matches

_MISSING = object()


class ProjectionError(Exception):
    pass


def _is_elem_match_spec(value: Any) -> bool:
    return isinstance(value, Mapping) and len(value) == 1 and "$elemMatch" in value


def apply_projection(doc: dict[str, Any], spec: Mapping[str, Any] | None) -> dict[str, Any]:
    if not spec:
        return copy.deepcopy(doc)

    non_id = {k: v for k, v in spec.items() if k != "_id"}
    if not non_id:
        result = copy.deepcopy(doc)
        if spec.get("_id") == 0:
            result.pop("_id", None)
        return result

    inclusion_mode = _detect_inclusion(non_id)

    if inclusion_mode:
        result: dict[str, Any] = {}
        if spec.get("_id", 1) and "_id" in doc:
            result["_id"] = copy.deepcopy(doc["_id"])
        for path, value in non_id.items():
            if _is_elem_match_spec(value):
                first = _first_match(doc, path, value["$elemMatch"])
                if first is not _MISSING:
                    set_path(result, path, [copy.deepcopy(first)])
                continue
            extracted = get_path(doc, path, default=_MISSING)
            if extracted is not _MISSING:
                set_path(result, path, copy.deepcopy(extracted))
        return result

    result = copy.deepcopy(doc)
    for path in non_id:
        if has_path(result, path):
            unset_path(result, path)
    if spec.get("_id") == 0:
        result.pop("_id", None)
    return result


def _first_match(doc: dict[str, Any], path: str, sub_filter: Mapping[str, Any]) -> Any:
    arr = get_path(doc, path)
    if not isinstance(arr, list):
        return _MISSING
    for elem in arr:
        if isinstance(elem, Mapping):
            if matches(elem, sub_filter):
                return elem
        elif matches({"_": elem}, {"_": sub_filter}):
            return elem
    return _MISSING


def _detect_inclusion(spec: Mapping[str, Any]) -> bool:
    truthy: list[bool] = []
    for v in spec.values():
        if _is_elem_match_spec(v):
            truthy.append(True)
        else:
            truthy.append(bool(v))
    if all(truthy):
        return True
    if not any(truthy):
        return False
    raise ProjectionError(
        "projection cannot mix inclusion and exclusion (other than excluding _id)"
    )
