from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import Any

from secantus.paths import get_path, has_path, set_path, unset_path
from secantus.query import matches

_MISSING = object()


class ProjectionError(Exception):
    pass


def _is_elem_match_spec(value: Any) -> bool:
    return isinstance(value, Mapping) and len(value) == 1 and "$elemMatch" in value


def _is_slice_spec(value: Any) -> bool:
    return isinstance(value, Mapping) and len(value) == 1 and "$slice" in value


def _apply_slice(arr: Any, slice_arg: Any) -> Any:
    """Apply a ``$slice`` projection operator argument to an array value.

    Argument forms (per mongod):
      * ``n`` (positive int) — first ``n`` elements
      * ``-n`` (negative int) — last ``n`` elements
      * ``[skip, limit]`` — skip then take limit (limit may be negative
        to take from the end of the skipped suffix)

    Non-array values pass through unchanged (mongod is lenient here).
    """
    if not isinstance(arr, list):
        return arr
    if isinstance(slice_arg, (int, float)) and not isinstance(slice_arg, bool):
        n = int(slice_arg)
        if n >= 0:
            return arr[:n]
        return arr[n:]
    if isinstance(slice_arg, (list, tuple)) and len(slice_arg) == 2:
        raw_skip, raw_limit = slice_arg
        try:
            skip = int(raw_skip)
            limit = int(raw_limit)
        except (TypeError, ValueError):
            return arr
        if skip < 0:
            skip = max(len(arr) + skip, 0)
        tail = arr[skip:]
        if limit >= 0:
            return tail[:limit]
        return tail[limit:]
    return arr


def apply_projection(doc: dict[str, Any], spec: Mapping[str, Any] | None) -> dict[str, Any]:
    if not spec:
        return copy.deepcopy(doc)

    # Separate ``$slice`` projections — they don't participate in
    # inclusion / exclusion mode detection (mongod treats them as
    # "neutral" modifiers that re-shape the value at a path). Apply
    # them after the inclusion/exclusion pass on the result doc.
    slice_specs: dict[str, Any] = {}
    spec_main: dict[str, Any] = {}
    for k, v in spec.items():
        if _is_slice_spec(v):
            slice_specs[k] = v["$slice"]
        else:
            spec_main[k] = v

    non_id = {k: v for k, v in spec_main.items() if k != "_id"}
    if not non_id:
        result = copy.deepcopy(doc)
        if spec_main.get("_id") == 0:
            result.pop("_id", None)
        for path, slice_arg in slice_specs.items():
            current = get_path(result, path, default=_MISSING)
            if current is not _MISSING:
                set_path(result, path, _apply_slice(current, slice_arg))
        return result

    inclusion_mode = _detect_inclusion(non_id)

    if inclusion_mode:
        result: dict[str, Any] = {}
        if spec_main.get("_id", 1) and "_id" in doc:
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
        # $slice on a path also implicitly INCLUDES the path in
        # inclusion mode — pull the value out of the source doc when
        # the path wasn't already in the inclusion set.
        for path, slice_arg in slice_specs.items():
            if not has_path(result, path):
                extracted = get_path(doc, path, default=_MISSING)
                if extracted is not _MISSING:
                    set_path(result, path, copy.deepcopy(extracted))
            current = get_path(result, path, default=_MISSING)
            if current is not _MISSING:
                set_path(result, path, _apply_slice(current, slice_arg))
        return result

    result = copy.deepcopy(doc)
    for path in non_id:
        if has_path(result, path):
            unset_path(result, path)
    if spec_main.get("_id") == 0:
        result.pop("_id", None)
    for path, slice_arg in slice_specs.items():
        current = get_path(result, path, default=_MISSING)
        if current is not _MISSING:
            set_path(result, path, _apply_slice(current, slice_arg))
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
    # Match mongod's literal error text — mongo-node-driver's
    # ``server_errors.test.ts`` asserts on the exact string ("Cannot
    # do exclusion on field ... in inclusion projection").
    raise ProjectionError("Projection cannot have a mix of inclusion and exclusion.")
