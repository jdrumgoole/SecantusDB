from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import Any

from secantus.expressions import _bson_type_name
from secantus.paths import get_path, has_path, set_path
from secantus.query import matches

_MISSING = object()


class ProjectionError(Exception):
    """Projection-validation error. ``code``/``code_name`` default to the generic
    mapping (14 TypeMismatch) but raise sites may pin mongod's specific code (e.g.
    31254 / 31253 for an inclusion/exclusion mix)."""

    def __init__(
        self, message: str, *, code: int | None = None, code_name: str | None = None
    ) -> None:
        super().__init__(message)
        self.code = code
        self.code_name = code_name


def _is_elem_match_spec(value: Any) -> bool:
    return isinstance(value, Mapping) and len(value) == 1 and "$elemMatch" in value


# Recognized ``$meta`` keywords (mongod). An unrecognized argument is a
# Location17308; ``textScore`` without a ``$text`` query is a Location40218.
# Everything here is accepted at parse time; SecantusDB doesn't actually compute
# any of these metadata values, so the projected field is omitted (partial —
# graceful degradation — rather than a wrong value or a spurious error).
_META_KEYWORDS = frozenset(
    {
        "textScore",
        "indexKey",
        "recordId",
        "sortKey",
        "searchScore",
        "searchHighlights",
        "geoNearDistance",
        "geoNearPoint",
        "vectorSearchScore",
    }
)


def _is_meta_spec(value: Any) -> bool:
    return isinstance(value, Mapping) and len(value) == 1 and "$meta" in value


def _query_has_text(query: Mapping[str, Any] | None) -> bool:
    """Whether ``query`` carries a ``$text`` clause (top-level or nested inside a
    ``$and`` / ``$or`` / ``$nor`` array). mongod requires a ``$text`` predicate
    before a ``{$meta: "textScore"}`` projection is legal."""
    if not isinstance(query, Mapping):
        return False
    for key, val in query.items():
        if key == "$text":
            return True
        if (
            key in ("$and", "$or", "$nor")
            and isinstance(val, (list, tuple))
            and any(_query_has_text(clause) for clause in val)
        ):
            return True
    return False


def validate_meta_projection(
    spec: Mapping[str, Any] | None, query: Mapping[str, Any] | None = None
) -> None:
    """Raise ``ProjectionError`` for a faulty ``{$meta: ...}`` projection value.
    Oracle-pinned against mongod 6.0:
      * an unrecognized ``$meta`` argument => Location17308
        ``Unsupported argument to $meta: <arg>``
      * ``{$meta: "textScore"}`` without a ``$text`` query => Location40218
        ``query requires text score metadata, but it is not available``
    Recognized-but-unsupported args (``indexKey`` / ``recordId`` / ``sortKey`` /
    …) validate cleanly here; :func:`apply_projection` omits the field."""
    if not spec:
        return
    for value in spec.values():
        if not _is_meta_spec(value):
            continue
        arg = value["$meta"]
        if arg not in _META_KEYWORDS:
            raise ProjectionError(
                f"Unsupported argument to $meta: {arg}",
                code=17308,
                code_name="Location17308",
            )
        if arg == "textScore" and not _query_has_text(query):
            raise ProjectionError(
                "query requires text score metadata, but it is not available",
                code=40218,
                code_name="Location40218",
            )


def _is_positional_key(key: str) -> bool:
    """A positional-projection key ``arr.$`` (the projection ``$`` operator, not
    the update one). ``arr.$.field`` is not a projection form."""
    return key.endswith(".$")


def _positional_element_predicate(
    query: Mapping[str, Any] | None, array_path: str
) -> tuple[Mapping[str, Any] | None, Any]:
    """Build the per-element predicate the positional ``arr.$`` projects against,
    from the query's clauses on ``array_path``. Returns ``(doc_pred, value_pred)``
    where ``doc_pred`` is a sub-document match (``{sub: v}`` from ``arr.sub``
    clauses and from an ``arr: {$elemMatch: E}`` clause) and ``value_pred`` is a
    direct value/operator predicate (from ``arr: <value|ops>``). Returns
    ``(None, _MISSING)`` when the query has no clause on ``array_path`` — mongod
    errors ``Location51246`` in that case."""
    if not isinstance(query, Mapping):
        return None, _MISSING
    doc_pred: dict[str, Any] = {}
    value_pred: Any = _MISSING
    prefix = array_path + "."
    found = False
    for key, val in query.items():
        if key == array_path:
            found = True
            if isinstance(val, Mapping) and set(val.keys()) == {"$elemMatch"}:
                em = val["$elemMatch"]
                if isinstance(em, Mapping):
                    doc_pred.update(em)
            else:
                value_pred = val
        elif key.startswith(prefix):
            found = True
            doc_pred[key[len(prefix) :]] = val
    if not found:
        return None, _MISSING
    return doc_pred, value_pred


def validate_projection(
    spec: Mapping[str, Any] | None, query: Mapping[str, Any] | None = None
) -> None:
    """Raise ``ProjectionError`` for an invalid positional projection. mongod
    validates the projection at parse time — *before* matching — so these errors
    fire even when the query returns zero documents (whereas the per-doc
    :func:`apply_projection` only sees them once a document is projected)."""
    if not spec:
        return
    validate_meta_projection(spec, query)
    positional = [k for k in spec if _is_positional_key(k)]
    if not positional:
        return
    if len(positional) > 1:
        raise ProjectionError(
            "Cannot specify more than one positional projection per query.",
            code=31276,
            code_name="Location31276",
        )
    key = positional[0]
    if not spec[key]:
        raise ProjectionError(
            "positional projection cannot be used with exclusion",
            code=31395,
            code_name="Location31395",
        )
    doc_pred, _ = _positional_element_predicate(query, key[: -len(".$")])
    if doc_pred is None:
        raise ProjectionError(
            "positional operator '.$' couldn't find a matching element in the query",
            code=51246,
            code_name="Location51246",
        )


def _positional_first(arr: Any, doc_pred: Mapping[str, Any], value_pred: Any) -> Any:
    """First element of ``arr`` matching the positional predicate, or ``_MISSING``."""
    if not isinstance(arr, list):
        return _MISSING
    for elem in arr:
        if doc_pred and not (isinstance(elem, Mapping) and matches(elem, doc_pred)):
            continue
        if value_pred is not _MISSING and not matches({"_": elem}, {"_": value_pred}):
            continue
        return elem
    return _MISSING


def _is_slice_spec(value: Any) -> bool:
    return isinstance(value, Mapping) and len(value) == 1 and "$slice" in value


def _slice_type_name(v: Any) -> str:
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, int):
        return "int" if -(2**31) <= v < 2**31 else "long"
    if isinstance(v, float):
        return "double"
    if isinstance(v, str):
        return "string"
    if isinstance(v, Mapping):
        return "object"
    if isinstance(v, (list, tuple)):
        return "array"
    if v is None:
        return "null"
    return type(v).__name__


def _validate_slice_arg(slice_arg: Any) -> None:
    """mongod's projection ``$slice`` validation. Valid forms are a number
    (first / last n) or ``[skip, limit]`` with a positive numeric limit. Any
    other shape is evaluated as the aggregation ``$slice`` *expression*, which
    errors: a scalar or an array of fewer than 2 / more than 3 elements is
    ``Location28667`` (wrong argument count); a 2- or 3-element array whose first
    element isn't an array is ``Location28724``."""
    if isinstance(slice_arg, (int, float)) and not isinstance(slice_arg, bool):
        return
    if isinstance(slice_arg, (list, tuple)):
        n = len(slice_arg)
        if n == 2:
            skip, limit = slice_arg
            numeric = (
                isinstance(skip, (int, float))
                and not isinstance(skip, bool)
                and isinstance(limit, (int, float))
                and not isinstance(limit, bool)
            )
            if numeric and limit > 0:
                return
        if n < 2 or n > 3:
            raise ProjectionError(
                f"Expression $slice takes at least 2 arguments, and at most 3, but {n} "
                "were passed in.",
                code=28667,
                code_name="Location28667",
            )
        raise ProjectionError(
            "First argument to $slice must be an array, but is of type: "
            f"{_slice_type_name(slice_arg[0])}",
            code=28724,
            code_name="Location28724",
        )
    raise ProjectionError(
        "Expression $slice takes at least 2 arguments, and at most 3, but 1 were passed in.",
        code=28667,
        code_name="Location28667",
    )


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
    docs: list[dict[str, Any]],
    spec: Mapping[str, Any] | None,
    query: Mapping[str, Any] | None = None,
) -> list[dict[str, Any]]:
    """Project every doc in ``docs`` against ``spec`` in one shot.

    Every ``find`` result is projected; an empty spec is a no-op copy. ``query`` is
    the find filter, needed only to resolve a positional ``arr.$`` projection.
    """
    if not spec:
        return [copy.deepcopy(d) for d in docs]
    plan = compile_projection(spec, query)
    return [apply_projection_plan(d, plan) for d in docs]


class _ProjectionPlan:
    """A projection spec compiled once per cursor: meta validation done, spec
    partitioned (main / $slice / positional), inclusion mode detected, path
    tries built, positional predicate resolved. ``apply_projection_plan`` then
    does only the per-doc work — previously ALL of this re-ran per document
    (the projected-find hot spot from the 2026-08 micro-opt review)."""

    __slots__ = (
        "kind",
        "spec_main",
        "slice_specs",
        "include_id",
        "id_excluded",
        "elem_match_paths",
        "plain_tree",
        "exclude_tree",
        "array_path",
        "doc_pred",
        "value_pred",
        "non_id",
    )

    def __init__(self) -> None:
        self.kind = ""
        self.spec_main: dict[str, Any] = {}
        self.slice_specs: dict[str, Any] = {}
        self.include_id = True
        self.id_excluded = False
        self.elem_match_paths: set[str] = set()
        self.plain_tree: dict[str, Any] | None = None
        self.exclude_tree: dict[str, Any] | None = None
        self.array_path = ""
        self.doc_pred: Mapping[str, Any] | None = None
        self.value_pred: Any = None
        self.non_id: dict[str, Any] = {}


def apply_projection(
    doc: dict[str, Any],
    spec: Mapping[str, Any] | None,
    query: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    if not spec:
        return copy.deepcopy(doc)
    return apply_projection_plan(doc, compile_projection(spec, query))


def compile_projection(
    spec: Mapping[str, Any],
    query: Mapping[str, Any] | None = None,
) -> _ProjectionPlan:
    plan = _ProjectionPlan()

    # ``$meta`` projections validate at parse time (Location17308 for an unknown
    # argument, Location40218 for ``textScore`` without a ``$text`` query). A
    # ``$meta`` field is inclusion-mode in mongod, but SecantusDB doesn't compute
    # the metadata — so the field is *omitted* (partial, graceful degradation).
    # We drop the meta keys from the spec while remembering one was present: a
    # spec that was *only* ``$meta`` fields becomes an inclusion projection of no
    # fields (mongod result: just ``_id``, unless ``_id`` was excluded).
    meta_present = any(_is_meta_spec(v) for v in spec.values())
    if meta_present:
        validate_meta_projection(spec, query)
        spec = {k: v for k, v in spec.items() if not _is_meta_spec(v)}
        non_meta_non_id = any(k != "_id" for k in spec)
        if not non_meta_non_id:
            # Inclusion projection with no surviving field: keep only ``_id``
            # (dropped when the spec excludes it via ``_id: 0``).
            plan.kind = "meta_id_only"
            plan.include_id = bool(spec.get("_id", 1))
            return plan

    # Separate ``$slice`` and positional (``arr.$``) projections — they don't
    # participate in inclusion / exclusion mode detection (mongod treats them as
    # value re-shapers). Apply them after the inclusion/exclusion pass.
    slice_specs: dict[str, Any] = plan.slice_specs
    positional_specs: dict[str, Any] = {}
    spec_main: dict[str, Any] = plan.spec_main
    for k, v in spec.items():
        if _is_slice_spec(v):
            _validate_slice_arg(v["$slice"])
            slice_specs[k] = v["$slice"]
        elif _is_positional_key(k):
            positional_specs[k] = v
        else:
            if _is_elem_match_spec(v) and not isinstance(v["$elemMatch"], Mapping):
                # mongod: the $elemMatch projection argument must be an object.
                raise ProjectionError(
                    "elemMatch: Invalid argument, object required, but got "
                    f"{_bson_type_name(v['$elemMatch'])}",
                    code=31274,
                    code_name="Location31274",
                )
            spec_main[k] = v

    if positional_specs:
        if len(positional_specs) > 1:
            raise ProjectionError(
                "Cannot specify more than one positional projection per query.",
                code=31276,
                code_name="Location31276",
            )
        # A positional projection is inclusion-only; an exclusion value rejects.
        ((pos_key, pos_val),) = positional_specs.items()
        if not pos_val:
            raise ProjectionError(
                "positional projection cannot be used with exclusion",
                code=31395,
                code_name="Location31395",
            )
        array_path = pos_key[: -len(".$")]
        doc_pred, value_pred = _positional_element_predicate(query, array_path)
        if doc_pred is None:
            raise ProjectionError(
                "positional operator '.$' couldn't find a matching element in the query",
                code=51246,
                code_name="Location51246",
            )
        plan.kind = "positional"
        plan.array_path = array_path
        plan.doc_pred = doc_pred
        plan.value_pred = value_pred
        return plan

    non_id = {k: v for k, v in spec_main.items() if k != "_id"}
    plan.non_id = non_id
    if not non_id:
        # The spec is at most an ``_id`` entry plus ``$slice`` modifiers.
        # mongod's rules (oracle-pinned against a real mongod):
        #   * non-zero ``_id`` (incl. None and "") => INCLUSION: only
        #     ``_id`` plus any $slice'd fields survive;
        #   * numeric zero / False => whole doc minus ``_id``;
        #   * no ``_id`` key => whole doc ($slice applied in place).
        if "_id" in spec_main and spec_main["_id"] != 0:
            plan.kind = "id_only_inclusion"
        else:
            plan.kind = "whole_doc"
            plan.id_excluded = "_id" in spec_main
        return plan

    if _detect_inclusion(non_id):
        plan.kind = "inclusion"
        plan.include_id = bool(spec_main.get("_id", 1))
        plan.elem_match_paths = {p for p, v in non_id.items() if _is_elem_match_spec(v)}
        plain_paths = [p for p in non_id if p not in plan.elem_match_paths]
        plan.plain_tree = _spec_tree(plain_paths) if plain_paths else None
        return plan

    plan.kind = "exclusion"
    plan.id_excluded = spec_main.get("_id") == 0
    plan.exclude_tree = _spec_tree(list(non_id))
    return plan


def apply_projection_plan(doc: dict[str, Any], plan: _ProjectionPlan) -> dict[str, Any]:
    """The per-document half of a compiled projection — branch bodies verbatim
    from the pre-split ``apply_projection``."""
    slice_specs = plan.slice_specs
    if plan.kind == "meta_id_only":
        result: dict[str, Any] = {}
        if plan.include_id and "_id" in doc:
            result["_id"] = copy.deepcopy(doc["_id"])
        return result
    if plan.kind == "positional":
        assert plan.doc_pred is not None
        return _apply_positional(
            doc, plan.spec_main, slice_specs, plan.array_path, plan.doc_pred, plan.value_pred
        )
    if plan.kind == "id_only_inclusion":
        result = {}
        if "_id" in doc:
            result["_id"] = copy.deepcopy(doc["_id"])
        for path, slice_arg in slice_specs.items():
            current = get_path(doc, path, default=_MISSING)
            if current is not _MISSING:
                set_path(result, path, _apply_slice(copy.deepcopy(current), slice_arg))
        return result
    if plan.kind == "whole_doc":
        result = copy.deepcopy(doc)
        if plan.id_excluded:
            result.pop("_id", None)
        for path, slice_arg in slice_specs.items():
            current = get_path(result, path, default=_MISSING)
            if current is not _MISSING:
                set_path(result, path, _apply_slice(current, slice_arg))
        return result
    if plan.kind == "inclusion":
        result = {}
        if plan.include_id and "_id" in doc:
            result["_id"] = copy.deepcopy(doc["_id"])
        if plan.plain_tree is not None:
            projected = _include_doc(doc, plan.plain_tree)
            for k, v in projected.items():
                result[k] = v
        for path in plan.elem_match_paths:
            first = _first_match(doc, path, plan.non_id[path]["$elemMatch"])
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
    # exclusion
    result = copy.deepcopy(doc)
    assert plan.exclude_tree is not None
    _exclude_doc(result, plan.exclude_tree)
    if plan.id_excluded:
        result.pop("_id", None)
    for path, slice_arg in slice_specs.items():
        current = get_path(result, path, default=_MISSING)
        if current is not _MISSING:
            set_path(result, path, _apply_slice(current, slice_arg))
    return result


def _apply_positional(
    doc: dict[str, Any],
    spec_main: Mapping[str, Any],
    slice_specs: Mapping[str, Any],
    array_path: str,
    doc_pred: Mapping[str, Any],
    value_pred: Any,
) -> dict[str, Any]:
    """Inclusion projection carrying a positional ``arr.$``: include the other
    requested fields plus ``array_path: [first-matching-element]``."""
    result: dict[str, Any] = {}
    if spec_main.get("_id", 1) and "_id" in doc:
        result["_id"] = copy.deepcopy(doc["_id"])
    non_id = {k: v for k, v in spec_main.items() if k != "_id"}
    for field, v in non_id.items():
        # Positional forces inclusion mode; a companion exclusion is the same
        # mix mongod rejects (Location31254).
        if not (_is_elem_match_spec(v) or v):
            raise ProjectionError(
                f"Cannot do exclusion on field {field} in inclusion projection",
                code=31254,
                code_name="Location31254",
            )
    plain_paths = [p for p, v in non_id.items() if not _is_elem_match_spec(v)]
    if plain_paths:
        result.update(_include_doc(doc, _spec_tree(plain_paths)))
    for p, v in non_id.items():
        if _is_elem_match_spec(v):
            first = _first_match(doc, p, v["$elemMatch"])
            if first is not _MISSING:
                set_path(result, p, [copy.deepcopy(first)])
    first = _positional_first(get_path(doc, array_path), doc_pred, value_pred)
    if first is not _MISSING:
        set_path(result, array_path, [copy.deepcopy(first)])
    for path, slice_arg in slice_specs.items():
        current = get_path(result, path, default=_MISSING)
        if current is _MISSING:
            extracted = get_path(doc, path, default=_MISSING)
            if extracted is not _MISSING:
                set_path(result, path, copy.deepcopy(extracted))
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
    """Whether ``spec`` (already stripped of ``_id``) is an inclusion (vs
    exclusion) projection. mongod validates field-by-field in order: the first
    field sets the mode and a later field of the opposite mode is rejected with
    mongod's *specific* per-field error — ``Location31254`` (exclusion in an
    inclusion projection) or ``Location31253`` (inclusion in an exclusion
    projection), naming the offending field. Mirrors the Rust server's
    `projection_mix_error`; drivers' projection-error tests assert both the code
    and the exact wording. An ``$elemMatch`` field counts as inclusion (as before);
    an all-inclusion or empty spec is inclusion mode."""
    mode: bool | None = None
    for field, v in spec.items():
        incl = True if _is_elem_match_spec(v) else bool(v)
        if mode is None:
            mode = incl
        elif mode != incl:
            if mode:
                raise ProjectionError(
                    f"Cannot do exclusion on field {field} in inclusion projection",
                    code=31254,
                    code_name="Location31254",
                )
            raise ProjectionError(
                f"Cannot do inclusion on field {field} in exclusion projection",
                code=31253,
                code_name="Location31253",
            )
    return True if mode is None else mode
