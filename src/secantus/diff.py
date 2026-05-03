"""Compute MongoDB-style ``updateDescription`` between a pre- and post-image.

The output mirrors what real ``mongod`` puts on a change-stream ``update``
event:

- ``updatedFields``: ``{dotted.path: new_value}`` for every leaf that changed
  or was added.
- ``removedFields``: ``[dotted.path, ...]`` for every leaf that disappeared.
- ``truncatedArrays``: ``[{field: dotted.path, newSize: N}, ...]`` when the
  post array is a strict head-prefix of the pre array (leading elements
  match, post is shorter). Otherwise the array is replaced wholesale via
  ``updatedFields``.

Nested dicts are walked element-wise so that changing one nested leaf
emits only that leaf rather than the whole sub-document. Arrays whose
contents shifted (insertion / deletion in the middle) are replaced
wholesale, matching mongod's $v:2 diff semantics in the simple case.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any


def _is_truncation(pre: list[Any], post: list[Any]) -> bool:
    """True if ``post`` is a strict head-prefix of ``pre`` (and shorter)."""
    if len(post) >= len(pre):
        return False
    return pre[: len(post)] == post


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
        if _is_truncation(pre, post):
            truncated.append({"field": path, "newSize": len(post)})
            return
        # Fall back to wholesale replacement; preserve the original semantics
        # of "this array changed" so consumers can re-fetch.
        updated[path] = post
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
