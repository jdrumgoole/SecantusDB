from __future__ import annotations

import copy
from collections.abc import Mapping
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
    vars: dict[str, Any] = _dc_field(default_factory=dict)

    def with_vars(self, more: dict[str, Any]) -> PipelineContext:
        return PipelineContext(
            storage=self.storage,
            db_name=self.db_name,
            vars={**self.vars, **more},
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

    groups: dict[Any, dict[str, Any]] = {}
    order: list[Any] = []
    for d in docs:
        key = evaluate(id_expr, d, ctx.vars)
        hashable_key = _hashable(key)
        if hashable_key not in groups:
            groups[hashable_key] = {"_id": key}
            order.append(hashable_key)
        bucket = groups[hashable_key]
        for field, accumulator in accumulators.items():
            _accumulate(bucket, field, accumulator, d, ctx.vars)
    return [_finalize(groups[k]) for k in order]


def _finalize(bucket: dict[str, Any]) -> dict[str, Any]:
    for k, v in list(bucket.items()):
        if isinstance(v, dict) and "_avg_total" in v and "_avg_n" in v:
            bucket[k] = v["_avg_total"] / v["_avg_n"] if v["_avg_n"] else None
    return bucket


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
    if op == "$sum":
        increment = 1 if arg == 1 else evaluate(arg, doc, vars)
        if increment is None:
            increment = 0
        bucket[field] = bucket.get(field, 0) + increment
    elif op == "$count":
        bucket[field] = bucket.get(field, 0) + 1
    elif op == "$avg":
        v = evaluate(arg, doc, vars)
        if v is None:
            return
        state = bucket.get(field)
        if not isinstance(state, dict) or "_avg_total" not in state:
            state = {"_avg_total": 0, "_avg_n": 0}
            bucket[field] = state
        state["_avg_total"] += v
        state["_avg_n"] += 1
    elif op == "$max":
        v = evaluate(arg, doc, vars)
        cur = bucket.get(field)
        if cur is None or (v is not None and v > cur):
            bucket[field] = v
    elif op == "$min":
        v = evaluate(arg, doc, vars)
        cur = bucket.get(field)
        if cur is None or (v is not None and v < cur):
            bucket[field] = v
    elif op == "$first":
        if field not in bucket:
            bucket[field] = evaluate(arg, doc, vars)
    elif op == "$last":
        bucket[field] = evaluate(arg, doc, vars)
    elif op == "$push":
        bucket.setdefault(field, []).append(evaluate(arg, doc, vars))
    elif op == "$addToSet":
        v = evaluate(arg, doc, vars)
        bucket.setdefault(field, [])
        if v not in bucket[field]:
            bucket[field].append(v)
    else:
        raise AggregateError(f"unsupported $group accumulator: {op}")


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
    foreign_docs = ctx.storage.find_matching(ctx.db_name, from_coll, {})
    out: list[dict[str, Any]] = []
    for doc in docs:
        local_value = get_path(doc, local_field)
        matches_list = [
            fd for fd in foreign_docs if _lookup_match(local_value, get_path(fd, foreign_field))
        ]
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
    foreign_docs = ctx.storage.find_matching(ctx.db_name, from_coll, {})
    local_field = full_spec.get("localField")
    foreign_field = full_spec.get("foreignField")
    out: list[dict[str, Any]] = []
    for doc in docs:
        bound = {name: evaluate(expr, doc, ctx.vars) for name, expr in let_spec.items()}
        if isinstance(local_field, str) and isinstance(foreign_field, str):
            local_value = get_path(doc, local_field)
            candidates = [
                fd for fd in foreign_docs if _lookup_match(local_value, get_path(fd, foreign_field))
            ]
        else:
            candidates = list(foreign_docs)
        sub_ctx = ctx.with_vars(bound)
        joined = apply_pipeline(candidates, sub_pipeline, sub_ctx)
        new = copy.deepcopy(doc)
        new[as_field] = joined
        out.append(new)
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
    "$replaceRoot": _stage_replace_root,
    "$replaceWith": _stage_replace_with,
    "$group": _stage_group,
    "$lookup": _stage_lookup,
    "$sample": _stage_sample,
    "$sortByCount": _stage_sort_by_count,
    "$facet": _stage_facet,
    "$bucket": _stage_bucket,
}
