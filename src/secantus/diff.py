"""Compute MongoDB-style ``updateDescription`` between a pre- and post-image.

The output mirrors what real ``mongod`` puts on a change-stream ``update``
event:

- ``updatedFields``: ``{dotted.path: new_value}`` for every leaf that
  changed or was added. Array elements that changed in place produce
  ``arr.<index>`` paths; nested sub-doc fields produce
  ``arr.<index>.<field>`` paths.
- ``removedFields``: ``[dotted.path, ...]`` for every leaf that
  disappeared.
- ``truncatedArrays``: ``[{field: dotted.path, newSize: N}, ...]``
  whenever an array got shorter (``len(post) < len(pre)``). The kept
  prefix is diffed pairwise, so changed elements inside a truncated
  array surface as indexed ``updatedFields`` alongside the
  ``truncatedArrays`` entry — matching mongod's ``$v: 2`` semantics.

Arrays whose post version is **longer** than pre still wholesale-
replace (mongod's $v:2 can encode appends via index updates, but our
simpler model treats "the array grew" as a wholesale change so
downstream consumers re-fetch). Same-length-with-changes arrays diff
pairwise.

Nested dicts walk element-wise so that changing one nested leaf
emits only that leaf rather than the whole sub-document.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any


def _walk(
    pre: Any,
    post: Any,
    path: str,
    updated: dict[str, Any],
    removed: list[str],
    truncated: list[dict[str, Any]],
) -> None:
    """Recursive walk; mutates ``updated`` / ``removed`` / ``truncated`` in place."""
    if isinstance(pre, Mapping) and isinstance(post, Mapping):
        pre_keys = set(pre.keys())
        post_keys = set(post.keys())
        for key in sorted(pre_keys | post_keys):
            child_path = f"{path}.{key}" if path else key
            if key not in post_keys:
                removed.append(child_path)
            elif key not in pre_keys:
                updated[child_path] = post[key]
            else:
                _walk(pre[key], post[key], child_path, updated, removed, truncated)
        return
    if isinstance(pre, list) and isinstance(post, list):
        if pre == post:
            return
        # If post is longer, we can't represent the change as indexed
        # updates without an "append" operator — wholesale-replace.
        # mongod's $v:2 supports indexed insertion, but the simpler
        # wholesale fallback is correct (just less compact).
        if len(post) > len(pre):
            updated[path] = post
            return
        # ``len(post) <= len(pre)``: walk the kept prefix pairwise so
        # changes to individual elements surface as ``arr.<i>`` updates
        # (and, for kept elements that are sub-docs, as
        # ``arr.<i>.<field>`` updates via recursion).
        for i in range(len(post)):
            child_path = f"{path}.{i}" if path else str(i)
            _walk(pre[i], post[i], child_path, updated, removed, truncated)
        if len(post) < len(pre):
            truncated.append({"field": path, "newSize": len(post)})
        return
    if pre != post:
        updated[path] = post


def compute_update_description(pre: Mapping[str, Any], post: Mapping[str, Any]) -> dict[str, Any]:
    """Return ``{updatedFields, removedFields, truncatedArrays}`` for ``pre`` -> ``post``.

    Both arguments must be document-like (``Mapping``). The ``_id`` field is
    intentionally compared like any other — change streams should not surface
    ``_id`` changes (mongod doesn't allow them) but if one slips through it
    will appear in ``updatedFields``.
    """
    updated: dict[str, Any] = {}
    removed: list[str] = []
    truncated: list[dict[str, Any]] = []
    _walk(dict(pre), dict(post), "", updated, removed, truncated)
    return {
        "updatedFields": updated,
        "removedFields": removed,
        "truncatedArrays": truncated,
    }
