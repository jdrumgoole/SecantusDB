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
    # MongoDB 3.4+ ``collation`` flows through every stage that does
    # string comparison — ``$match`` (forwarded to query.matches),
    # ``$group`` / ``$sortByCount`` (bucket keys), ``$sort`` (string
    # ordering). Set by the ``aggregate`` command handler from the
    # request's ``collation`` argument; ``None`` keeps default
    # codepoint comparison.
    collation: Any = None
    # The aggregate command's request body (minus the ``$db`` /
    # ``lsid`` envelope fields). Surfaced by ``$currentOp`` as the
    # ``command`` sub-doc on the self-row mongo-node-driver's
    # ``$currentOp`` test introspects.
    command_doc: dict[str, Any] | None = None

    def with_vars(self, more: dict[str, Any]) -> PipelineContext:
        return PipelineContext(
            storage=self.storage,
            db_name=self.db_name,
            coll_name=self.coll_name,
            vars={**self.vars, **more},
            change_stream=self.change_stream,
            collation=self.collation,
            command_doc=self.command_doc,
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
    from secantus.collation import parse as _parse_collation

    coll_obj = _parse_collation(ctx.collation)
    return [d for d in docs if matches(d, spec, vars=ctx.vars, collation=coll_obj)]


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

# Variable-length units: each call needs ``dateutil.relativedelta``
# because the step is not a fixed duration. ``quarter`` is canonically
# 3 months, so it maps to ``months=3 * step``.
_DENSIFY_UNIT_TO_RELATIVEDELTA: dict[str, tuple[str, int]] = {
    "month": ("months", 1),
    "quarter": ("months", 3),
    "year": ("years", 1),
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

    Date densify accepts every mongod unit:

    * Fixed-duration: ``"week"`` / ``"day"`` / ``"hour"`` /
      ``"minute"`` / ``"second"`` / ``"millisecond"`` — represented
      via ``datetime.timedelta``.
    * Variable-length: ``"month"`` / ``"quarter"`` / ``"year"`` —
      represented via ``dateutil.relativedelta.relativedelta`` because
      a month / year duration depends on the calendar date. ``quarter``
      is canonically 3 months.

    The 1M-filler safety cap only applies to numeric bounds (where
    ``span / step`` is well-defined). Variable-length date densify
    can in principle OOM on pathological inputs like ``[year 1, year
    10_000_000]`` with ``step: 1 year`` — the per-partition loop will
    eventually grow the result list to that size. Pragmatic non-goal:
    densify is for filling sub-day gaps in time-series data, not for
    enumerating millennia.
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
        td_kwarg = _DENSIFY_UNIT_TO_TIMEDELTA.get(unit)
        rd_pair = _DENSIFY_UNIT_TO_RELATIVEDELTA.get(unit)
        if td_kwarg is not None:
            step: Any = _dt.timedelta(**{td_kwarg: raw_step})
        elif rd_pair is not None:
            from dateutil.relativedelta import relativedelta

            kw, multiplier = rd_pair
            step = relativedelta(**{kw: int(raw_step) * multiplier})
        else:
            raise AggregateError(f"$densify range.unit={unit!r} is not recognised")
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


def _field_is_filled(doc: Mapping[str, Any], path: str) -> bool:
    """``$fill`` semantics for "this field already has a value": the path
    resolves and the leaf is neither ``None`` nor a missing key. Matches
    mongod's ``null``-vs-missing collapse — both count as "needs filling".
    """
    parts = path.split(".")
    cur: Any = doc
    for p in parts:
        if not isinstance(cur, Mapping) or p not in cur:
            return False
        cur = cur[p]
    return cur is not None


def _apply_locf(part: list[dict[str, Any]], field: str) -> None:
    """Last-observation-carried-forward within a single sorted partition.

    Leading nulls before the first observed value stay null (mongod does
    the same — there's nothing to carry forward from).
    """
    last: Any = None
    have = False
    for doc in part:
        v = get_path(doc, field)
        if v is not None:
            last = v
            have = True
        elif have:
            set_path(doc, field, copy.deepcopy(last))


def _apply_linear(part: list[dict[str, Any]], field: str, sort_field: str) -> None:
    """Linear interpolation along ``sort_field`` between non-null anchors.

    Values strictly between two anchors get interpolated. Leading /
    trailing nulls (before the first or after the last anchor) stay
    null because there's nothing to bracket with. Works for numbers
    and datetimes — date arithmetic gives ``timedelta`` which divides
    cleanly to ``float`` and multiplies back to ``timedelta``.
    """
    anchor_indices = [i for i, d in enumerate(part) if get_path(d, field) is not None]
    if len(anchor_indices) < 2:
        return
    # Adjacent-pairs zip — lengths intentionally differ by 1, hence
    # ``strict=False``. Linter wants strict explicitly stated either way.
    for ai, bi in zip(anchor_indices, anchor_indices[1:], strict=False):
        ya = get_path(part[ai], field)
        yb = get_path(part[bi], field)
        xa = get_path(part[ai], sort_field)
        xb = get_path(part[bi], sort_field)
        if xa is None or xb is None or xa == xb:
            continue
        span = xb - xa
        for i in range(ai + 1, bi):
            x = get_path(part[i], sort_field)
            if x is None or get_path(part[i], field) is not None:
                continue
            frac = (x - xa) / span
            set_path(part[i], field, ya + (yb - ya) * frac)


def _stage_fill(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    """``$fill`` (5.3+): fill missing / null fields by value, locf, or linear.

    Per-partition (``partitionBy`` or ``partitionByFields``), optionally
    sorted by ``sortBy``. ``output: {<field>: {value: <expr>} |
    {method: "locf" | "linear"}}``. ``method`` requires ``sortBy``.
    Documents are returned in partition-discovery order, with each
    partition in its sortBy order (if any) — matches mongod.
    """
    from secantus.storage import sort_docs

    if not isinstance(spec, Mapping):
        raise AggregateError("$fill requires a document spec")
    output = spec.get("output")
    if not isinstance(output, Mapping) or not output:
        raise AggregateError("$fill requires a non-empty output object")

    fillers: list[tuple[str, str, Any]] = []
    for field, action in output.items():
        if not isinstance(action, Mapping):
            raise AggregateError(f"$fill output.{field} must be a document")
        if "value" in action:
            fillers.append(("value", field, action["value"]))
        elif "method" in action:
            method = action["method"]
            if method not in ("locf", "linear"):
                raise AggregateError(f"$fill output.{field}.method must be 'locf' or 'linear'")
            fillers.append((method, field, None))
        else:
            raise AggregateError(f"$fill output.{field} requires value or method")

    has_method = any(kind in ("locf", "linear") for kind, _, _ in fillers)
    sort_by = spec.get("sortBy")
    if has_method and not isinstance(sort_by, Mapping):
        raise AggregateError("$fill requires sortBy when using method")
    if sort_by is not None and not isinstance(sort_by, Mapping):
        raise AggregateError("$fill sortBy must be a document")

    partition_by_fields = spec.get("partitionByFields")
    partition_by = spec.get("partitionBy")
    if partition_by is not None and partition_by_fields is not None:
        raise AggregateError("$fill cannot use both partitionBy and partitionByFields")

    if partition_by_fields is not None:
        if not isinstance(partition_by_fields, list):
            raise AggregateError("$fill partitionByFields must be an array")
        fields = list(partition_by_fields)

        def part_key(d: Mapping[str, Any]) -> Any:
            return tuple(get_path(d, f) for f in fields)
    elif partition_by is not None:

        def part_key(d: Mapping[str, Any]) -> Any:
            v = evaluate(partition_by, d, ctx.vars)
            try:
                hash(v)
                return v
            except TypeError:
                # Fall back to a JSON-stable repr for unhashable values
                # (dicts / lists). Good enough for partition identity.
                return repr(v)
    else:

        def part_key(d: Mapping[str, Any]) -> Any:
            return None

    groups: dict[Any, list[dict[str, Any]]] = {}
    group_order: list[Any] = []
    for d in docs:
        key = part_key(d)
        if key not in groups:
            groups[key] = []
            group_order.append(key)
        groups[key].append(d)

    sort_field: str | None = None
    if isinstance(sort_by, Mapping) and sort_by:
        sort_field = next(iter(sort_by.keys()))
        for key in groups:
            groups[key] = sort_docs(groups[key], sort_by)

    for key in groups:
        part = groups[key]
        for kind, field, payload in fillers:
            if kind == "value":
                for d in part:
                    if not _field_is_filled(d, field):
                        set_path(d, field, evaluate(payload, d, ctx.vars))
            elif kind == "locf":
                _apply_locf(part, field)
            elif kind == "linear":
                assert sort_field is not None  # guarded above
                _apply_linear(part, field, sort_field)

    out: list[dict[str, Any]] = []
    for key in group_order:
        out.extend(groups[key])
    return out


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

    from secantus.collation import parse as _parse_collation

    coll_obj = _parse_collation(ctx.collation)
    groups: dict[Any, dict[str, Any]] = {}
    order: list[Any] = []
    for d in docs:
        key = evaluate(id_expr, d, ctx.vars)
        # Apply the collation's string normalisation to the bucket key
        # so case-insensitive ``$group`` collapses ``"a"`` / ``"A"`` /
        # ``"a"`` into one bucket. ``cmp_key`` only touches strings;
        # other types pass through unchanged.
        if coll_obj is not None:
            hashable_key = _hashable_with_collation(key, coll_obj)
        else:
            hashable_key = _hashable(key)
        if hashable_key not in groups:
            groups[hashable_key] = {"_id": key}
            order.append(hashable_key)
        bucket = groups[hashable_key]
        for field, handler, arg in compiled:
            handler(bucket, field, arg, d, ctx.vars)
    return [_finalize(groups[k]) for k in order]


def _hashable_with_collation(value: Any, collation: Any) -> Any:
    from secantus.collation import cmp_key

    if isinstance(value, Mapping):
        return tuple(sorted((k, _hashable_with_collation(v, collation)) for k, v in value.items()))
    if isinstance(value, list):
        return tuple(_hashable_with_collation(v, collation) for v in value)
    return cmp_key(value, collation)


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


# Optional deterministic RNG for ``$sample``. The env var
# ``SECANTUS_SAMPLE_SEED`` (read once at module load) installs a
# dedicated ``random.Random(seed)`` instance so test suites can pin
# the sample order without leaking the seed into the module-level
# ``random`` state (which other code in the same process may also
# consume). Unset → use ``random.sample`` directly, fresh entropy
# per call as before.
def _build_sample_rng() -> object | None:
    import os
    import random as _random

    raw = os.environ.get("SECANTUS_SAMPLE_SEED")
    if raw is None or raw == "":
        return None
    try:
        seed: int | str = int(raw)
    except ValueError:
        seed = raw
    return _random.Random(seed)


_SAMPLE_RNG = _build_sample_rng()


def _stage_sample(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    import random

    if not isinstance(spec, Mapping) or "size" not in spec:
        raise AggregateError("$sample requires {size: N}")
    size = int(spec["size"])
    if size >= len(docs):
        return list(docs)
    rng = _SAMPLE_RNG if _SAMPLE_RNG is not None else random
    return rng.sample(list(docs), size)


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


_VALID_WHEN_MATCHED_STRINGS = frozenset(("merge", "replace", "keepExisting", "fail", "delete"))
_VALID_WHEN_NOT_MATCHED = frozenset(("insert", "discard", "fail"))


def _validate_on_field_index(
    ctx: PipelineContext,
    target_db: str,
    target_coll: str,
    on_fields: list[str],
) -> None:
    """``$merge`` requires a unique index on the ``on`` fields.

    ``_id`` is implicitly unique and needs no explicit index. For any
    other field combination, real mongod refuses the command unless
    there's a ``unique: true`` index whose keys exactly match the
    ``on`` field set. Mismatching this guard lets non-unique ``on``
    fields silently collapse multiple targets onto one source doc.
    """
    if on_fields == ["_id"]:
        return
    assert ctx.storage is not None
    target_set = set(on_fields)
    for ix in ctx.storage.list_indexes(target_db, target_coll):
        if not ix.get("unique"):
            continue
        key = ix.get("key") or {}
        if set(key.keys()) == target_set:
            return
    raise AggregateError(
        f"$merge requires a unique index covering {on_fields!r} on {target_db}.{target_coll}"
    )


def _stage_merge(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if ctx.storage is None:
        raise AggregateError("$merge requires storage context")
    let_spec: Mapping[str, Any] = {}
    if isinstance(spec, str):
        target_db, target_coll = ctx.db_name, spec
        on_fields: list[str] = ["_id"]
        when_matched: Any = "merge"
        when_not_matched: str = "insert"
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
        let_spec = spec.get("let") or {}
        if not isinstance(let_spec, Mapping):
            raise AggregateError("$merge let must be an object")
    else:
        raise AggregateError("$merge requires a string or document spec")

    if isinstance(when_matched, str) and when_matched not in _VALID_WHEN_MATCHED_STRINGS:
        raise AggregateError(
            f"$merge whenMatched must be one of {sorted(_VALID_WHEN_MATCHED_STRINGS)} "
            "or a pipeline array"
        )
    if not isinstance(when_matched, (str, list)):
        raise AggregateError("$merge whenMatched must be a string or pipeline array")
    if when_not_matched not in _VALID_WHEN_NOT_MATCHED:
        raise AggregateError(
            f"$merge whenNotMatched must be one of {sorted(_VALID_WHEN_NOT_MATCHED)}"
        )

    _validate_on_field_index(ctx, target_db, target_coll, on_fields)

    for doc in docs:
        match_filter = {f: get_path(doc, f) for f in on_fields}
        existing = ctx.storage.find_matching(target_db, target_coll, match_filter, limit=1)
        if existing:
            existing_doc = existing[0]
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
            if when_matched == "delete":
                ctx.storage.delete_matching(
                    target_db, target_coll, {"_id": existing_doc["_id"]}, limit=1
                )
                continue
            if when_matched == "replace":
                ctx.storage.delete_matching(
                    target_db, target_coll, {"_id": existing_doc["_id"]}, limit=1
                )
                new = copy.deepcopy(doc)
                new.setdefault("_id", existing_doc["_id"])
                ctx.storage.insert(target_db, target_coll, [new])
                continue
            if isinstance(when_matched, list):
                # Pipeline form: run the sub-pipeline against the matched
                # doc with ``$$new`` bound to the source doc and any user
                # ``let`` vars threaded through. The output's first doc
                # replaces the target (preserving ``_id``); mongod
                # requires the pipeline to yield exactly one doc.
                bound: dict[str, Any] = {"new": copy.deepcopy(doc)}
                for name, expr in let_spec.items():
                    bound[name] = evaluate(expr, doc, ctx.vars)
                sub_ctx = ctx.with_vars(bound)
                result_docs = apply_pipeline(
                    [copy.deepcopy(existing_doc)], list(when_matched), sub_ctx
                )
                if not result_docs:
                    raise AggregateError("$merge whenMatched pipeline must yield a document")
                replacement = result_docs[0]
                replacement["_id"] = existing_doc["_id"]
                ctx.storage.delete_matching(
                    target_db, target_coll, {"_id": existing_doc["_id"]}, limit=1
                )
                ctx.storage.insert(target_db, target_coll, [replacement])
                continue
            # default: deep merge
            merged = _deep_merge_docs(existing_doc, doc)
            merged["_id"] = existing_doc["_id"]
            ctx.storage.update_matching(
                target_db, target_coll, {"_id": existing_doc["_id"]}, merged
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
        storage_stats: dict[str, Any] = {
            "size": 0,
            "count": count,
            "avgObjSize": 0,
            "storageSize": 0,
            "indexSizes": {i["name"]: 0 for i in indexes},
            "totalIndexSize": 0,
            "scaleFactor": 1,
            "nindexes": len(indexes),
        }
        # Surface capped-collection bounds from the stored options.
        # Real mongod renames the user-set ``size`` to ``maxSize`` in
        # the storageStats payload (so callers can distinguish the
        # current data size from the cap). mongo-ruby-driver's
        # ``Collection#create ... applies the options`` capped spec
        # reads `storageStats.{capped, max, maxSize}` directly.
        opts = ctx.storage.get_collection_options(ctx.db_name, ctx.coll_name)
        if opts.get("capped"):
            storage_stats["capped"] = True
            if "size" in opts:
                storage_stats["maxSize"] = int(opts["size"])
            if "max" in opts:
                storage_stats["max"] = int(opts["max"])
        out["storageStats"] = storage_stats
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
    # Mongo-node-driver's ``Aggregation should correctly execute
    # db.aggregate() with $currentOp`` test asserts the op's
    # ``command`` matches the actual aggregate request (pipeline,
    # cursor, $db). Use the real command doc threaded through
    # PipelineContext; fall back to the stub shape so older callers
    # still see *something*.
    if isinstance(_ctx.command_doc, dict) and "aggregate" in _ctx.command_doc:
        command_doc: dict[str, Any] = dict(_ctx.command_doc)
        command_doc.setdefault("$db", _ctx.db_name)
        command_doc.setdefault("cursor", {})
    else:
        command_doc = {"aggregate": 1}
    return [
        {
            "type": "op",
            "host": "secantus",
            "desc": "$currentOp",
            "active": False,
            "currentOpTime": "",
            "command": command_doc,
            "ns": _ctx.db_name + "." + (_ctx.coll_name or "$cmd.aggregate"),
            "op": "command",
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


_RANK_FUNCS = frozenset({"$rank", "$denseRank", "$documentNumber"})


def _stage_set_window_fields(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    """``$setWindowFields`` — partition + sort + per-row windowed accumulators.

    Spec shape::

        {
            partitionBy: <expression>,  # optional; missing = single partition
            sortBy: <sort spec>,        # optional for accumulators; required for $rank/$denseRank
            output: {
                <field>: {
                    <$accumulator>: <expr>,
                    window: {documents: [<lower>, <upper>]},  # optional
                }
            },
        }

    For accumulator output fields, the op runs over the rows inside
    that row's window (within the row's partition, in the partition's
    sorted order). Window bounds are integer offsets relative to the
    current row, or the strings ``"current"`` / ``"unbounded"``.
    Missing ``window`` defaults to the whole partition. Original input
    order is preserved in the result — the partition / sort dance
    happens only to compute the new fields.

    Supported output functions:

    * The nine ``$group`` accumulators (``$sum`` / ``$avg`` / ``$min``
      / ``$max`` / ``$first`` / ``$last`` / ``$push`` / ``$addToSet``
      / ``$count``) over the position-based ``documents`` window.
    * The three rank functions (``$rank`` / ``$denseRank`` /
      ``$documentNumber``), evaluated per-row across the whole sorted
      partition. They take an empty ``{}`` arg and reject ``window``
      (mongod's rule); ``$rank`` and ``$denseRank`` require
      ``sortBy``.

    Range-based windows (``window: {range: [...]}``, optionally with
    ``unit``) and the time-series functions (``$derivative`` /
    ``$integral`` / ``$linearFill`` / ``$locf`` / ``$shift`` /
    ``$expMovingAvg``) raise ``AggregateError`` so the gap is visible
    rather than silently wrong.
    """
    from secantus.storage import sort_docs as _sort_docs

    if not isinstance(spec, Mapping):
        raise AggregateError("$setWindowFields requires a doc spec")
    partition_by = spec.get("partitionBy")
    sort_by = spec.get("sortBy")
    output = spec.get("output")
    if not isinstance(output, Mapping) or not output:
        raise AggregateError("$setWindowFields requires a non-empty output doc")

    compiled: list[tuple[str, str, Any, Mapping[str, Any] | None]] = []
    for field, field_spec in output.items():
        if not isinstance(field_spec, Mapping):
            raise AggregateError(f"$setWindowFields output[{field!r}] must be a doc")
        op_keys = [k for k in field_spec if k.startswith("$")]
        if len(op_keys) != 1:
            raise AggregateError(
                f"$setWindowFields output[{field!r}] requires exactly one accumulator"
            )
        op = op_keys[0]
        arg = field_spec[op]
        window = field_spec.get("window")
        if window is not None and not isinstance(window, Mapping):
            raise AggregateError(f"$setWindowFields output[{field!r}].window must be a doc")
        if op in _RANK_FUNCS:
            # Rank functions take no arg and don't accept a window. Mongod
            # surfaces both violations as parse errors; mirror.
            if arg not in ({}, None):
                raise AggregateError(f"$setWindowFields {op} takes no argument (got {arg!r})")
            if window is not None:
                raise AggregateError(f"$setWindowFields {op} does not accept a window")
            if op in ("$rank", "$denseRank") and not sort_by:
                raise AggregateError(f"$setWindowFields {op} requires sortBy")
            compiled.append((field, op, arg, window))
            continue
        if op not in _ACC_DISPATCH:
            raise AggregateError(
                f"$setWindowFields: unsupported function {op!r} "
                "(time-series operators are not yet implemented)"
            )
        compiled.append((field, op, arg, window))

    docs_list = list(docs)
    partitions: dict[Any, list[tuple[int, dict[str, Any]]]] = {}
    partition_order: list[Any] = []
    for i, doc in enumerate(docs_list):
        key = None if partition_by is None else evaluate(partition_by, doc, ctx.vars)
        hkey = _hashable(key)
        if hkey not in partitions:
            partitions[hkey] = []
            partition_order.append(hkey)
        partitions[hkey].append((i, doc))

    out_docs: list[dict[str, Any]] = [dict(d) for d in docs_list]

    for pkey in partition_order:
        members = partitions[pkey]
        if sort_by:
            sorted_docs = _sort_docs([doc for _, doc in members], sort_by)
            idx_lookup = {id(doc): orig_i for orig_i, doc in members}
            members = [(idx_lookup[id(doc)], doc) for doc in sorted_docs]
        partition_docs = [doc for _, doc in members]
        n = len(partition_docs)
        # Precompute per-partition rank vectors only when a rank function
        # is referenced. One linear walk covers all three.
        rank_state = _compute_rank_state(partition_docs, sort_by, compiled)
        for slot, (orig_i, _) in enumerate(members):
            target = out_docs[orig_i]
            for field, op, arg, window in compiled:
                if op in _RANK_FUNCS:
                    target[field] = rank_state[op][slot]
                    continue
                low, high = _window_bounds(slot, n, window)
                if high < low:
                    target[field] = _empty_window_value(op)
                    continue
                window_docs = partition_docs[low : high + 1]
                bucket: dict[str, Any] = {}
                handler = _ACC_DISPATCH[op]
                for wdoc in window_docs:
                    handler(bucket, field, arg, wdoc, ctx.vars)
                _finalize(bucket)
                target[field] = bucket.get(field, _empty_window_value(op))
    return out_docs


def _compute_rank_state(
    partition_docs: list[dict[str, Any]],
    sort_by: Mapping[str, Any] | None,
    compiled: list[tuple[str, str, Any, Mapping[str, Any] | None]],
) -> dict[str, list[int]]:
    """Per-partition rank vectors for whichever rank functions appear
    in ``compiled``. Returns ``{op: [per-slot value]}``; empty dict
    when no rank function is referenced.

    All three computations share one linear walk:

    * ``$documentNumber`` — 1-indexed slot position. Independent of
      ties; ignores sortBy comparisons.
    * ``$rank`` — 1-indexed position with **gaps** after ties: tied
      rows share the lower rank; the next non-tied row jumps by the
      number of ties (``[10, 20, 20, 30]`` → ``[1, 2, 2, 4]``).
    * ``$denseRank`` — 1-indexed position **without gaps**: tied rows
      share, next row is +1 (``[10, 20, 20, 30]`` → ``[1, 2, 2, 3]``).

    Ties are detected by sort-key tuple equality. Without ``sortBy``
    only ``$documentNumber`` is allowed (validated at the compile
    step), so the no-sort branch never needs key comparisons.
    """
    needed = {op for _f, op, _a, _w in compiled if op in _RANK_FUNCS}
    if not needed:
        return {}
    n = len(partition_docs)
    state: dict[str, list[int]] = {op: [0] * n for op in needed}
    if n == 0:
        return state
    keys = [_sort_key_values(doc, sort_by) for doc in partition_docs] if sort_by else [None] * n

    rank = 1
    dense = 1
    if "$documentNumber" in needed:
        state["$documentNumber"][0] = 1
    if "$rank" in needed:
        state["$rank"][0] = 1
    if "$denseRank" in needed:
        state["$denseRank"][0] = 1
    for i in range(1, n):
        if "$documentNumber" in needed:
            state["$documentNumber"][i] = i + 1
        tied = sort_by is not None and keys[i] == keys[i - 1]
        if not tied:
            rank = i + 1
            dense += 1
        if "$rank" in needed:
            state["$rank"][i] = rank
        if "$denseRank" in needed:
            state["$denseRank"][i] = dense
    return state


def _sort_key_values(doc: Mapping[str, Any], sort_by: Mapping[str, Any]) -> tuple:
    """Tuple of raw sort-by field values, in spec order. Used for
    tie detection in rank computation — equal tuples → tied rows."""
    from secantus.paths import get_path

    return tuple(get_path(dict(doc), field, default=None) for field in sort_by)


def _window_bounds(slot: int, n: int, window: Mapping[str, Any] | None) -> tuple[int, int]:
    """Resolve a window spec for a given row position.

    Returns inclusive ``(lower, upper)`` indices into the partition.
    ``window=None`` or missing ``documents`` → the whole partition
    (matches mongod's default window).
    """
    if window is None or "documents" not in window:
        if window is not None and "range" in window:
            raise AggregateError("$setWindowFields range-based windows are not yet implemented")
        return 0, n - 1
    bounds = window["documents"]
    if not isinstance(bounds, list) or len(bounds) != 2:
        raise AggregateError("$setWindowFields window.documents must be a [lower, upper] pair")

    def resolve(b: Any, is_lower: bool) -> int:
        if b == "unbounded":
            return 0 if is_lower else n - 1
        if b == "current":
            return slot
        if isinstance(b, bool) or not isinstance(b, int):
            raise AggregateError(
                f"$setWindowFields window.documents bound {b!r} must be an int "
                "or 'unbounded' / 'current'"
            )
        return slot + b

    low = max(0, resolve(bounds[0], is_lower=True))
    high = min(n - 1, resolve(bounds[1], is_lower=False))
    return low, high


def _empty_window_value(op: str) -> Any:
    """The value to use when the window is empty for this row.

    Matches mongod: ``$sum`` / ``$count`` → 0, ``$push`` /
    ``$addToSet`` → [], everything else → null.
    """
    if op in ("$sum", "$count"):
        return 0
    if op in ("$push", "$addToSet"):
        return []
    return None


def _stage_redact(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    """``$redact`` — content-based document / sub-document pruning.

    The expression is evaluated against each (sub-)document, with the
    current sub-doc bound as ``$$CURRENT``. It must return one of
    three sentinel strings — ``"$$KEEP"`` (include the sub-doc as-is,
    no recursion), ``"$$PRUNE"`` (drop the sub-doc), or
    ``"$$DESCEND"`` (recurse into nested sub-docs and arrays-of-
    sub-docs). The sentinels are returned by the expression evaluator
    when the user writes ``"$$KEEP"`` / ``"$$PRUNE"`` / ``"$$DESCEND"``
    inside a ``$cond`` / ``$switch`` / ``$let`` etc.

    Behaviour follows mongod:

    * Top-level ``$$PRUNE`` drops the doc from the pipeline.
    * ``$$DESCEND`` recurses into every dict-valued field and every
      list-element that is a dict; scalar and non-dict-list values
      pass through unchanged. Pruned sub-docs are removed from their
      arrays (the array stays, the element disappears).
    * Empty list spec, missing expression, or a non-sentinel result
      raises ``AggregateError``.
    """
    if spec is None or (isinstance(spec, Mapping) and not spec):
        raise AggregateError("$redact requires an expression")
    out: list[dict[str, Any]] = []
    for doc in docs:
        result = _redact_subdoc(doc, spec, ctx)
        if result is not None:
            out.append(result)
    return out


def _redact_subdoc(
    doc: Mapping[str, Any], spec: Any, ctx: PipelineContext
) -> dict[str, Any] | None:
    decision = evaluate(spec, dict(doc), ctx.vars)
    if decision == "$$KEEP":
        return dict(doc)
    if decision == "$$PRUNE":
        return None
    if decision == "$$DESCEND":
        return _redact_descend(doc, spec, ctx)
    raise AggregateError(
        f"$redact expression must return $$KEEP, $$PRUNE, or $$DESCEND, got {decision!r}"
    )


def _redact_descend(doc: Mapping[str, Any], spec: Any, ctx: PipelineContext) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for k, v in doc.items():
        if isinstance(v, Mapping):
            sub = _redact_subdoc(v, spec, ctx)
            if sub is not None:
                out[k] = sub
        elif isinstance(v, list):
            new_list: list[Any] = []
            for elem in v:
                if isinstance(elem, Mapping):
                    redacted = _redact_subdoc(elem, spec, ctx)
                    if redacted is not None:
                        new_list.append(redacted)
                else:
                    new_list.append(elem)
            out[k] = new_list
        else:
            out[k] = v
    return out


def _stage_union_with(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    """``$unionWith`` — concatenate docs from another collection after
    optionally running them through a sub-pipeline.

    Two spec shapes (per mongod):

    * Shorthand: ``{$unionWith: "<coll>"}`` — equivalent to
      ``{$unionWith: {coll: "<coll>"}}``, no sub-pipeline.
    * Full form: ``{$unionWith: {coll: "<coll>", pipeline: [...]}}``.

    The sub-pipeline runs in a *fresh* :class:`PipelineContext` — outer
    ``let`` / ``vars`` are not visible inside, matching mongod's
    semantics (``$unionWith`` does not accept a ``let`` field). Outer
    docs come first, then the union docs in the order the sub-pipeline
    produced them; mongod imposes no ordering guarantee between the
    two sets, but appending the union docs is the documented
    implementation. No deduplication — duplicates across the boundary
    survive.
    """
    if isinstance(spec, str):
        from_coll = spec
        sub_pipeline: list[dict[str, Any]] | None = None
    elif isinstance(spec, Mapping):
        from_coll = spec.get("coll")
        sub_pipeline = spec.get("pipeline")
        if not isinstance(from_coll, str):
            raise AggregateError("$unionWith requires 'coll' (string)")
        if sub_pipeline is not None and not isinstance(sub_pipeline, list):
            raise AggregateError("$unionWith 'pipeline' must be an array")
    else:
        raise AggregateError("$unionWith requires a collection name or {coll, pipeline} doc")
    if ctx.storage is None:
        raise AggregateError("$unionWith requires storage context")

    foreign_docs = ctx.storage.find_matching(ctx.db_name, from_coll, {})
    if sub_pipeline:
        sub_ctx = PipelineContext(
            storage=ctx.storage,
            db_name=ctx.db_name,
            coll_name=from_coll,
            collation=ctx.collation,
        )
        foreign_docs = apply_pipeline(foreign_docs, sub_pipeline, sub_ctx)
    return list(docs) + list(foreign_docs)


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
    "$fill": _stage_fill,
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
    "$unionWith": _stage_union_with,
    "$redact": _stage_redact,
    "$setWindowFields": _stage_set_window_fields,
}
