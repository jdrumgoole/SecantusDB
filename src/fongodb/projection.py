from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import Any

from fongodb.paths import get_path, has_path, set_path, unset_path


class ProjectionError(Exception):
    pass


_MISSING = object()


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
        for path in non_id:
            value = get_path(doc, path, default=_MISSING)
            if value is not _MISSING:
                set_path(result, path, copy.deepcopy(value))
        return result

    result = copy.deepcopy(doc)
    for path in non_id:
        if has_path(result, path):
            unset_path(result, path)
    if spec.get("_id") == 0:
        result.pop("_id", None)
    return result


def _detect_inclusion(spec: Mapping[str, Any]) -> bool:
    truthy = [bool(v) for v in spec.values()]
    if all(truthy):
        return True
    if not any(truthy):
        return False
    raise ProjectionError(
        "projection cannot mix inclusion and exclusion (other than excluding _id)"
    )
