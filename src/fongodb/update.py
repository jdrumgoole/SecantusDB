from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import Any


class UpdateError(Exception):
    pass


def apply_update(doc: dict[str, Any], update: Mapping[str, Any]) -> dict[str, Any]:
    if not update:
        return copy.deepcopy(doc)
    keys = list(update.keys())
    has_op = any(k.startswith("$") for k in keys)
    if has_op:
        if not all(k.startswith("$") for k in keys):
            raise UpdateError("update document cannot mix operators with replacement fields")
        result = copy.deepcopy(doc)
        for op, payload in update.items():
            _apply_op(result, op, payload)
        return result
    new = copy.deepcopy(dict(update))
    if "_id" in doc:
        if "_id" in new and new["_id"] != doc["_id"]:
            raise UpdateError("cannot change the _id of a document")
        new["_id"] = doc["_id"]
    return new


def _apply_op(doc: dict[str, Any], op: str, payload: Mapping[str, Any]) -> None:
    if op == "$set":
        for path, value in payload.items():
            _set_path(doc, path, value)
    elif op == "$unset":
        for path in payload:
            _unset_path(doc, path)
    elif op == "$inc":
        for path, delta in payload.items():
            current = _get_path(doc, path, default=0)
            if current is None:
                current = 0
            _set_path(doc, path, current + delta)
    elif op == "$mul":
        for path, factor in payload.items():
            current = _get_path(doc, path, default=0)
            if current is None:
                current = 0
            _set_path(doc, path, current * factor)
    elif op == "$min":
        for path, value in payload.items():
            current = _get_path(doc, path, default=None)
            if current is None or value < current:
                _set_path(doc, path, value)
    elif op == "$max":
        for path, value in payload.items():
            current = _get_path(doc, path, default=None)
            if current is None or value > current:
                _set_path(doc, path, value)
    elif op == "$push":
        for path, value in payload.items():
            arr = _get_path(doc, path, default=None)
            if arr is None:
                _set_path(doc, path, [value])
            elif isinstance(arr, list):
                arr.append(value)
            else:
                raise UpdateError(f"$push on non-array at {path!r}")
    elif op == "$addToSet":
        for path, value in payload.items():
            arr = _get_path(doc, path, default=None)
            if arr is None:
                _set_path(doc, path, [value])
            elif isinstance(arr, list):
                if value not in arr:
                    arr.append(value)
            else:
                raise UpdateError(f"$addToSet on non-array at {path!r}")
    elif op == "$pull":
        for path, criterion in payload.items():
            arr = _get_path(doc, path, default=None)
            if isinstance(arr, list):
                arr[:] = [e for e in arr if e != criterion]
    elif op == "$pop":
        for path, direction in payload.items():
            arr = _get_path(doc, path, default=None)
            if isinstance(arr, list) and arr:
                if direction == 1:
                    arr.pop()
                elif direction == -1:
                    arr.pop(0)
    elif op == "$rename":
        for old, new in payload.items():
            if _has_path(doc, old):
                value = _get_path(doc, old)
                _unset_path(doc, old)
                _set_path(doc, new, value)
    else:
        raise UpdateError(f"unsupported update operator: {op}")


def _walk_to_parent(doc: dict[str, Any], path: str, *, create: bool) -> tuple[Any, str | None]:
    parts = path.split(".")
    cur: Any = doc
    for part in parts[:-1]:
        if isinstance(cur, dict):
            if part not in cur:
                if not create:
                    return None, None
                cur[part] = {}
            cur = cur[part]
        elif isinstance(cur, list):
            if not part.isdigit():
                return None, None
            idx = int(part)
            if 0 <= idx < len(cur):
                cur = cur[idx]
            else:
                return None, None
        else:
            return None, None
    return cur, parts[-1]


def _set_path(doc: dict[str, Any], path: str, value: Any) -> None:
    parent, leaf = _walk_to_parent(doc, path, create=True)
    if parent is None or leaf is None:
        return
    if isinstance(parent, dict):
        parent[leaf] = value
    elif isinstance(parent, list) and leaf.isdigit():
        idx = int(leaf)
        while len(parent) <= idx:
            parent.append(None)
        parent[idx] = value


def _unset_path(doc: dict[str, Any], path: str) -> None:
    parent, leaf = _walk_to_parent(doc, path, create=False)
    if parent is None or leaf is None:
        return
    if isinstance(parent, dict):
        parent.pop(leaf, None)
    elif isinstance(parent, list) and leaf.isdigit():
        idx = int(leaf)
        if 0 <= idx < len(parent):
            parent[idx] = None


def _get_path(doc: dict[str, Any], path: str, default: Any = None) -> Any:
    parent, leaf = _walk_to_parent(doc, path, create=False)
    if parent is None or leaf is None:
        return default
    if isinstance(parent, dict):
        return parent.get(leaf, default)
    if isinstance(parent, list) and leaf.isdigit():
        idx = int(leaf)
        if 0 <= idx < len(parent):
            return parent[idx]
    return default


def _has_path(doc: dict[str, Any], path: str) -> bool:
    parent, leaf = _walk_to_parent(doc, path, create=False)
    if parent is None or leaf is None:
        return False
    if isinstance(parent, dict):
        return leaf in parent
    if isinstance(parent, list) and leaf.isdigit():
        idx = int(leaf)
        return 0 <= idx < len(parent)
    return False
