from __future__ import annotations

from typing import Any

# Hard cap on numeric indices in dotted paths that would grow a list.
# `{$set: {"arr.99999999": "x"}}` would otherwise allocate ~10**8 None
# entries (~800 MB on CPython). Real BSON docs are 16 MB, so any path
# index above this cap is by definition wrong.
_MAX_LIST_GROW_INDEX = 100_000


class PathError(ValueError):
    """Raised when a dotted-path operation would exceed safety bounds."""


def walk_to_parent(doc: dict[str, Any], path: str, *, create: bool) -> tuple[Any, str | None]:
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


def get_path(doc: dict[str, Any], path: str, default: Any = None) -> Any:
    parent, leaf = walk_to_parent(doc, path, create=False)
    if parent is None or leaf is None:
        return default
    if isinstance(parent, dict):
        return parent.get(leaf, default)
    if isinstance(parent, list) and leaf.isdigit():
        idx = int(leaf)
        if 0 <= idx < len(parent):
            return parent[idx]
    return default


def set_path(doc: dict[str, Any], path: str, value: Any) -> None:
    parent, leaf = walk_to_parent(doc, path, create=True)
    if parent is None or leaf is None:
        return
    if isinstance(parent, dict):
        parent[leaf] = value
    elif isinstance(parent, list) and leaf.isdigit():
        idx = int(leaf)
        if idx > _MAX_LIST_GROW_INDEX:
            raise PathError(
                f"set_path index {idx} exceeds the {_MAX_LIST_GROW_INDEX}-element "
                f"list-growth cap (path={path!r})"
            )
        while len(parent) <= idx:
            parent.append(None)
        parent[idx] = value


def unset_path(doc: dict[str, Any], path: str) -> None:
    parent, leaf = walk_to_parent(doc, path, create=False)
    if parent is None or leaf is None:
        return
    if isinstance(parent, dict):
        parent.pop(leaf, None)
    elif isinstance(parent, list) and leaf.isdigit():
        idx = int(leaf)
        if 0 <= idx < len(parent):
            parent[idx] = None


def has_path(doc: dict[str, Any], path: str) -> bool:
    parent, leaf = walk_to_parent(doc, path, create=False)
    if parent is None or leaf is None:
        return False
    if isinstance(parent, dict):
        return leaf in parent
    if isinstance(parent, list) and leaf.isdigit():
        idx = int(leaf)
        return 0 <= idx < len(parent)
    return False
