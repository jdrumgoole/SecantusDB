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

from secantus.paths import get_path, set_path, unset_path


def _record_ambiguous(path: str, segments: list[Any], disambiguated: dict[str, list[Any]]) -> None:
    """mongod 6.1+ ``disambiguatedPaths``: any reported path containing a
    numeric-string FIELD name (a dict key like ``"1"`` that a reader
    could mistake for an array index) maps to its typed segment list —
    ints for real array indices, strings for field names."""
    if any(isinstance(s, str) and s.isdigit() for s in segments):
        disambiguated[path] = list(segments)


def _walk(
    pre: Any,
    post: Any,
    path: str,
    updated: dict[str, Any],
    removed: list[str],
    truncated: list[dict[str, Any]],
    disambiguated: dict[str, list[Any]],
    segments: list[Any],
) -> None:
    """Recursive walk; mutates the output collections in place."""
    if isinstance(pre, Mapping) and isinstance(post, Mapping):
        pre_keys = set(pre.keys())
        post_keys = set(post.keys())
        for key in sorted(pre_keys | post_keys):
            child_path = f"{path}.{key}" if path else key
            child_segments = [*segments, key]
            if key not in post_keys:
                removed.append(child_path)
                _record_ambiguous(child_path, child_segments, disambiguated)
            elif key not in pre_keys:
                updated[child_path] = post[key]
                _record_ambiguous(child_path, child_segments, disambiguated)
            else:
                _walk(
                    pre[key],
                    post[key],
                    child_path,
                    updated,
                    removed,
                    truncated,
                    disambiguated,
                    child_segments,
                )
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
            _record_ambiguous(path, segments, disambiguated)
            return
        # ``len(post) <= len(pre)``: walk the kept prefix pairwise so
        # changes to individual elements surface as ``arr.<i>`` updates
        # (and, for kept elements that are sub-docs, as
        # ``arr.<i>.<field>`` updates via recursion).
        for i in range(len(post)):
            child_path = f"{path}.{i}" if path else str(i)
            _walk(
                pre[i],
                post[i],
                child_path,
                updated,
                removed,
                truncated,
                disambiguated,
                [*segments, i],
            )
        if len(post) < len(pre):
            truncated.append({"field": path, "newSize": len(post)})
            _record_ambiguous(path, segments, disambiguated)
        return
    if pre != post:
        updated[path] = post
        _record_ambiguous(path, segments, disambiguated)


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
    disambiguated: dict[str, list[Any]] = {}
    _walk(dict(pre), dict(post), "", updated, removed, truncated, disambiguated, [])
    out: dict[str, Any] = {
        "updatedFields": updated,
        "removedFields": removed,
        "truncatedArrays": truncated,
    }
    if disambiguated:
        # Only stamped when an ambiguous path exists (the unified specs
        # use $$unsetOrMatches — absence is valid when unambiguous).
        out["disambiguatedPaths"] = disambiguated
    return out


def apply_update_description(doc: dict[str, Any], diff: Mapping[str, Any]) -> dict[str, Any]:
    """Apply a ``$v: 2`` ``updateDescription`` to ``doc`` in place; return ``doc``.

    This is the inverse of :func:`compute_update_description`: given the
    pre-image ``doc`` and the ``{updatedFields, removedFields, truncatedArrays}``
    payload stored under an oplog update's ``o.diff``, it reconstructs the
    post-image. Used by oplog replay (point-in-time recovery) to roll a
    document forward without re-running the original update operators.

    ``disambiguatedPaths`` is intentionally not consulted: every path is
    applied against the real pre-image, so the runtime type of each parent
    container (``dict`` vs ``list``) already resolves the numeric-key vs
    array-index ambiguity that field exists to flag for a blind reader. The
    dotted-path helpers key off that container type, matching how the original
    update wrote the value.

    Order matters: ``updatedFields`` (which only ever target indices below an
    array's new length) are written first, then ``removedFields`` are unset,
    then ``truncatedArrays`` shorten any arrays last.
    """
    for path, value in (diff.get("updatedFields") or {}).items():
        set_path(doc, path, value)
    for path in diff.get("removedFields") or []:
        unset_path(doc, path)
    for entry in diff.get("truncatedArrays") or []:
        arr = get_path(doc, entry["field"])
        new_size = entry["newSize"]
        if isinstance(arr, list) and new_size < len(arr):
            del arr[new_size:]
    return doc
