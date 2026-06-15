from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import Any

from secantus.paths import get_path, has_path, set_path
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


def apply_projection_batch(
    docs: list[dict[str, Any]], spec: Mapping[str, Any] | None
) -> list[dict[str, Any]]:
    """Project every doc in ``docs`` against ``spec`` in one shot.

    Every ``find`` result is projected; an empty spec is a no-op copy.
    """
    if not spec:
        return [copy.deepcopy(d) for d in docs]
    return [apply_projection(d, spec) for d in docs]


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
        # The spec is at most an ``_id`` entry plus ``$slice`` modifiers.
        # mongod's rules (oracle-pinned against a real mongod):
        #   * non-zero ``_id`` (incl. None and "") => INCLUSION: only
        #     ``_id`` plus any $slice'd fields survive;
        #   * numeric zero / False => whole doc minus ``_id``;
        #   * no ``_id`` key => whole doc ($slice applied in place).
        if "_id" in spec_main and spec_main["_id"] != 0:
            result = {}
            if "_id" in doc:
                result["_id"] = copy.deepcopy(doc["_id"])
            for path, slice_arg in slice_specs.items():
                current = get_path(doc, path, default=_MISSING)
                if current is not _MISSING:
                    set_path(result, path, _apply_slice(copy.deepcopy(current), slice_arg))
            return result
        result = copy.deepcopy(doc)
        if "_id" in spec_main:
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
        elem_match_paths = {p for p, v in non_id.items() if _is_elem_match_spec(v)}
        plain_paths = [p for p in non_id if p not in elem_match_paths]
        if plain_paths:
            projected = _include_doc(doc, _spec_tree(plain_paths))
            for k, v in projected.items():
                result[k] = v
        for path in elem_match_paths:
            first = _first_match(doc, path, non_id[path]["$elemMatch"])
            if first is not _MISSING:
                set_path(result, path, [copy.deepcopy(first)])
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
    _exclude_doc(result, _spec_tree(list(non_id)))
    if spec_main.get("_id") == 0:
        result.pop("_id", None)
    for path, slice_arg in slice_specs.items():
        current = get_path(result, path, default=_MISSING)
        if current is not _MISSING:
            set_path(result, path, _apply_slice(current, slice_arg))
    return result


def _spec_tree(paths: list[str]) -> dict[str, Any]:
    """Dotted paths -> nested trie; a leaf is an empty dict."""
    tree: dict[str, Any] = {}
    for p in paths:
        node = tree
        for seg in p.split("."):
            node = node.setdefault(seg, {})
    return tree


def _include_doc(doc: Mapping[str, Any], tree: Mapping[str, Any]) -> dict[str, Any]:
    """Inclusion projection of ``doc`` against a path trie.

    mongod semantics (oracle-pinned): a trie leaf copies the whole
    value; an interior segment recurses into dicts (keeping the ``{}``
    skeleton when the leaf is absent), maps over array elements
    (documents project — possibly to ``{}`` — and scalar elements are
    dropped), and drops the field entirely when the value is a scalar.
    Numeric segments are field names, never array indexes.
    """
    out: dict[str, Any] = {}
    for key, subtree in tree.items():
        if key not in doc:
            continue
        val = doc[key]
        if not subtree:
            out[key] = copy.deepcopy(val)
            continue
        projected = _include_value(val, subtree)
        if projected is not _MISSING:
            out[key] = projected
    return out


def _include_value(val: Any, subtree: Mapping[str, Any]) -> Any:
    if isinstance(val, Mapping):
        return _include_doc(val, subtree)
    if isinstance(val, list):
        return [
            p
            for p in (
                _include_value(elem, subtree) for elem in val if isinstance(elem, (Mapping, list))
            )
            if p is not _MISSING
        ]
    return _MISSING


def _exclude_doc(doc: dict[str, Any], tree: Mapping[str, Any]) -> None:
    """Exclusion projection: unset trie leaves, recursing through dicts
    and mapping over array elements (non-document elements survive)."""
    for key, subtree in tree.items():
        if key not in doc:
            continue
        if not subtree:
            del doc[key]
        else:
            _exclude_value(doc[key], subtree)


def _exclude_value(val: Any, subtree: Mapping[str, Any]) -> None:
    if isinstance(val, dict):
        _exclude_doc(val, subtree)
    elif isinstance(val, list):
        for elem in val:
            _exclude_value(elem, subtree)


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
