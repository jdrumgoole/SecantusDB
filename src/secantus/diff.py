"""Compute MongoDB-style ``updateDescription`` between a pre- and post-image.

The output mirrors what real ``mongod`` puts on a change-stream ``update``
event:

- ``updatedFields``: ``{dotted.path: new_value}`` for every leaf that
  changed or was added. Array elements that changed in place produce
  ``arr.<index>`` paths; nested sub-doc fields produce
  ``arr.<index>.<field>`` paths.
- ``removedFields``: ``[dotted.path, ...]`` for every leaf that
  disappeared.
- ``truncatedArrays``: always ``[]``. Measured against mongod 8.2.11
  (and 6.0.16, and 8.3.4): it is never emitted for any ordinary update,
  at any array size — popping one element off a 1000-element array
  sends the whole 999-element array rather than a truncation.

**Arrays are reported by the OPERATION, not by diffing the values**, so
``update`` is threaded in. mongod reports:

- ``$push`` / ``$addToSet`` (no ``$slice`` / ``$sort`` / ``$position``)
  → ``arr.<i>`` for each appended index;
- ``$set`` / ``$unset`` of an indexed path → exactly that path (``$set:
  {"arr.7": 77}`` on a 5-element array reports ``arr.7`` **only**, not
  the nulls it creates at 5 and 6);
- everything else that touches an array — ``$pop``, ``$pull``,
  ``$pullAll``, ``$push`` with ``$slice`` / ``$sort``, and whole-field
  ``$set`` — → the **whole array**.

That last pair is the proof it cannot be done from values alone:
``$set: {arr: [1,2,3,4,5,6,7]}`` and ``$push: {arr: {$each: [6,7]}}``
produce an identical document, and mongod reports the first wholesale
and the second positionally. Without ``update`` (or for an operation
this does not recognise) arrays fall back to wholesale, which is what
mongod does for the majority of operators anyway.

Nested dicts walk element-wise so that changing one nested leaf
emits only that leaf rather than the whole sub-document.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from secantus.paths import get_path, set_path, unset_path

#: Operators whose effect on an array mongod reports element-wise, provided
#: they are a plain append (``$slice`` / ``$sort`` / ``$position`` reorder or
#: shrink the array, and mongod then sends the whole thing).
_APPEND_OPS = ("$push", "$addToSet")
_NON_APPEND_MODIFIERS = ("$slice", "$sort", "$position")
#: Operators that write ONE named path. An indexed path under any of them makes
#: that array element-wise -- checked against mongod 8.2.11 for `$set`,
#: `$unset`, `$inc`, `$mul`, `$min` and `$max`, which all report `arr.2`.
_PATH_WRITE_OPS = (
    "$set",
    "$unset",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$currentDate",
    "$bit",
)


def _elementwise_array_paths(
    update: Mapping[str, Any],
) -> dict[str, set[int] | None]:
    """Array paths this update touches in a way mongod reports element-wise.

    Maps each such path to the indices PAST THE OLD END that may be reported:
    ``None`` means "any" (an append knows every index it wrote), a set means
    "only these" -- ``$set: {"arr.7": 77}`` on a 5-element array reports
    ``arr.7`` alone, not the nulls it silently creates at 5 and 6.

    A path absent from this mapping falls back to wholesale replacement, which
    is what mongod does for ``$pop`` / ``$pull`` / ``$pullAll`` / sliced or
    sorted ``$push`` / whole-field ``$set``.
    """
    paths: dict[str, set[int] | None] = {}
    for op, spec in update.items():
        if not isinstance(spec, Mapping):
            continue
        if op in _APPEND_OPS:
            for field, value in spec.items():
                if isinstance(value, Mapping) and any(m in value for m in _NON_APPEND_MODIFIERS):
                    continue  # reorders or truncates -> whole array
                paths[str(field)] = None
        elif op in _PATH_WRITE_OPS:
            # An indexed path like ``arr.2`` or ``a.b.3.c`` makes the array
            # PREFIX element-wise; the array itself is never named.
            for field in spec:
                parts = str(field).split(".")
                for i, part in enumerate(parts):
                    if not (part.isdigit() and i):
                        continue
                    prefix = ".".join(parts[:i])
                    if prefix in paths and paths[prefix] is None:
                        continue  # an append already allowed every index
                    named = paths.setdefault(prefix, set())
                    assert named is not None
                    named.add(int(part))
    return paths


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
    elementwise: dict[str, set[int] | None] | None,
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
                    elementwise,
                )
        return
    if isinstance(pre, list) and isinstance(post, list):
        if pre == post:
            return
        # mongod reports an array by the OPERATION that changed it, not by
        # diffing the values -- see this module's docstring. Only an append or
        # an indexed write is element-wise; anything else (and anything we were
        # not given an update for) sends the whole array, which is what mongod
        # does for every other operator.
        if elementwise is None:
            # Pipeline update (or no update given): mongod diffs the VALUES
            # here, and this is the one shape where it really does emit
            # `truncatedArrays` -- `[{$set: {a: [...shorter...]}}]` reports a
            # truncation where the same `$set` as an OPERATOR resends the whole
            # array. Measured on 8.2.11; it is what pymongo's unified "Test
            # array truncation" spec asserts.
            for i in range(len(post)):
                child_path = f"{path}.{i}" if path else str(i)
                if i >= len(pre):
                    updated[child_path] = post[i]
                    _record_ambiguous(child_path, [*segments, i], disambiguated)
                    continue
                _walk(
                    pre[i],
                    post[i],
                    child_path,
                    updated,
                    removed,
                    truncated,
                    disambiguated,
                    [*segments, i],
                    elementwise,
                )
            if len(post) < len(pre):
                truncated.append({"field": path, "newSize": len(post)})
                _record_ambiguous(path, segments, disambiguated)
            return
        if path not in elementwise or len(post) < len(pre):
            updated[path] = post
            _record_ambiguous(path, segments, disambiguated)
            return
        for i in range(len(post)):
            child_path = f"{path}.{i}" if path else str(i)
            if i >= len(pre):
                # Past the old end. An append reports every index it wrote; an
                # indexed `$set` reports only the one it named.
                beyond = elementwise[path]
                if beyond is not None and i not in beyond:
                    continue
                updated[child_path] = post[i]
                _record_ambiguous(child_path, [*segments, i], disambiguated)
                continue
            _walk(
                pre[i],
                post[i],
                child_path,
                updated,
                removed,
                truncated,
                disambiguated,
                [*segments, i],
                elementwise,
            )
        return
    if pre != post:
        updated[path] = post
        _record_ambiguous(path, segments, disambiguated)


def compute_update_description(
    pre: Mapping[str, Any],
    post: Mapping[str, Any],
    update: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Return ``{updatedFields, removedFields, truncatedArrays}`` for ``pre`` -> ``post``.

    ``update`` is the update spec that produced ``post``. It is optional, and
    omitting it is safe -- arrays then fall back to wholesale replacement --
    but it is required to match mongod exactly, because mongod reports an array
    by the operation rather than by the values (module docstring).

    Both arguments must be document-like (``Mapping``). The ``_id`` field is
    intentionally compared like any other — change streams should not surface
    ``_id`` changes (mongod doesn't allow them) but if one slips through it
    will appear in ``updatedFields``.
    """
    updated: dict[str, Any] = {}
    removed: list[str] = []
    truncated: list[dict[str, Any]] = []
    disambiguated: dict[str, list[Any]] = {}
    _walk(
        dict(pre),
        dict(post),
        "",
        updated,
        removed,
        truncated,
        disambiguated,
        [],
        _elementwise_array_paths(update) if isinstance(update, Mapping) else None,
    )
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
