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

import bson

from secantus.ordering import bson_same_stored_value
from secantus.paths import get_path, has_path, set_path, unset_path

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
                    # A positional token (``$``, ``$[]``, ``$[<id>]``) writes
                    # elements of the array before it, like a numeric index:
                    # mongod reports ``a.1``, not the whole array (measured
                    # 8.2.11, 2026-10-01). Which elements is only known after
                    # the query, so any index is allowed.
                    if i and part.startswith("$"):
                        paths[".".join(parts[:i])] = None
                        continue
                    if not (part.isdigit() and i):
                        continue
                    prefix = ".".join(parts[:i])
                    if prefix in paths and paths[prefix] is None:
                        continue  # an append already allowed every index
                    named = paths.setdefault(prefix, set())
                    assert named is not None
                    named.add(int(part))
    return paths


def _push_targets(update: Mapping[str, Any]) -> set[str]:
    """The fields a ``$push`` appends to."""
    spec = update.get("$push")
    return {str(k) for k in spec} if isinstance(spec, Mapping) else set()


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
    push_targets: frozenset[str] | set[str] = frozenset(),
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
                    push_targets,
                )
        return
    if isinstance(pre, list) and isinstance(post, list):
        # `bson_same_stored_value` recurses, so this fast path sees a signed
        # zero nested in the array. A bare `pre == post` did not, and
        # `[0.0]` -> `[-0.0]` returned here with an EMPTY `updatedFields`.
        if bson_same_stored_value(pre, post):
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
                    push_targets,
                )
            if len(post) < len(pre):
                truncated.append({"field": path, "newSize": len(post)})
                _record_ambiguous(path, segments, disambiguated)
            return
        # Not element-wise, it shrank, or a ``$push`` onto an EMPTY array:
        # mongod resends the array. ``$push`` onto ``[]`` is ``updatedFields:
        # {a: [x]}``, while ``$addToSet`` onto ``[]``, or a ``$set`` of an
        # index past its end, is ``{a.0: x}`` (measured 8.2.11, 2026-10-01).
        if path not in elementwise or len(post) < len(pre) or (not pre and path in push_targets):
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
                push_targets,
            )
        return
    # BSON equality, not Python's: `True == 1` is true in Python, so a field
    # changing from `true` to `1` produced NO oplog entry at all -- a change a
    # change-stream consumer never saw. The two are different BSON types.
    # `bson_same_stored_value`, not `bson_equal`: mongod's EQUALITY calls
    # `0.0` and `-0.0` the same value while its CHANGE detection does not, and
    # this is the change-detection side. With `bson_equal` here a `$set` of
    # `-0.0` over `0.0` produced an EMPTY `updatedFields` -- the consumer was
    # told the document was updated and never told which field (probed 8.2.11,
    # 2026-09-05).
    if not bson_same_stored_value(pre, post):
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
        _push_targets(update) if isinstance(update, Mapping) else set(),
    )
    if isinstance(update, Mapping):
        _report_whole_values(update, post, updated, removed, truncated, disambiguated)
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


def _report_whole_values(
    update: Mapping[str, Any],
    post: Mapping[str, Any],
    updated: dict[str, Any],
    removed: list[str],
    truncated: list[dict[str, Any]],
    disambiguated: dict[str, list[Any]],
) -> None:
    """mongod describes a modifier update by what each operator TOUCHED, not by
    diffing the documents: a ``$set`` of a field reports the field's whole new
    value, and so does a ``$rename`` target. So ``$set: {a: {x: 1, z: 3}}`` over
    ``a: {x: 1, y: 2}`` is ``updatedFields: {a: {x: 1, z: 3}}`` on mongod, where
    a value diff says ``{a.z: 3}`` plus ``removedFields: [a.y]``. A ``$rename``
    target is reported even when the value it receives equals the one it
    replaces; a ``$set`` that leaves its field unchanged is still no change.
    Measured against mongod 8.2.11, 2026-10-01
    (``tools/probes/update_description.py``).

    Applied after the value diff: every entry at or below such a path collapses
    into one entry for the path itself (appended last, as mongod orders it).
    Positional (``$``, ``$[]``, ``$[<id>]``) paths are left to the diff, which
    already reports the resolved element. Mutates the collections in place.
    """
    targets: list[tuple[str, bool]] = []
    for op, payload in update.items():
        if not isinstance(payload, Mapping):
            continue
        if op == "$set":
            for path in payload:
                if not any(p.startswith("$") for p in str(path).split(".")):
                    targets.append((str(path), False))
        elif op == "$rename":
            for to in payload.values():
                if isinstance(to, str):
                    targets.append((to, True))
    for path, always in targets:
        if not has_path(post, path):
            continue
        value = get_path(post, path)
        prefix = path + "."

        def below(p: str, path: str = path, prefix: str = prefix) -> bool:
            return p == path or p.startswith(prefix)

        touched = any(below(k) for k in updated)
        kept_removed = [r for r in removed if not below(r)]
        touched |= len(kept_removed) != len(removed)
        kept_truncated = [t for t in truncated if not below(str(t.get("field", "")))]
        touched |= len(kept_truncated) != len(truncated)
        if touched or always:
            removed[:] = kept_removed
            truncated[:] = kept_truncated
            keep = {k: v for k, v in updated.items() if not below(k)}
            updated.clear()
            updated.update(keep)
            updated[path] = value
            for k in [k for k in disambiguated if below(k)]:
                del disambiguated[k]
            _record_ambiguous(path, _typed_segments(post, path), disambiguated)


def _typed_segments(doc: Any, path: str) -> list[Any]:
    """``path``'s segments typed against ``doc``: an int where the parent is an
    array, the field name otherwise (``disambiguatedPaths``' shape)."""
    segments: list[Any] = []
    node = doc
    for part in path.split("."):
        if isinstance(node, list) and part.isdigit():
            segments.append(int(part))
            idx = int(part)
            node = node[idx] if idx < len(node) else None
        else:
            segments.append(part)
            node = node.get(part) if isinstance(node, Mapping) else None
    return segments


# --- pipeline updates: mongod's own diff ------------------------------------
#
# A PIPELINE update is not described by what operators touched (there are
# none): mongod diffs the two documents into its ``$v: 2`` oplog diff, and logs
# a full REPLACEMENT instead whenever that diff is not clearly smaller than the
# new document. Both halves are observable -- the event is ``replace`` rather
# than ``update``, and an ``update``'s ``updatedFields`` follows the diff's own
# choices. Measured against mongod 8.2.11, 2026-10-01
# (``tools/probes/update_description.py``, ``change_stream_fuzz.py``):
#
# * a diff is logged iff ``bsonsize(diff) + 15 < bsonsize(post)`` -- the 15 is
#   the oplog entry's own overhead;
# * fields are compared in lockstep while their names line up; once the order
#   diverges, the rest of ``post`` is INSERTED and the rest of ``pre`` that
#   ``post`` lacks is DELETED;
# * a document or array that changed gets a sub-diff only when the sub-diff is
#   smaller than the new value; otherwise the whole value is an update.
#
# A diff node is a dict: a document node ``{"kind": "doc", "d": [names],
# "u": [(name, v)], "i": [(name, v)], "s": [(name, node)]}``, an array node
# ``{"kind": "arr", "l": new_len | None, "e": [(index, ("u", v) | ("s", node))]}``.

#: mongod's per-entry overhead of a delta oplog entry (see above).
_DELTA_OPLOG_OVERHEAD = 15


def _bson_size(value: Any) -> int:
    # document header (4) + type byte (1) + empty name's NUL (1) + trailer (1)
    return len(bson.encode({"": value})) - 7


def _binary_equal(a: Any, b: Any) -> bool:
    return bson.encode({"": a}) == bson.encode({"": b})


def _serialize(node: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    if node["kind"] == "doc":
        if node["d"]:
            out["d"] = {f: False for f in node["d"]}
        if node["u"]:
            out["u"] = dict(node["u"])
        if node["i"]:
            out["i"] = dict(node["i"])
        for f, sub in node["s"]:
            out[f"s{f}"] = _serialize(sub)
        return out
    out["a"] = True
    if node["l"] is not None:
        out["l"] = node["l"]
    for i, (tag, v) in node["e"]:
        out[f"{tag}{i}"] = _serialize(v) if tag == "s" else v
    return out


def _node_size(node: dict[str, Any]) -> int:
    return _bson_size(_serialize(node))


def _changed_value(pre: Any, post: Any) -> tuple[str, Any]:
    """``("s", node)`` when a sub-diff is smaller than the new value, else
    ``("u", post)``."""
    sub = None
    if isinstance(pre, Mapping) and isinstance(post, Mapping):
        sub = _diff_doc(pre, post)
    elif isinstance(pre, list) and isinstance(post, list):
        sub = _diff_arr(pre, post)
    if sub is not None and _node_size(sub) < _bson_size(post):
        return ("s", sub)
    return ("u", post)


def _diff_doc(pre: Mapping[str, Any], post: Mapping[str, Any]) -> dict[str, Any] | None:
    node: dict[str, Any] = {"kind": "doc", "d": [], "u": [], "i": [], "s": []}
    pre_items = list(pre.items())
    post_items = list(post.items())
    i = 0
    while i < len(pre_items) and i < len(post_items) and pre_items[i][0] == post_items[i][0]:
        name, a = pre_items[i]
        b = post_items[i][1]
        if not _binary_equal(a, b):
            tag, v = _changed_value(a, b)
            node[tag].append((name, v))
        i += 1
    # Past the first name mismatch the order changed (or fields came and
    # went): everything left in `post` is inserted, and what `post` no longer
    # has is deleted.
    for name, _ in pre_items[i:]:
        if name not in post:
            node["d"].append(name)
    node["i"].extend(post_items[i:])
    if node["d"] or node["u"] or node["i"] or node["s"]:
        return node
    return None


def _diff_arr(pre: list[Any], post: list[Any]) -> dict[str, Any] | None:
    node: dict[str, Any] = {"kind": "arr", "l": None, "e": []}
    for i, b in enumerate(post):
        if i < len(pre):
            if _binary_equal(pre[i], b):
                continue
            node["e"].append((i, _changed_value(pre[i], b)))
        else:
            node["e"].append((i, ("u", b)))
    if len(post) < len(pre):
        node["l"] = len(post)
    if node["l"] is not None or node["e"]:
        return node
    return None


def _describe(
    node: dict[str, Any],
    prefix: str,
    segs: list[Any],
    updated: dict[str, Any],
    removed: list[str],
    truncated: list[dict[str, Any]],
    disambiguated: dict[str, list[Any]],
) -> None:
    def path(name: str) -> str:
        return f"{prefix}.{name}" if prefix else name

    if node["kind"] == "doc":
        for f in node["d"]:
            _record_ambiguous(path(f), [*segs, f], disambiguated)
            removed.append(path(f))
        for f, v in [*node["u"], *node["i"]]:
            _record_ambiguous(path(f), [*segs, f], disambiguated)
            updated[path(f)] = v
        for f, sub in node["s"]:
            _describe(sub, path(f), [*segs, f], updated, removed, truncated, disambiguated)
        return
    if node["l"] is not None:
        truncated.append({"field": prefix, "newSize": node["l"]})
    for i, (tag, v) in node["e"]:
        p = path(str(i))
        if tag == "u":
            _record_ambiguous(p, [*segs, i], disambiguated)
            updated[p] = v
        else:
            _describe(v, p, [*segs, i], updated, removed, truncated, disambiguated)


def pipeline_update_description(
    pre: Mapping[str, Any], post: Mapping[str, Any]
) -> dict[str, Any] | None:
    """What mongod logs for a PIPELINE update of ``pre`` into ``post``: ``None``
    when it logs a full replacement (the event is then ``replace``), else the
    ``updateDescription`` its delta yields. An unchanged document is an empty
    description, as for any no-op update."""
    updated: dict[str, Any] = {}
    removed: list[str] = []
    truncated: list[dict[str, Any]] = []
    disambiguated: dict[str, list[Any]] = {}
    node = _diff_doc(pre, post)
    if node is not None:
        if _node_size(node) + _DELTA_OPLOG_OVERHEAD >= _bson_size(dict(post)):
            return None
        _describe(node, "", [], updated, removed, truncated, disambiguated)
    out: dict[str, Any] = {
        "updatedFields": updated,
        "removedFields": removed,
        "truncatedArrays": truncated,
    }
    if disambiguated:
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
