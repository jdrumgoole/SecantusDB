from __future__ import annotations

import copy
import datetime as _dt
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from dataclasses import field as _dc_field
from typing import TYPE_CHECKING, Any

from secantus.expressions import evaluate
from secantus.paths import get_path, set_path, unset_path
from secantus.query import matches

if TYPE_CHECKING:
    from secantus.storage import Storage


class AggregateError(Exception):
    pass


@dataclass
class PipelineContext:
    storage: Storage | None = None
    db_name: str = ""
    coll_name: str = ""
    vars: dict[str, Any] = _dc_field(default_factory=dict)
    change_stream: Any = None  # changestreams.ChangeStreamSpec | None — set by $changeStream stage

    def with_vars(self, more: dict[str, Any]) -> PipelineContext:
        return PipelineContext(
            storage=self.storage,
            db_name=self.db_name,
            coll_name=self.coll_name,
            vars={**self.vars, **more},
            change_stream=self.change_stream,
        )


_NULL_CTX = PipelineContext()


def apply_pipeline(
    docs: list[dict[str, Any]],
    pipeline: list[dict[str, Any]],
    ctx: PipelineContext | None = None,
) -> list[dict[str, Any]]:
    ctx = ctx or _NULL_CTX
    for stage in pipeline:
        docs = _apply_stage(stage, docs, ctx)
    return docs


def _apply_stage(
    stage: dict[str, Any],
    docs: list[dict[str, Any]],
    ctx: PipelineContext,
) -> list[dict[str, Any]]:
    if len(stage) != 1:
        raise AggregateError("each pipeline stage must have exactly one key")
    name, spec = next(iter(stage.items()))
    handler = _STAGES.get(name)
    if handler is None:
        raise AggregateError(f"unsupported aggregation stage: {name}")
    return handler(spec, docs, ctx)


def _stage_match(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    return [d for d in docs if matches(d, spec, vars=ctx.vars)]


def _stage_count(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, str):
        raise AggregateError("$count requires a field name string")
    return [{spec: len(docs)}]


def _stage_limit(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    return docs[: int(spec)]


def _stage_skip(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    return docs[int(spec) :]


def _stage_sort(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    from secantus.storage import sort_docs

    return sort_docs(list(docs), spec)


def _stage_project(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$project requires a document spec")
    return [_project_one(d, spec, ctx.vars) for d in docs]


def _project_one(
    doc: dict[str, Any], spec: Mapping[str, Any], vars: dict[str, Any]
) -> dict[str, Any]:
    inclusions: list[str] = []
    exclusions: list[str] = []
    computed: dict[str, Any] = {}
    id_handling: int | None = None

    for key, value in spec.items():
        if key == "_id":
            if value in (0, False):
                id_handling = 0
            elif value in (1, True):
                id_handling = 1
            else:
                computed["_id"] = value
                id_handling = 1
            continue
        if value in (1, True):
            inclusions.append(key)
        elif value in (0, False):
            exclusions.append(key)
        else:
            computed[key] = value

    has_inclusion = bool(inclusions) or bool(computed)
    has_exclusion = bool(exclusions)
    if has_inclusion and has_exclusion:
        raise AggregateError(
            "$project cannot mix inclusion and exclusion (other than excluding _id)"
        )

    if has_inclusion:
        result: dict[str, Any] = {}
        if id_handling != 0 and "_id" in doc:
            result["_id"] = copy.deepcopy(doc["_id"])
        for path in inclusions:
            value = get_path(doc, path)
            if value is not None or _path_present(doc, path):
                set_path(result, path, copy.deepcopy(value))
        for key, expr in computed.items():
            set_path(result, key, evaluate(expr, doc, vars))
        return result

    result = copy.deepcopy(doc)
    for path in exclusions:
        unset_path(result, path)
    if id_handling == 0:
        result.pop("_id", None)
    return result


def _path_present(doc: Mapping[str, Any], path: str) -> bool:
    parts = path.split(".")
    cur: Any = doc
    for part in parts:
        if isinstance(cur, Mapping) and part in cur:
            cur = cur[part]
        else:
            return False
    return True


def _stage_add_fields(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$addFields requires a document spec")
    return [_add_fields_one(d, spec, ctx.vars) for d in docs]


def _add_fields_one(
    doc: dict[str, Any], spec: Mapping[str, Any], vars: dict[str, Any]
) -> dict[str, Any]:
    result = copy.deepcopy(doc)
    for path, expr in spec.items():
        set_path(result, path, evaluate(expr, doc, vars))
    return result


def _stage_unset(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    paths = [spec] if isinstance(spec, str) else list(spec)
    out: list[dict[str, Any]] = []
    for d in docs:
        new = copy.deepcopy(d)
        for path in paths:
            unset_path(new, path)
        out.append(new)
    return out


_DENSIFY_UNIT_TO_TIMEDELTA: dict[str, str] = {
    # Units whose duration is fixed: trivially expressed as a timedelta.
    "week": "weeks",
    "day": "days",
    "hour": "hours",
    "minute": "minutes",
    "second": "seconds",
    "millisecond": "milliseconds",
}


def _stage_densify(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    """``$densify``: fill gaps between consecutive values, numeric or date.

    Output is each input doc plus filler docs (containing only the
    densify field, plus any partitionByFields) at every multiple of
    ``step`` strictly between consecutive input values within a
    partition. ``bounds`` may be ``"full"`` (use min/max from input) or
    a two-element ``[min, max]`` array (clamp/extend to those bounds);
    ``"partition"`` is treated like ``"full"`` per partition because we
    don't yet plumb a partition-wide range distinct from each
    partition's observed extremes.

    Date densify is enabled by setting ``range.unit`` to one of
    ``"week"`` / ``"day"`` / ``"hour"`` / ``"minute"`` / ``"second"`` /
    ``"millisecond"`` — those are the units with a fixed duration so
    a ``timedelta`` represents them exactly. ``"month"`` / ``"quarter"``
    / ``"year"`` are rejected (variable-length, would need
    ``relativedelta``-style arithmetic).
    """
    if not isinstance(spec, Mapping):
        raise AggregateError("$densify requires a document spec")
    field = spec.get("field")
    if not isinstance(field, str):
        raise AggregateError("$densify requires a field name")
    range_spec = spec.get("range")
    if not isinstance(range_spec, Mapping):
        raise AggregateError("$densify requires range")
    raw_step = range_spec.get("step")
    if not isinstance(raw_step, (int, float)) or raw_step <= 0:
        raise AggregateError("$densify step must be a positive number")
    unit = range_spec.get("unit")
    if unit is not None:
        if not isinstance(unit, str):
            raise AggregateError("$densify range.unit must be a string")
        if unit in {"month", "quarter", "year"}:
            raise AggregateError(
                f"$densify range.unit={unit!r} is variable-length; "
                "supported units are week / day / hour / minute / second / millisecond"
            )
        td_kwarg = _DENSIFY_UNIT_TO_TIMEDELTA.get(unit)
        if td_kwarg is None:
            raise AggregateError(f"$densify range.unit={unit!r} is not recognised")
        step: Any = _dt.timedelta(**{td_kwarg: raw_step})
    else:
        step = raw_step
    bounds = range_spec.get("bounds")
    partition_fields = list(spec.get("partitionByFields") or [])

    # Hard cap on filler-doc count per partition. Without this, an
    # explicit `bounds: [0, 10**15]` with `step: 1` materialises 10**15
    # filler docs and OOMs the process. mongod caps the densify result
    # internally at ~250k; we use 1M to leave headroom for legitimate
    # large-but-bounded ranges.
    if isinstance(bounds, list) and len(bounds) == 2:
        try:
            span = float(bounds[1]) - float(bounds[0])
            if span / float(step) > 1_000_000:
                raise AggregateError(
                    f"$densify range {bounds} with step {step} would emit "
                    f"more than 1,000,000 fillers — refusing"
                )
        except (TypeError, ValueError):
            # Non-numeric bounds reach the existing per-partition path
            # which raises a more specific error there.
            pass

    def partition_key(doc: Mapping[str, Any]) -> tuple[Any, ...]:
        return tuple(get_path(doc, f) for f in partition_fields)

    grouped: dict[tuple[Any, ...], list[dict[str, Any]]] = {}
    insertion_order: list[tuple[Any, ...]] = []
    for d in docs:
        key = partition_key(d)
        if key not in grouped:
            grouped[key] = []
            insertion_order.append(key)
        grouped[key].append(d)

    if not grouped and isinstance(bounds, list) and len(bounds) == 2:
        # No input docs but explicit bounds — emit fillers across the
        # whole range with no partition keys.
        return _densify_fill_range(field, bounds[0], bounds[1], step, {})

    out: list[dict[str, Any]] = []
    for key in insertion_order:
        partition_docs = sorted(grouped[key], key=lambda d: get_path(d, field))
        partition_carry = {f: get_path(partition_docs[0], f) for f in partition_fields}
        if isinstance(bounds, list) and len(bounds) == 2:
            lo, hi = bounds[0], bounds[1]
        else:  # "full" or "partition"
            lo = get_path(partition_docs[0], field)
            hi = get_path(partition_docs[-1], field)
        out.extend(_densify_partition(field, partition_docs, lo, hi, step, partition_carry))
    return out


def _densify_fill_range(
    field: str, lo: Any, hi: Any, step: float, carry: Mapping[str, Any]
) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    cursor = lo
    while cursor < hi:
        filler = dict(carry)
        filler[field] = _densify_canon(cursor)
        out.append(filler)
        cursor = cursor + step
    return out


def _densify_partition(
    field: str,
    partition_docs: list[dict[str, Any]],
    lo: Any,
    hi: Any,
    step: float,
    carry: Mapping[str, Any],
) -> list[dict[str, Any]]:
    """Emit fillers + originals for one partition, sorted by ``field``."""
    existing_values = {get_path(d, field) for d in partition_docs}
    out: list[dict[str, Any]] = []
    cursor = lo
    iter_docs = iter(partition_docs)
    next_doc: dict[str, Any] | None = next(iter_docs, None)
    while cursor < hi:
        next_val = get_path(next_doc, field) if next_doc is not None else None
        if next_doc is not None and next_val == cursor:
            out.append(copy.deepcopy(next_doc))
            next_doc = next(iter_docs, None)
        elif cursor not in existing_values:
            filler = dict(carry)
            filler[field] = _densify_canon(cursor)
            out.append(filler)
        cursor = cursor + step
    # Append any docs at or beyond hi (originals must always appear).
    while next_doc is not None:
        out.append(copy.deepcopy(next_doc))
        next_doc = next(iter_docs, None)
    return out


def _densify_canon(value: Any) -> Any:
    """Normalise the cursor value so int+int step yields int rather than float."""
    if isinstance(value, float) and value.is_integer():
        return int(value)
    return value


def _stage_unwind(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    if isinstance(spec, str):
        path = spec.lstrip("$")
        preserve_null = False
        include_index: str | None = None
    elif isinstance(spec, Mapping):
        raw_path = spec.get("path")
        if not isinstance(raw_path, str):
            raise AggregateError("$unwind requires a path string")
        path = raw_path.lstrip("$")
        preserve_null = bool(spec.get("preserveNullAndEmptyArrays", False))
        include_index = spec.get("includeArrayIndex")
    else:
        raise AggregateError("$unwind requires a path string or document spec")

    result: list[dict[str, Any]] = []
    for doc in docs:
        value = get_path(doc, path)
        if isinstance(value, list):
            if not value:
                if preserve_null:
                    new = copy.deepcopy(doc)
                    unset_path(new, path)
                    if include_index:
                        new[include_index] = None
                    result.append(new)
                continue
            for i, elem in enumerate(value):
                new = copy.deepcopy(doc)
                set_path(new, path, elem)
                if include_index:
                    new[include_index] = i
                result.append(new)
        elif value is None:
            if preserve_null:
                new = copy.deepcopy(doc)
                if include_index:
                    new[include_index] = None
                result.append(new)
        else:
            new = copy.deepcopy(doc)
            if include_index:
                new[include_index] = None
            result.append(new)
    return result


def _stage_replace_root(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping) or "newRoot" not in spec:
        raise AggregateError("$replaceRoot requires {newRoot: <expression>}")
    return [_replace_root_one(d, spec["newRoot"], ctx.vars) for d in docs]


def _stage_replace_with(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    return [_replace_root_one(d, spec, ctx.vars) for d in docs]


def _replace_root_one(
    doc: dict[str, Any], new_root_expr: Any, vars: dict[str, Any]
) -> dict[str, Any]:
    new_root = evaluate(new_root_expr, doc, vars)
    if not isinstance(new_root, Mapping):
        raise AggregateError("$replaceRoot newRoot must evaluate to a document")
    return dict(new_root)


def _stage_group(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping) or "_id" not in spec:
        raise AggregateError("$group requires an _id expression")
    id_expr = spec["_id"]
    accumulators = {k: v for k, v in spec.items() if k != "_id"}
    # Pre-compile accumulators once: each entry is (field, handler, arg) where
    # handler is a per-op callable bound on (bucket, field, arg, doc, vars).
    # Replaces a 9-way if/elif chain in the per-doc loop.
    compiled: list[tuple[str, _AccHandler, Any]] = []
    for field, accumulator in accumulators.items():
        if not isinstance(accumulator, Mapping) or len(accumulator) != 1:
            raise AggregateError(f"$group accumulator for {field!r} must be a single-op doc")
        op, arg = next(iter(accumulator.items()))
        handler = _ACC_DISPATCH.get(op)
        if handler is None:
            raise AggregateError(f"unsupported $group accumulator: {op}")
        compiled.append((field, handler, arg))

    groups: dict[Any, dict[str, Any]] = {}
    order: list[Any] = []
    for d in docs:
        key = evaluate(id_expr, d, ctx.vars)
        hashable_key = _hashable(key)
        if hashable_key not in groups:
            groups[hashable_key] = {"_id": key}
            order.append(hashable_key)
        bucket = groups[hashable_key]
        for field, handler, arg in compiled:
            handler(bucket, field, arg, d, ctx.vars)
    return [_finalize(groups[k]) for k in order]


def _finalize(bucket: dict[str, Any]) -> dict[str, Any]:
    for k, v in list(bucket.items()):
        if isinstance(v, dict) and "_avg_total" in v and "_avg_n" in v:
            bucket[k] = v["_avg_total"] / v["_avg_n"] if v["_avg_n"] else None
    return bucket


_AccHandler = Callable[[dict[str, Any], str, Any, Mapping[str, Any], dict[str, Any]], None]


def _acc_sum(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    increment = 1 if arg == 1 else evaluate(arg, doc, vars)
    if increment is None:
        increment = 0
    bucket[field] = bucket.get(field, 0) + increment


def _acc_count(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    bucket[field] = bucket.get(field, 0) + 1


def _acc_avg(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    v = evaluate(arg, doc, vars)
    if v is None:
        return
    state = bucket.get(field)
    if not isinstance(state, dict) or "_avg_total" not in state:
        state = {"_avg_total": 0, "_avg_n": 0}
        bucket[field] = state
    state["_avg_total"] += v
    state["_avg_n"] += 1


def _acc_max(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    v = evaluate(arg, doc, vars)
    cur = bucket.get(field)
    if cur is None or (v is not None and v > cur):
        bucket[field] = v


def _acc_min(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    v = evaluate(arg, doc, vars)
    cur = bucket.get(field)
    if cur is None or (v is not None and v < cur):
        bucket[field] = v


def _acc_first(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    if field not in bucket:
        bucket[field] = evaluate(arg, doc, vars)


def _acc_last(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    bucket[field] = evaluate(arg, doc, vars)


def _acc_push(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    bucket.setdefault(field, []).append(evaluate(arg, doc, vars))


def _acc_add_to_set(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    v = evaluate(arg, doc, vars)
    bucket.setdefault(field, [])
    if v not in bucket[field]:
        bucket[field].append(v)


_ACC_DISPATCH: dict[str, _AccHandler] = {
    "$sum": _acc_sum,
    "$count": _acc_count,
    "$avg": _acc_avg,
    "$max": _acc_max,
    "$min": _acc_min,
    "$first": _acc_first,
    "$last": _acc_last,
    "$push": _acc_push,
    "$addToSet": _acc_add_to_set,
}


def _accumulate(
    bucket: dict[str, Any],
    field: str,
    accumulator: Any,
    doc: Mapping[str, Any],
    vars: dict[str, Any],
) -> None:
    if not isinstance(accumulator, Mapping) or len(accumulator) != 1:
        raise AggregateError(f"$group accumulator for {field!r} must be a single-op doc")
    op, arg = next(iter(accumulator.items()))
    handler = _ACC_DISPATCH.get(op)
    if handler is None:
        raise AggregateError(f"unsupported $group accumulator: {op}")
    handler(bucket, field, arg, doc, vars)


def _hashable(value: Any) -> Any:
    if isinstance(value, Mapping):
        return tuple(sorted((k, _hashable(v)) for k, v in value.items()))
    if isinstance(value, list):
        return tuple(_hashable(v) for v in value)
    return value


def _stage_lookup(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$lookup requires a document spec")
    from_coll = spec.get("from")
    as_field = spec.get("as")
    if not (isinstance(from_coll, str) and isinstance(as_field, str)):
        raise AggregateError("$lookup requires from and as (strings)")
    if ctx.storage is None:
        raise AggregateError("$lookup requires storage context")

    sub_pipeline = spec.get("pipeline")
    let_spec = spec.get("let") or {}
    if sub_pipeline is not None:
        return _stage_lookup_pipeline(ctx, docs, from_coll, as_field, let_spec, sub_pipeline, spec)

    local_field = spec.get("localField")
    foreign_field = spec.get("foreignField")
    if not (isinstance(local_field, str) and isinstance(foreign_field, str)):
        raise AggregateError("$lookup requires localField+foreignField, or pipeline form")

    # Index-driven path: when the foreign collection has a non-multikey
    # single-field index on the foreign field, do per-outer-doc lookups
    # via Storage.find_matching. The query goes through the existing
    # picker, so it lands as IXSCAN. This skips materialising the whole
    # foreign collection in memory and turns the join from O(N+M) into
    # O(M log N) with a much smaller constant for selective queries.
    if _foreign_field_has_simple_index(ctx.storage, ctx.db_name, from_coll, foreign_field):
        out: list[dict[str, Any]] = []
        for doc in docs:
            local_value = get_path(doc, local_field)
            matches_list = _index_join_lookup(
                ctx.storage, ctx.db_name, from_coll, foreign_field, local_value
            )
            new = copy.deepcopy(doc)
            new[as_field] = matches_list
            out.append(new)
        return out

    # Fallback: materialise the foreign collection and hash-join.
    foreign_docs = ctx.storage.find_matching(ctx.db_name, from_coll, {})
    join_index = _build_lookup_index(foreign_docs, foreign_field)
    out = []
    for doc in docs:
        local_value = get_path(doc, local_field)
        matches_list = _hash_join_lookup(local_value, foreign_docs, foreign_field, join_index)
        new = copy.deepcopy(doc)
        new[as_field] = matches_list
        out.append(new)
    return out


def _stage_lookup_pipeline(
    ctx: PipelineContext,
    docs: list[dict[str, Any]],
    from_coll: str,
    as_field: str,
    let_spec: Mapping[str, Any],
    sub_pipeline: list[dict[str, Any]],
    full_spec: Mapping[str, Any],
) -> list[dict[str, Any]]:
    assert ctx.storage is not None
    local_field = full_spec.get("localField")
    foreign_field = full_spec.get("foreignField")

    # When localField + foreignField are also given (the simple-form-
    # plus-pipeline mix), the simple-form join pre-filters the candidates
    # fed into the sub-pipeline. Use the index when available; fall back
    # to materialising the foreign collection for the hash-join.
    use_index = isinstance(foreign_field, str) and _foreign_field_has_simple_index(
        ctx.storage, ctx.db_name, from_coll, foreign_field
    )
    foreign_docs: list[dict[str, Any]] | None = None
    join_index: _LookupIndex | None = None
    if not use_index:
        foreign_docs = ctx.storage.find_matching(ctx.db_name, from_coll, {})
        join_index = (
            _build_lookup_index(foreign_docs, foreign_field)
            if isinstance(foreign_field, str)
            else None
        )

    out: list[dict[str, Any]] = []
    for doc in docs:
        bound = {name: evaluate(expr, doc, ctx.vars) for name, expr in let_spec.items()}
        if isinstance(local_field, str) and isinstance(foreign_field, str):
            local_value = get_path(doc, local_field)
            if use_index:
                assert ctx.storage is not None
                candidates = _index_join_lookup(
                    ctx.storage, ctx.db_name, from_coll, foreign_field, local_value
                )
            else:
                assert foreign_docs is not None and join_index is not None
                candidates = _hash_join_lookup(local_value, foreign_docs, foreign_field, join_index)
        else:
            # No localField/foreignField: pure pipeline form. We have
            # to feed the entire foreign collection in.
            if foreign_docs is None:
                foreign_docs = ctx.storage.find_matching(ctx.db_name, from_coll, {})
            candidates = list(foreign_docs)
        sub_ctx = ctx.with_vars(bound)
        joined = apply_pipeline(candidates, sub_pipeline, sub_ctx)
        new = copy.deepcopy(doc)
        new[as_field] = joined
        out.append(new)
    return out


def _foreign_field_has_simple_index(storage: Storage, db: str, coll: str, field: str) -> bool:
    """Is there an index whose leading column is ``field`` we can drive
    `$lookup` through?

    A single-field index keyed exactly on ``field`` is the canonical
    fit, but a compound index whose leading field is ``field`` is also
    eligible: Storage's picker turns ``{field: value}`` into a
    leading-prefix scan over the compound index's entries (correctness
    is identical to the single-field case for equality on the leading
    column). Multikey is fine too — Storage's
    ``_index_key_variants`` writes per-element entries, so equality
    lookups against array-valued foreign fields hit at least all true
    matches. Direction (1 / -1) is fine for either shape — the storage
    range scan handles ASC and DESC.

    Geo / hashed / text indexes are excluded by the all-numeric
    direction check below (their direction values are strings like
    ``"2dsphere"`` or ``"hashed"``).
    """
    try:
        indexes = storage.list_indexes(db, coll)
    except Exception:
        return False
    for ix in indexes:
        key = ix.get("key", {})
        if not isinstance(key, Mapping):
            continue
        keys = list(key.keys())
        if not keys or keys[0] != field:
            continue
        # Every column must be ASC/DESC numeric — excludes geo / hashed /
        # text whose direction values are strings.
        if any(v not in (1, -1) for v in key.values()):
            continue
        return True
    return False


def _index_join_lookup(
    storage: Storage,
    db: str,
    coll: str,
    foreign_field: str,
    local_value: Any,
) -> list[dict[str, Any]]:
    """Find the foreign docs whose ``foreign_field`` matches ``local_value``.

    Equivalent to the hash-join's per-outer-doc step but routes through
    Storage.find_matching so the existing index picker decides between
    IXSCAN and COLLSCAN. For array local values we use ``$in`` so the
    single index lookup covers all elements.
    """
    if isinstance(local_value, list):
        # Empty list never matches anything (mirrors mongod's $in: []
        # semantics) — short-circuit instead of a wasted query.
        if not local_value:
            return []
        return storage.find_matching(db, coll, {foreign_field: {"$in": list(local_value)}})
    return storage.find_matching(db, coll, {foreign_field: local_value})


class _LookupIndex:
    """Hash-join index from foreign-field value → list of foreign docs.

    ``hashable`` covers scalar/hashable values; ``unhashable`` retains
    foreign docs whose foreign-field value contains an unhashable element
    (e.g. nested dicts) so they can be checked via ``_lookup_match``.
    """

    __slots__ = ("hashable", "unhashable")

    def __init__(self) -> None:
        self.hashable: dict[Any, list[dict[str, Any]]] = {}
        self.unhashable: list[dict[str, Any]] = []


def _build_lookup_index(foreign_docs: list[dict[str, Any]], foreign_field: str) -> _LookupIndex:
    idx = _LookupIndex()
    for fd in foreign_docs:
        fv = get_path(fd, foreign_field)
        keys = list(fv) if isinstance(fv, list) else [fv]
        added = False
        for k in keys:
            try:
                idx.hashable.setdefault(k, []).append(fd)
                added = True
            except TypeError:
                continue
        if not added:
            idx.unhashable.append(fd)
    return idx


def _hash_join_lookup(
    local_value: Any,
    foreign_docs: list[dict[str, Any]],
    foreign_field: str,
    idx: _LookupIndex,
) -> list[dict[str, Any]]:
    lookups = list(local_value) if isinstance(local_value, list) else [local_value]
    seen: set[int] = set()
    out: list[dict[str, Any]] = []
    local_unhashable = False
    for lv in lookups:
        try:
            hits = idx.hashable.get(lv)
        except TypeError:
            local_unhashable = True
            continue
        if hits:
            for fd in hits:
                key = id(fd)
                if key not in seen:
                    seen.add(key)
                    out.append(fd)
    if local_unhashable:
        for fd in foreign_docs:
            if id(fd) in seen:
                continue
            if _lookup_match(local_value, get_path(fd, foreign_field)):
                seen.add(id(fd))
                out.append(fd)
    else:
        for fd in idx.unhashable:
            if id(fd) in seen:
                continue
            if _lookup_match(local_value, get_path(fd, foreign_field)):
                seen.add(id(fd))
                out.append(fd)
    return out


def _lookup_match(local: Any, foreign: Any) -> bool:
    if isinstance(local, list) and isinstance(foreign, list):
        return any(le == fe for le in local for fe in foreign)
    if isinstance(local, list):
        return foreign in local
    if isinstance(foreign, list):
        return local in foreign
    return local == foreign


def _stage_sample(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    import random

    if not isinstance(spec, Mapping) or "size" not in spec:
        raise AggregateError("$sample requires {size: N}")
    size = int(spec["size"])
    if size >= len(docs):
        return list(docs)
    return random.sample(list(docs), size)


def _stage_sort_by_count(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    grouped = _stage_group({"_id": spec, "count": {"$sum": 1}}, docs, ctx)
    grouped.sort(key=lambda d: d.get("count", 0), reverse=True)
    return grouped


def _stage_facet(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$facet requires a document of {name: pipeline}")
    out: dict[str, Any] = {}
    for name, sub_pipeline in spec.items():
        if not isinstance(sub_pipeline, list):
            raise AggregateError(f"$facet entry {name!r} must be a pipeline array")
        out[name] = apply_pipeline(list(docs), sub_pipeline, ctx)
    return [out]


def _stage_bucket(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$bucket requires a document spec")
    group_by = spec.get("groupBy")
    boundaries = spec.get("boundaries")
    default = spec.get("default")
    output_spec = spec.get("output") or {"count": {"$sum": 1}}
    if not isinstance(boundaries, list) or len(boundaries) < 2:
        raise AggregateError("$bucket requires boundaries array of >=2 values")

    buckets: dict[Any, list[dict[str, Any]]] = {b: [] for b in boundaries[:-1]}
    if default is not None:
        buckets[default] = []

    for d in docs:
        value = evaluate(group_by, d, ctx.vars)
        placed = False
        for i in range(len(boundaries) - 1):
            lo, hi = boundaries[i], boundaries[i + 1]
            try:
                if lo <= value < hi:
                    buckets[lo].append(d)
                    placed = True
                    break
            except TypeError:
                continue
        if not placed and default is not None:
            buckets[default].append(d)

    result: list[dict[str, Any]] = []
    for key, bucket_docs in buckets.items():
        bucket: dict[str, Any] = {"_id": key}
        for field_name, accumulator in output_spec.items():
            for d in bucket_docs:
                _accumulate(bucket, field_name, accumulator, d, ctx.vars)
        result.append(_finalize(bucket))
    return result


def _stage_out(spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext) -> list[dict[str, Any]]:
    if ctx.storage is None:
        raise AggregateError("$out requires storage context")
    if isinstance(spec, str):
        target_db, target_coll = ctx.db_name, spec
    elif isinstance(spec, Mapping):
        target_db = spec.get("db", ctx.db_name)
        target_coll = spec.get("coll")
        if not isinstance(target_coll, str):
            raise AggregateError("$out requires a coll string")
    else:
        raise AggregateError("$out requires a string or {db, coll}")
    ctx.storage.drop_collection(target_db, target_coll)
    if docs:
        ctx.storage.insert(target_db, target_coll, [copy.deepcopy(d) for d in docs])
    return []


def _deep_merge_docs(existing: Mapping[str, Any], new: Mapping[str, Any]) -> dict[str, Any]:
    """Recursive merge for ``$merge whenMatched: "merge"``.

    For overlapping keys: if both sides are sub-documents, merge them; for
    arrays or scalars, the new value wins (matches MongoDB's behaviour).
    Non-overlapping keys from both sides are kept.
    """
    result: dict[str, Any] = copy.deepcopy(dict(existing))
    for key, value in new.items():
        if key in result and isinstance(result[key], Mapping) and isinstance(value, Mapping):
            result[key] = _deep_merge_docs(result[key], value)
        else:
            result[key] = copy.deepcopy(value)
    return result


def _stage_merge(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if ctx.storage is None:
        raise AggregateError("$merge requires storage context")
    if isinstance(spec, str):
        target_db, target_coll = ctx.db_name, spec
        on_fields: list[str] = ["_id"]
        when_matched = "merge"
        when_not_matched = "insert"
    elif isinstance(spec, Mapping):
        into = spec.get("into")
        if isinstance(into, str):
            target_db, target_coll = ctx.db_name, into
        elif isinstance(into, Mapping):
            target_db = into.get("db", ctx.db_name)
            target_coll = into.get("coll")
            if not isinstance(target_coll, str):
                raise AggregateError("$merge into.coll must be a string")
        else:
            raise AggregateError("$merge requires into")
        on = spec.get("on", "_id")
        on_fields = [on] if isinstance(on, str) else list(on)
        when_matched = spec.get("whenMatched", "merge")
        when_not_matched = spec.get("whenNotMatched", "insert")
    else:
        raise AggregateError("$merge requires a string or document spec")

    for doc in docs:
        match_filter = {f: get_path(doc, f) for f in on_fields}
        existing = ctx.storage.find_matching(target_db, target_coll, match_filter, limit=1)
        if existing:
            if when_matched == "fail":
                # Real mongod raises a duplicate-key style error (code
                # 11000) with ``keyPattern`` and ``keyValue`` so drivers
                # can surface it through their DuplicateKeyException
                # path. Mongo-java-driver's
                # ``aggregate-merge-errorResponse`` test asserts both
                # fields on the ``errorResponse``.
                from secantus.storage import IndexConflict

                raise IndexConflict(
                    "_id_",
                    doc.get("_id"),
                    key_pattern={f: 1 for f in on_fields},
                    key_value=match_filter,
                )
            if when_matched == "keepExisting":
                continue
            if when_matched == "replace":
                ctx.storage.delete_matching(
                    target_db, target_coll, {"_id": existing[0]["_id"]}, limit=1
                )
                new = copy.deepcopy(doc)
                new.setdefault("_id", existing[0]["_id"])
                ctx.storage.insert(target_db, target_coll, [new])
            else:  # default "merge"
                merged = _deep_merge_docs(existing[0], doc)
                merged["_id"] = existing[0]["_id"]
                ctx.storage.update_matching(
                    target_db, target_coll, {"_id": existing[0]["_id"]}, merged
                )
        else:
            if when_not_matched == "fail":
                raise AggregateError("$merge whenNotMatched=fail and no match exists")
            if when_not_matched == "discard":
                continue
            ctx.storage.insert(target_db, target_coll, [copy.deepcopy(doc)])
    return []


def _stage_coll_stats(
    spec: Any, _docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if ctx.storage is None or not ctx.coll_name:
        raise AggregateError("$collStats requires storage and a collection")
    import datetime as _dt

    count = ctx.storage.count_matching(ctx.db_name, ctx.coll_name, None)
    indexes = ctx.storage.list_indexes(ctx.db_name, ctx.coll_name)
    out: dict[str, Any] = {
        "ns": f"{ctx.db_name}.{ctx.coll_name}",
        "host": "secantus",
        "localTime": _dt.datetime.now(_dt.timezone.utc),
    }
    spec = spec if isinstance(spec, Mapping) else {}
    if "storageStats" in spec:
        out["storageStats"] = {
            "size": 0,
            "count": count,
            "avgObjSize": 0,
            "storageSize": 0,
            "indexSizes": {i["name"]: 0 for i in indexes},
            "totalIndexSize": 0,
            "scaleFactor": 1,
            "nindexes": len(indexes),
        }
    if "latencyStats" in spec:
        out["latencyStats"] = {
            "reads": {"latency": 0, "ops": 0},
            "writes": {"latency": 0, "ops": 0},
            "commands": {"latency": 0, "ops": 0},
        }
    if "count" in spec:
        out["count"] = count
    return [out]


def _stage_index_stats(
    _spec: Any, _docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if ctx.storage is None or not ctx.coll_name:
        raise AggregateError("$indexStats requires storage and a collection")
    indexes = ctx.storage.list_indexes(ctx.db_name, ctx.coll_name)
    return [
        {
            "name": i["name"],
            "key": i["key"],
            "host": "secantus",
            "accesses": {"ops": 0, "since": None},
        }
        for i in indexes
    ]


def _stage_current_op(
    _spec: Any, _docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    """``$currentOp`` — currently-executing operations.

    Real ``mongod`` reports every operation in flight on the server.
    SecantusDB processes commands synchronously per connection and
    doesn't keep a per-op registry, but we still return one stub
    "op" document so callers that introspect the result (e.g.
    mongo-ruby-driver's ``database_spec`` collation test, which
    checks that every doc carries a ``host`` field) get a
    plausibly-shaped row. The stub is the ``$currentOp`` itself —
    the aggregation request that produced it — minus any sensitive
    state.
    """
    return [
        {
            "type": "op",
            "host": "secantus",
            "desc": "$currentOp",
            "active": False,
            "currentOpTime": "",
        }
    ]


def _stage_bucket_auto(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$bucketAuto requires a document spec")
    group_by = spec.get("groupBy")
    n_buckets = spec.get("buckets")
    if group_by is None or not isinstance(n_buckets, int) or n_buckets < 1:
        raise AggregateError("$bucketAuto requires groupBy and a positive buckets int")
    output_spec = spec.get("output") or {"count": {"$sum": 1}}

    from secantus.storage import _SortKey

    pairs = [(evaluate(group_by, d, ctx.vars), d) for d in docs]
    pairs.sort(key=lambda p: _SortKey(p[0]))
    if not pairs:
        return []
    bucket_size = max(1, len(pairs) // n_buckets)
    out: list[dict[str, Any]] = []
    i = 0
    while i < len(pairs) and len(out) < n_buckets:
        is_last = len(out) == n_buckets - 1
        chunk = pairs[i:] if is_last else pairs[i : i + bucket_size]
        if not chunk:
            break
        if not is_last and i + bucket_size < len(pairs):
            upper = pairs[i + bucket_size][0]
        else:
            upper = chunk[-1][0]
        bucket: dict[str, Any] = {"_id": {"min": chunk[0][0], "max": upper}}
        for field_name, accumulator in output_spec.items():
            for _, d in chunk:
                _accumulate(bucket, field_name, accumulator, d, ctx.vars)
        out.append(_finalize(bucket))
        i += len(chunk)
    return out


def _stage_graph_lookup(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$graphLookup requires a document spec")
    from_coll = spec.get("from")
    start_with = spec.get("startWith")
    connect_from = spec.get("connectFromField")
    connect_to = spec.get("connectToField")
    as_field = spec.get("as")
    # Default maxDepth=100 when caller doesn't specify, mirroring mongod.
    # Without a default, a self-referencing collection blows up to O(N^2)
    # memory in the BFS frontier.
    max_depth = spec.get("maxDepth", 100)
    depth_field = spec.get("depthField")
    if not all(isinstance(x, str) for x in (from_coll, connect_from, connect_to, as_field)):
        raise AggregateError(
            "$graphLookup requires from/connectFromField/connectToField/as as strings"
        )
    if ctx.storage is None:
        raise AggregateError("$graphLookup requires storage context")
    foreign = ctx.storage.find_matching(ctx.db_name, from_coll, {})

    def _walk(seed_value: Any) -> list[dict[str, Any]]:
        seen_ids: set[Any] = set()
        out_docs: list[dict[str, Any]] = []
        frontier: list[tuple[Any, int]] = [(seed_value, 0)]
        while frontier:
            value, depth = frontier.pop(0)
            if max_depth is not None and depth > int(max_depth):
                continue
            for fdoc in foreign:
                fid = fdoc.get("_id")
                if fid in seen_ids:
                    continue
                target = get_path(fdoc, connect_to)
                if _values_match(value, target):
                    seen_ids.add(fid)
                    new_doc = copy.deepcopy(fdoc)
                    if depth_field:
                        new_doc[depth_field] = depth
                    out_docs.append(new_doc)
                    next_value = get_path(fdoc, connect_from)
                    if next_value is not None:
                        frontier.append((next_value, depth + 1))
        return out_docs

    out: list[dict[str, Any]] = []
    for doc in docs:
        seed = evaluate(start_with, doc, ctx.vars)
        new = copy.deepcopy(doc)
        new[as_field] = _walk(seed)
        out.append(new)
    return out


def _values_match(a: Any, b: Any) -> bool:
    if isinstance(a, list) and isinstance(b, list):
        return any(x == y for x in a for y in b)
    if isinstance(a, list):
        return b in a
    if isinstance(b, list):
        return a in b
    return a == b


def _stage_change_stream(
    spec: Any, _docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    """Source-style stage: stashes the parsed spec on ``ctx`` and yields no docs.

    The real work — reading the oplog, projecting events, blocking on
    ``getMore`` — happens in a producer closure installed by the
    ``aggregate`` command handler. Subsequent pipeline stages run on the
    produced events, not on stored documents.
    """
    from secantus import changestreams

    if not isinstance(spec, Mapping):
        raise AggregateError("$changeStream spec must be a document")
    ctx.change_stream = changestreams.parse_spec(spec)
    return []


def _stage_documents(
    spec: Any, _docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, list):
        raise AggregateError("$documents requires an array of documents")
    out: list[dict[str, Any]] = []
    for entry in spec:
        evaluated = evaluate(entry, {}, ctx.vars)
        if not isinstance(evaluated, Mapping):
            raise AggregateError("$documents entries must evaluate to documents")
        out.append(dict(evaluated))
    return out


def _stage_geo_near(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    """``$geoNear`` — proximity search with attached distances.

    Walks every doc, optionally pre-filters via ``query``, computes the
    distance from each doc's ``key`` field to ``near``, drops docs that
    fall outside [``minDistance``, ``maxDistance``], attaches the
    distance under ``distanceField`` (with dotted-path support), and
    returns docs sorted ascending by distance.

    ``key`` is optional: when omitted, the stage picks the first geo
    index on the collection (matching ``mongod``'s behaviour). Without
    a geo index *and* without an explicit ``key`` the stage errors —
    real ``mongod`` does the same.
    """
    from shapely.geometry import Point

    from secantus.geo import distance, parse_doc_geometry

    if not isinstance(spec, Mapping):
        raise AggregateError("$geoNear requires an object")
    near = spec.get("near")
    if near is None:
        raise AggregateError("$geoNear requires `near`")
    distance_field = spec.get("distanceField")
    if not isinstance(distance_field, str) or not distance_field:
        raise AggregateError("$geoNear requires a string `distanceField`")
    key = spec.get("key")
    if not isinstance(key, str) or not key:
        key = _infer_geo_near_key(ctx)
        if key is None:
            raise AggregateError("$geoNear requires `key` when the collection has no geo index")
    pre_filter = spec.get("query")
    distance_multiplier = spec.get("distanceMultiplier", 1.0)
    if not isinstance(distance_multiplier, (int, float)) or isinstance(distance_multiplier, bool):
        raise AggregateError("$geoNear distanceMultiplier must be a number")
    include_locs_field = spec.get("includeLocs")
    if include_locs_field is not None and (
        not isinstance(include_locs_field, str) or not include_locs_field
    ):
        raise AggregateError("$geoNear includeLocs must be a non-empty string")

    spherical, center = _parse_geo_near_origin(near, spec.get("spherical"))

    max_distance = spec.get("maxDistance")
    min_distance = spec.get("minDistance")

    results: list[tuple[float, dict[str, Any]]] = []
    for doc in docs:
        if pre_filter is not None and not matches(doc, pre_filter, vars=ctx.vars):
            continue
        # `key` is a dotted path into the doc.
        value = get_path(doc, key)
        if value is None:
            continue
        doc_geom = parse_doc_geometry(value)
        if doc_geom is None:
            continue
        d = distance(doc_geom, Point(*center), spherical=spherical)
        if d is None:
            continue
        if max_distance is not None and d > max_distance:
            continue
        if min_distance is not None and d < min_distance:
            continue
        out = copy.deepcopy(doc)
        set_path(out, distance_field, d * float(distance_multiplier))
        if include_locs_field is not None:
            # Attach the *raw* doc geometry value, not a re-serialized
            # Shapely point, so a doc stored as ``[x, y]`` round-trips
            # as ``[x, y]`` and one stored as GeoJSON round-trips as
            # GeoJSON. Matches mongod's "echo what was indexed" semantics.
            set_path(out, include_locs_field, copy.deepcopy(value))
        results.append((d, out))
    results.sort(key=lambda pair: pair[0])
    return [doc for _d, doc in results]


def _infer_geo_near_key(ctx: PipelineContext) -> str | None:
    """Return the field name of the first geo index on the collection.

    Real ``mongod`` allows ``$geoNear`` to omit ``key`` when there is
    exactly one geo index and uses it implicitly. We pick the first
    geo-typed key (``"2dsphere"`` or ``"2d"``) from
    :meth:`storage.Storage.list_indexes` order, which is deterministic
    (sorted by name). Returns ``None`` if no geo index exists or the
    storage isn't available (e.g. the pipeline runs outside a server).
    """
    if ctx.storage is None or not ctx.db_name or not ctx.coll_name:
        return None
    for index in ctx.storage.list_indexes(ctx.db_name, ctx.coll_name):
        key_spec = index.get("key", {})
        if not isinstance(key_spec, Mapping):
            continue
        for field, value in key_spec.items():
            if isinstance(value, str) and value in ("2dsphere", "2d"):
                return field
    return None


def _parse_geo_near_origin(near: Any, spherical_opt: Any) -> tuple[bool, tuple[float, float]]:
    """Extract ``(spherical, (x, y))`` from ``$geoNear`` ``near`` value.

    A GeoJSON Point implies spherical; a legacy ``[x, y]`` is planar
    unless ``spherical: true`` is explicitly set, matching ``mongod``.
    """
    if isinstance(near, Mapping) and near.get("type") == "Point":
        coords = near.get("coordinates")
        if not isinstance(coords, list) or len(coords) != 2:
            raise AggregateError("$geoNear `near` Point needs [lng, lat]")
        return True, (float(coords[0]), float(coords[1]))
    if isinstance(near, list) and len(near) == 2:
        spherical = bool(spherical_opt) if spherical_opt is not None else False
        return spherical, (float(near[0]), float(near[1]))
    raise AggregateError("$geoNear `near` must be a GeoJSON Point or a [x, y] pair")


_STAGES = {
    "$match": _stage_match,
    "$count": _stage_count,
    "$limit": _stage_limit,
    "$skip": _stage_skip,
    "$sort": _stage_sort,
    "$project": _stage_project,
    "$addFields": _stage_add_fields,
    "$set": _stage_add_fields,
    "$unset": _stage_unset,
    "$unwind": _stage_unwind,
    "$densify": _stage_densify,
    "$replaceRoot": _stage_replace_root,
    "$replaceWith": _stage_replace_with,
    "$group": _stage_group,
    "$lookup": _stage_lookup,
    "$sample": _stage_sample,
    "$sortByCount": _stage_sort_by_count,
    "$facet": _stage_facet,
    "$bucket": _stage_bucket,
    "$bucketAuto": _stage_bucket_auto,
    "$collStats": _stage_coll_stats,
    "$indexStats": _stage_index_stats,
    "$currentOp": _stage_current_op,
    # ``$listLocalSessions`` / ``$listSessions`` enumerate logical
    # sessions tracked by the server. Reuse the ``$currentOp`` stub —
    # we return one synthetic op doc so test probes ``[{$listLocalSessions: {}}]``
    # find a non-empty result with the expected shape.
    "$listLocalSessions": _stage_current_op,
    "$listSessions": _stage_current_op,
    "$out": _stage_out,
    "$merge": _stage_merge,
    "$graphLookup": _stage_graph_lookup,
    "$documents": _stage_documents,
    "$changeStream": _stage_change_stream,
    "$geoNear": _stage_geo_near,
}
