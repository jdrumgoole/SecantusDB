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


def get_path_values(doc: Any, path: str) -> tuple[list[Any], bool]:
    """Every value reachable at ``path``, descending into arrays.

    MongoDB's index-key generation walks a dotted path *through* an
    array: for ``{"prices": [{"owner": 1}, {"owner": 2}]}`` the path
    ``prices.owner`` yields ``[1, 2]``, and the index covering it is
    multikey. :func:`get_path` deliberately does not do this — it
    resolves one value and reads a numeric component as a positional
    index — which is right for ``$set`` / projection and wrong for
    index keys.

    Returns ``(values, descended)``. ``descended`` is True when any
    component was resolved by walking array elements; together with an
    array-valued leaf that is exactly the condition that makes an index
    multikey. A missing path yields an empty list (callers decide
    whether that means "null key" or "skip").
    """
    parts = path.split(".")
    current: list[Any] = [doc]
    descended = False
    for part in parts:
        nxt: list[Any] = []
        for cur in current:
            if isinstance(cur, dict):
                if part in cur:
                    nxt.append(cur[part])
            elif isinstance(cur, list):
                descended = True
                if part.isdigit():
                    idx = int(part)
                    if 0 <= idx < len(cur):
                        nxt.append(cur[idx])
                # mongod matches the component against each element too,
                # so ``a.0`` finds both the positional element and any
                # element carrying a literal ``"0"`` key.
                for elem in cur:
                    if isinstance(elem, dict) and part in elem:
                        nxt.append(elem[part])
        current = nxt
    return current, descended


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
