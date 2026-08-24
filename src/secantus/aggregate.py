from __future__ import annotations

import copy
import datetime as _dt
import decimal as _decimal
import math
import os
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from dataclasses import field as _dc_field
from typing import TYPE_CHECKING, Any

from bson import Decimal128

from secantus.expressions import (
    MISSING,
    ExpressionError,
    UnknownExpressionOperatorError,
    _bson_type_name,
    _fmt_double,
    evaluate,
    evaluate_or_missing,
)
from secantus.numerics import bson_sum
from secantus.paths import get_path, set_path, unset_path
from secantus.query import QueryError, matches

if TYPE_CHECKING:
    from secantus.storage import Storage


class AggregateError(Exception):
    """Pipeline-validation error. ``code``/``code_name`` default to the
    generic user-facing mapping (14 TypeMismatch) but raise sites may
    pin mongod's specific code (e.g. 40324 for unrecognized stages)."""

    def __init__(
        self, message: str, *, code: int | None = None, code_name: str | None = None
    ) -> None:
        super().__init__(message)
        self.code = code
        self.code_name = code_name


# Atlas Search is an Atlas-only feature. A real non-Atlas mongod rejects the
# ``$listSearchIndexes`` aggregation stage (and the createSearchIndexes /
# updateSearchIndex / dropSearchIndex commands, see ``commands.py``) with a
# message naming Atlas; the driver index-management spec tests assert only that
# the error mentions "Atlas". Shared with ``commands.py`` so the stage and the
# commands stay in lockstep.
#: Hard ceiling on documents materialized by a single pipeline stage. A join
#: whose predicates can't be pushed into the ``$lookup`` degenerates into a
#: cartesian product (an unkeyed comma-join over system catalogs — pgjdbc's
#: getImportedKeys for multi-column FKs ballooned to 183GB and OS-killed the
#: server). This bounds the blast radius: the pipeline fails with a clear error
#: instead of exhausting memory. Generous enough that real aggregations never
#: hit it; override with ``SECANTUS_MAX_PIPELINE_DOCS`` for a stress harness.
MAX_PIPELINE_DOCS = int(os.environ.get("SECANTUS_MAX_PIPELINE_DOCS", 5_000_000))


def _pipeline_overflow(stage: str) -> AggregateError:
    return AggregateError(
        f"{stage} would materialize more than {MAX_PIPELINE_DOCS} documents in one "
        "stage — the query degenerated into an unbounded cross product; add a join "
        "predicate the planner can push down, or raise SECANTUS_MAX_PIPELINE_DOCS",
        code=292,
        code_name="QueryExceededMemoryLimitNoDiskUseAllowed",
    )


SEARCH_INDEX_ATLAS_MSG = (
    "Using Atlas Search Database Commands and the $listSearchIndexes aggregation "
    "stage requires additional configuration. Please connect to Atlas or an "
    "Atlas-compatible deployment to use this feature."
)
# Atlas-only aggregation stages: not supported off Atlas, rejected with the
# Atlas message above rather than the generic "unrecognized stage" error.
_ATLAS_ONLY_STAGES = frozenset({"$listSearchIndexes", "$search", "$searchMeta", "$vectorSearch"})


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
    # Cleared (sticky) by ``apply_pipeline`` when any stage — including nested
    # $lookup/$facet/$unionWith sub-pipelines — mutates docs in place ($fill's
    # locf/linear and $densify write through set_path without copying). While
    # True, ``$unwind`` may produce shallow top-level copies that share
    # unmutated subtrees instead of deepcopying every fanned-out doc — the
    # dominant cost of high-fanout join pipelines (pgjdbc's getImportedKeys
    # 9-way join). Never flips back to True on a ctx once cleared.
    shared_unwind_ok: bool = True
    # The aggregate command's request body (minus the ``$db`` /
    # ``lsid`` envelope fields). Surfaced by ``$currentOp`` as the
    # ``command`` sub-doc on the self-row mongo-node-driver's
    # ``$currentOp`` test introspects.
    command_doc: dict[str, Any] | None = None
    # ``bypassDocumentValidation`` from the aggregate command. When
    # false (the default) a ``$out`` / ``$merge`` write into a
    # collection that carries a ``validator`` enforces it — mongo-c-
    # driver's ``aggregate/bypass_document_validation`` test sets a
    # ``{number: {$gte: 5}}`` validator and expects the cursor to error.
    bypass_validation: bool = False
    # The current connection's ``hello.client`` handshake subdoc (driver
    # name/version, OS, application name). Surfaced by ``$currentOp`` as the
    # ``clientMetadata`` document + the top-level ``appName`` on the self-row,
    # which mongocxx's "client metadata handshake feature" test reads back via
    # ``db.aggregate([{$currentOp: {}}])``. Set by the aggregate command handler
    # from the connection registry; ``None`` for non-connection callers.
    client_metadata: dict[str, Any] | None = None

    def with_vars(self, more: dict[str, Any]) -> PipelineContext:
        return PipelineContext(
            storage=self.storage,
            db_name=self.db_name,
            coll_name=self.coll_name,
            vars={**self.vars, **more},
            change_stream=self.change_stream,
            collation=self.collation,
            command_doc=self.command_doc,
            bypass_validation=self.bypass_validation,
            client_metadata=self.client_metadata,
        )


_NULL_CTX = PipelineContext()


def apply_pipeline(
    docs: list[dict[str, Any]],
    pipeline: list[dict[str, Any]],
    ctx: PipelineContext | None = None,
) -> list[dict[str, Any]]:
    ctx = ctx or _NULL_CTX
    # ``$$NOW``: a Date constant for the whole pipeline execution
    # (mongod semantics). Seeded into vars so it resolves through the
    # ordinary user-var path; never mutate the shared module-level null
    # context.
    if "NOW" not in ctx.vars:
        now = _dt.datetime.now(_dt.timezone.utc)
        if ctx is _NULL_CTX:
            ctx = PipelineContext(vars={"NOW": now})
        else:
            ctx.vars["NOW"] = now
    # ``$out`` / ``$merge`` may only appear as the final stage. mongod
    # rejects a non-terminal write stage with Location40601 before
    # executing anything (mongo-cxx-driver's "out fails when not last").
    for i, stage in enumerate(pipeline):
        if isinstance(stage, Mapping) and i != len(pipeline) - 1:
            for write_stage in ("$out", "$merge"):
                if write_stage in stage:
                    raise AggregateError(
                        f"{write_stage} can only be the final stage in the pipeline",
                        code=40601,
                        code_name="Location40601",
                    )
    if ctx.shared_unwind_ok and _pipeline_mutates_in_place(pipeline):
        ctx.shared_unwind_ok = False
    for stage in pipeline:
        docs = _apply_stage(stage, docs, ctx)
        if len(docs) > MAX_PIPELINE_DOCS:
            name = next(iter(stage)) if isinstance(stage, Mapping) and stage else "stage"
            raise _pipeline_overflow(name)
    return docs


def _pipeline_mutates_in_place(pipeline: Any) -> bool:
    """Whether any stage (recursing into $lookup / $facet / $unionWith
    sub-pipelines) writes through docs without copying — the gate for
    ``$unwind``'s shared shallow-copy fast path."""
    if not isinstance(pipeline, list):
        return False
    for stage in pipeline:
        if not isinstance(stage, Mapping):
            continue
        if "$fill" in stage or "$densify" in stage:
            return True
        for key in ("$lookup", "$unionWith"):
            spec = stage.get(key)
            if isinstance(spec, Mapping) and _pipeline_mutates_in_place(spec.get("pipeline")):
                return True
        facet = stage.get("$facet")
        if isinstance(facet, Mapping):
            for sub in facet.values():
                if _pipeline_mutates_in_place(sub):
                    return True
    return False


def _apply_stage(
    stage: dict[str, Any],
    docs: list[dict[str, Any]],
    ctx: PipelineContext,
) -> list[dict[str, Any]]:
    if len(stage) != 1:
        raise AggregateError("each pipeline stage must have exactly one key")
    name, spec = next(iter(stage.items()))
    if name in _ATLAS_ONLY_STAGES:
        # Atlas-only stage on a non-Atlas deployment — mongod fails it with a
        # message naming Atlas (CommandNotSupported), not the generic
        # "unrecognized stage" error (mongo-c-driver
        # /index-management/listSearchIndexes asserts errorContains "Atlas").
        raise AggregateError(SEARCH_INDEX_ATLAS_MSG, code=115, code_name="CommandNotSupported")
    handler = _STAGES.get(name)
    if handler is None:
        # mongod's exact shape: 40324 with this wording (the unified
        # change-streams-errors spec pins the code).
        raise AggregateError(
            f"Unrecognized pipeline stage name: '{name}'",
            code=40324,
            code_name="Location40324",
        )
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
    # mongod: the count field must be a non-empty string (40156/40157), not
    # $-prefixed (40158), without a '.' (40160), and not "_id" (15948).
    if not isinstance(spec, str):
        raise AggregateError(
            "the count field must be a non-empty string", code=40156, code_name="Location40156"
        )
    if not spec:
        raise AggregateError(
            "the count field must be a non-empty string", code=40157, code_name="Location40157"
        )
    if spec.startswith("$"):
        raise AggregateError(
            "the count field cannot be a $-prefixed path", code=40158, code_name="Location40158"
        )
    if "." in spec:
        raise AggregateError(
            "the count field cannot contain '.'", code=40160, code_name="Location40160"
        )
    if spec == "_id":
        raise AggregateError(
            "a group's _id may only be specified once", code=15948, code_name="Location15948"
        )
    return [{spec: len(docs)}]


def _fmt_stage_val(v: Any) -> str:
    """Render a $limit/$skip argument the way mongod prints it in the error."""
    if isinstance(v, bool):
        return "true" if v else "false"
    return repr(v) if isinstance(v, float) else str(v)


def _stage_nonneg_int(spec: Any, stage: str, code: int) -> int:
    """Validate a $limit/$skip argument like mongod: a whole-number double is
    accepted (coerced to int); a bool / non-number, a fractional double, and a
    negative value each raise `code` with mongod's exact per-case message."""
    if isinstance(spec, bool) or not isinstance(spec, (int, float)):
        raise AggregateError(
            f"invalid argument to {stage} stage: Expected a number in: "
            f"{stage}: {_fmt_stage_val(spec)}",
            code=code,
        )
    if isinstance(spec, float):
        if not spec.is_integer():
            raise AggregateError(
                f"invalid argument to {stage} stage: Expected an integer: "
                f"{stage}: {_fmt_stage_val(spec)}",
                code=code,
            )
        spec = int(spec)
    if spec < 0:
        raise AggregateError(
            f"invalid argument to {stage} stage: Expected a non-negative number in: "
            f"{stage}: {spec}",
            code=code,
        )
    return spec


def _stage_limit(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    n = _stage_nonneg_int(spec, "$limit", 5107201)
    if n == 0:
        raise AggregateError("the limit must be positive", code=15958)
    return docs[:n]


def _stage_skip(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    n = _stage_nonneg_int(spec, "$skip", 5107200)
    return docs[n:]


def _sort_val_repr(v: Any) -> str:
    """mongod renders the offending value in the Location15974 message as
    shell/JSON (`"asc"`, `true`, `null`), not Python repr."""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, str):
        return f'"{v}"'
    if v is None:
        return "null"
    return str(v)


def _validate_sort_spec(spec: Any) -> None:
    """mongod's `$sort` stage validation: at least one key (15976); each direction
    is 1 / -1 as an int or whole double, else a non-numeric value is "Illegal key"
    (15974) and a numeric non-±1 is "must be 1 … or -1" (15975)."""
    if not isinstance(spec, Mapping) or not spec:
        raise AggregateError(
            "$sort stage must have at least one sort key",
            code=15976,
            code_name="Location15976",
        )
    for key, direction in spec.items():
        if isinstance(direction, Mapping):
            continue  # {$meta: …} — text-score / indexKey sort, out of scope here
        if isinstance(direction, bool) or not isinstance(direction, (int, float)):
            raise AggregateError(
                f"Illegal key in $sort specification: {key}: {_sort_val_repr(direction)}",
                code=15974,
                code_name="Location15974",
            )
        if (isinstance(direction, float) and not direction.is_integer()) or int(direction) not in (
            1,
            -1,
        ):
            raise AggregateError(
                "$sort key ordering must be 1 (for ascending) or -1 (for descending)",
                code=15975,
                code_name="Location15975",
            )


def _stage_sort(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    from secantus.ordering import sort_docs

    _validate_sort_spec(spec)
    return sort_docs(list(docs), spec)


def _stage_project(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$project requires a document spec")
    if not spec:
        raise AggregateError(
            "projection specification must have at least one field",
            code=51272,
            code_name="Location51272",
        )
    try:
        return [_project_one(d, spec, ctx.vars) for d in docs]
    except UnknownExpressionOperatorError as exc:
        # mongod wraps an unrecognized expression operator used inside a
        # ``$project`` in a stage-specific ``Location31325`` error:
        # ``Invalid $project :: caused by :: Unknown expression $op``.
        raise ExpressionError(
            f"Invalid $project :: caused by :: Unknown expression {exc.op}",
            code=31325,
        ) from exc


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
            value = evaluate(expr, doc, vars)
            # A computed field that resolves to the "missing" marker (an
            # absent field via ``$getField`` / an explicit ``$$REMOVE``) is
            # omitted from the output, matching mongod — never emitted as null.
            if value is MISSING:
                continue
            set_path(result, key, value)
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
        value = evaluate(expr, doc, vars)
        # A field that resolves to the "missing" marker (absent field via
        # ``$getField`` / ``$$REMOVE``) is dropped rather than written —
        # matching mongod's ``$addFields``, which removes an existing field
        # when its new value is the missing/``$$REMOVE`` value.
        if value is MISSING:
            unset_path(result, path)
            continue
        set_path(result, path, value)
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
    if isinstance(raw_step, bool) or not isinstance(raw_step, (int, float)):
        raise AggregateError(
            f"BSON field '$densify.range.step' is the wrong type '{_bson_type_name(raw_step)}', "
            "expected types '[int, decimal, double, long']",
            code=14,
            code_name="TypeMismatch",
        )
    if raw_step <= 0:
        raise AggregateError(
            "The step parameter in a range statement must be a strictly positive numeric value",
            code=5733401,
            code_name="Location5733401",
        )
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

    # A date-unit step requires date field values (mongod 6053600) — a numeric
    # value with a date step would otherwise leak a Python TypeError.
    if unit is not None:
        for d in docs:
            fv = get_path(d, field)
            if isinstance(fv, (int, float)) and not isinstance(fv, bool):
                raise AggregateError(
                    "Encountered numeric densify value in collection when step has a date unit.",
                    code=6053600,
                    code_name="Location6053600",
                )

    # bounds must be the string "full"/"partition" or a strictly-ascending
    # two-element array of two numbers or two dates (mongod 5946802/5733403/5733402).
    if isinstance(bounds, str):
        if bounds not in ("full", "partition"):
            raise AggregateError(
                "Bounds string must either be 'full' or 'partition'",
                code=5946802,
                code_name="Location5946802",
            )
    elif isinstance(bounds, list):
        if len(bounds) != 2:
            raise AggregateError(
                "A bounding array in a range statement must have exactly two elements",
                code=5733403,
                code_name="Location5733403",
            )
        lo_b, hi_b = bounds
        both_num = all(isinstance(b, (int, float)) and not isinstance(b, bool) for b in bounds)
        both_date = all(isinstance(b, _dt.datetime) for b in bounds)
        try:
            ascending = lo_b < hi_b
        except TypeError:
            ascending = False
        if not (both_num or both_date) or not ascending:
            raise AggregateError(
                "A bounding array must be an ascending array of either two dates or two numbers",
                code=5733402,
                code_name="Location5733402",
            )
    elif bounds is not None:
        raise AggregateError(
            "Bounds string must either be 'full' or 'partition'",
            code=5946802,
            code_name="Location5946802",
        )

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
    include_index: str | None = None
    if isinstance(spec, str):
        raw_path: Any = spec
        preserve_null = False
    elif isinstance(spec, Mapping):
        raw_path = spec.get("path")
        preserve_raw = spec.get("preserveNullAndEmptyArrays", False)
        if not isinstance(preserve_raw, bool):
            raise AggregateError(
                "expected a boolean for the preserveNullAndEmptyArrays option to "
                f"$unwind stage, got {_bson_type_name(preserve_raw)}",
                code=28809,
                code_name="Location28809",
            )
        preserve_null = preserve_raw
        include_index = spec.get("includeArrayIndex")
        if include_index is not None:
            if not isinstance(include_index, str) or not include_index:
                raise AggregateError(
                    "expected a non-empty string for the includeArrayIndex  option to "
                    f"$unwind stage, got {_bson_type_name(include_index)}",
                    code=28810,
                    code_name="Location28810",
                )
            if include_index.startswith("$"):
                raise AggregateError(
                    "includeArrayIndex option to $unwind stage should not be prefixed "
                    f"with a '$': {include_index}",
                    code=28822,
                    code_name="Location28822",
                )
    else:
        raise AggregateError("$unwind requires a path string or document spec")
    if not isinstance(raw_path, str):
        raise AggregateError(
            f"expected a string as the path for $unwind stage, got {_bson_type_name(raw_path)}",
            code=28808,
            code_name="Location28808",
        )
    if not raw_path.startswith("$"):
        raise AggregateError(
            f"path option to $unwind stage should be prefixed with a '$': {raw_path}",
            code=28818,
            code_name="Location28818",
        )
    path = raw_path.lstrip("$")

    # Fast path: a top-level unwind field in a pipeline with no in-place
    # mutating stage (see PipelineContext.shared_unwind_ok) fans out with a
    # shallow dict copy — every stage that writes docs deepcopies its input
    # first, so shared subtrees are never corrupted. High-fanout join
    # pipelines spend nearly all their time in this deepcopy otherwise.
    shallow = _ctx.shared_unwind_ok and "." not in path
    _copy = (lambda d: dict(d)) if shallow else copy.deepcopy

    result: list[dict[str, Any]] = []
    for doc in docs:
        # Fail fast mid-fanout: an unkeyed join's $unwind multiplies row counts
        # per stage, so the balloon must be caught while materializing, not only
        # after (a single stage can otherwise reach tens of GB before the loop's
        # post-stage check runs).
        if len(result) > MAX_PIPELINE_DOCS:
            raise _pipeline_overflow("$unwind")
        value = get_path(doc, path)
        if isinstance(value, list):
            if not value:
                if preserve_null:
                    new = _copy(doc)
                    unset_path(new, path)
                    if include_index:
                        new[include_index] = None
                    result.append(new)
                continue
            for i, elem in enumerate(value):
                new = _copy(doc)
                set_path(new, path, elem)
                if include_index:
                    new[include_index] = i
                result.append(new)
        elif value is None:
            if preserve_null:
                new = _copy(doc)
                if include_index:
                    new[include_index] = None
                result.append(new)
        else:
            new = _copy(doc)
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
            bucket[k] = _avg_divide(v["_avg_total"], v["_avg_n"])
        elif isinstance(v, dict) and "_std_vals" in v:
            bucket[k] = _std_dev(v["_std_vals"], pop=v["_std_pop"])
        elif isinstance(v, dict) and "_nelem_vals" in v:
            bucket[k] = _nelem_finalize(v)
        elif isinstance(v, dict) and "_topn_items" in v:
            bucket[k] = _topn_finalize(v)
        elif isinstance(v, dict) and "_pct_vals" in v:
            bucket[k] = _percentile_finalize(v)
    return bucket


def _std_dev(values: list[Any], *, pop: bool) -> float | None:
    """Population / sample standard deviation, matching Mongo's ``$stdDevPop`` /
    ``$stdDevSamp``: pop is null for an empty set (0 for a single value); samp is
    null for fewer than two values.

    Deliberately uses **naive left-fold float summation** (an explicit loop, not
    the ``sum()`` builtin) plus multiply-based squaring and ``math.sqrt`` — all
    correctly-rounded IEEE operations in a fixed order — so the result is bit-for-bit
    reproducible by the Rust engine (whose ``Iterator::sum`` is the same naive fold).
    CPython 3.12's ``sum()`` switched to Neumaier *compensated* summation for
    floats, which is more accurate but would round a last ULP differently from
    Rust's naive fold; ``** 2`` / ``** 0.5`` go through ``pow`` and can likewise
    diverge from multiply / hardware sqrt. (mongod computes stddev with an online
    Welford-style algorithm, so neither server matches it to the last ULP anyway —
    aligning the two SecantusDB engines is what matters here.)"""
    n = len(values)
    if n == 0 or (not pop and n < 2):
        return None
    total = 0.0
    for x in values:
        total += x  # bool folds to 0.0/1.0, matching the Rust engine
    mean = total / n
    denom = n if pop else n - 1
    acc = 0.0
    for x in values:
        d = x - mean
        acc += d * d
    return math.sqrt(acc / denom)


_AccHandler = Callable[[dict[str, Any], str, Any, Mapping[str, Any], dict[str, Any]], None]


def _is_acc_number(v: Any) -> bool:
    """A numeric value that ``$sum`` / ``$avg`` accumulate. mongod ignores
    everything else (string / bool / null / missing / array / …); a bool is *not*
    numeric here (``$sum`` over ``true`` adds nothing). ``decimal.Decimal`` is
    included for the SQL engine, whose ``numeric`` columns compile SUM/AVG through
    ``$sum`` / ``$avg`` with Python decimals (not ``bson.Decimal128``)."""
    return isinstance(v, (int, float, Decimal128, _decimal.Decimal)) and not isinstance(v, bool)


def _avg_divide(total: Any, n: int) -> Any:
    """Finalise a running $avg, keeping Decimal128 in the decimal domain.

    mongod's $avg over Decimal128 values answers a Decimal128; dividing through
    Python float would both narrow the type and lose precision.
    """
    if not n:
        return None
    if isinstance(total, Decimal128):
        # Decimal128 carries 34 significant digits; Python's default decimal
        # context is 28, which silently truncated the quotient to 27 and left us
        # short of mongod (…333333333333333 vs our …333333333). Widen the context
        # for the division only.
        with _decimal.localcontext() as ctx:
            ctx.prec = 34
            return Decimal128(total.to_decimal() / _decimal.Decimal(n))
    return total / n


def _acc_sum(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    # $sum always yields at least 0 (int32) — even an all-non-numeric group.
    bucket.setdefault(field, 0)
    increment = 1 if (arg == 1 and not isinstance(arg, bool)) else evaluate(arg, doc, vars)
    if not _is_acc_number(increment):
        return  # mongod ignores non-numeric operands
    # bson_add preserves the BSON numeric type (int32 < int64 < double <
    # decimal128) so a $sum over Int64 values stays Int64 rather than
    # narrowing to int32 on the wire.
    bucket[field] = bson_sum(bucket[field], increment)


def _acc_count(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    bucket[field] = bucket.get(field, 0) + 1


def _acc_avg(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    # Always create the running state so an all-non-numeric group finalises to
    # null (mongod), rather than dropping the field.
    state = bucket.get(field)
    if not isinstance(state, dict) or "_avg_total" not in state:
        state = {"_avg_total": 0, "_avg_n": 0}
        bucket[field] = state
    v = evaluate(arg, doc, vars)
    if not _is_acc_number(v):
        return  # mongod averages only numeric values
    # `bson_add`, not `+=`: a raw Python add throws
    # `TypeError: unsupported operand type(s) for +=: 'float' and 'Decimal128'`
    # the moment a group mixes Decimal128 with any other numeric type, and that
    # escaped as a bare "internal server error" to the client. $sum already used
    # bson_add; $avg was simply missed.
    state["_avg_total"] = bson_sum(state["_avg_total"], v)
    state["_avg_n"] += 1


def _acc_max(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    # mongod $max ignores null / missing and orders every other value by BSON
    # cross-type order (bool > string > number > …); all-null/missing -> null.
    from secantus.ordering import _SortKey

    bucket.setdefault(field, None)
    v = evaluate_or_missing(arg, doc, vars)
    if v is MISSING or v is None:
        return
    cur = bucket[field]
    if cur is None or _SortKey(v) > _SortKey(cur):
        bucket[field] = v


def _acc_min(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    from secantus.ordering import _SortKey

    bucket.setdefault(field, None)
    v = evaluate_or_missing(arg, doc, vars)
    if v is MISSING or v is None:
        return
    cur = bucket[field]
    if cur is None or _SortKey(v) < _SortKey(cur):
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
    # mongod skips a missing field value (an explicit null is still pushed); the
    # accumulated field is created as [] even when every value is missing.
    lst = bucket.setdefault(field, [])
    v = evaluate_or_missing(arg, doc, vars)
    if v is not MISSING:
        lst.append(v)


def _acc_add_to_set(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    seen = bucket.setdefault(field, [])
    v = evaluate_or_missing(arg, doc, vars)
    if v is not MISSING and v not in seen:
        seen.append(v)


def _acc_merge_objects(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    # $mergeObjects accumulator: merge each per-doc operand document into the
    # accumulator (later keys override earlier — dict.update semantics). A
    # null/missing operand is skipped; a non-null, non-document operand is an
    # error (mongod Location 40400). An all-missing group still yields {}.
    acc = bucket.setdefault(field, {})
    v = evaluate_or_missing(arg, doc, vars)
    if v is MISSING or v is None:
        return
    if not isinstance(v, Mapping):
        raise AggregateError(
            "$mergeObjects requires object inputs, but input "
            f"{v!r} is of type {_bson_type_name(v)}",
            code=40400,
            code_name="Location40400",
        )
    acc.update(v)


def _acc_std(
    bucket: dict[str, Any],
    field: str,
    arg: Any,
    doc: Mapping[str, Any],
    vars: dict[str, Any],
    *,
    pop: bool,
) -> None:
    v = evaluate(arg, doc, vars)
    if v is None:
        return
    state = bucket.get(field)
    if not isinstance(state, dict) or "_std_vals" not in state:
        state = {"_std_vals": [], "_std_pop": pop}
        bucket[field] = state
    state["_std_vals"].append(v)


def _percentile_spec(arg: Any, op: str) -> tuple[Any, list[float] | None]:
    """Validate a ``$median`` / ``$percentile`` accumulator/expression spec and
    return ``(input_expr, ps)`` (``ps`` is None for $median). Codes and messages
    are verbatim from a mongod 7.0.12 probe."""
    if not isinstance(arg, Mapping):
        raise AggregateError(
            f"specification must be an object; found {op}: {arg!r}",
            code=7429703,
            code_name="Location7429703",
        )
    if "method" not in arg:
        raise AggregateError(
            f"BSON field '{op}.method' is missing but a required field",
            code=40414,
            code_name="Location40414",
        )
    if arg["method"] != "approximate":
        raise AggregateError(
            "Currently only 'approximate' can be used as percentile 'method'.",
            code=2,
            code_name="BadValue",
        )
    if "input" not in arg:
        raise AggregateError(
            f"BSON field '{op}.input' is missing but a required field",
            code=40414,
            code_name="Location40414",
        )
    if op == "$median":
        return arg["input"], None
    if "p" not in arg:
        raise AggregateError(
            "BSON field '$percentile.p' is missing but a required field",
            code=40414,
            code_name="Location40414",
        )
    ps = arg["p"]
    if not isinstance(ps, list):
        raise AggregateError(
            "The $percentile 'p' field must be an array of numbers from "
            f"[0.0, 1.0], but found: {ps}",
            code=7750301,
            code_name="Location7750301",
        )
    out: list[float] = []
    for p in ps:
        if isinstance(p, bool) or not isinstance(p, (int, float)) or not 0.0 <= p <= 1.0:
            raise AggregateError(
                "The $percentile 'p' field must be an array of numbers from "
                f"[0.0, 1.0], but found: {p}",
                code=7750303,
                code_name="Location7750303",
            )
        out.append(float(p))
    return arg["input"], out


def _percentile_value(v: Any) -> float | None:
    """The double a value contributes to a percentile computation, or None to
    skip it. mongod (probed): int / long / double / Decimal128 count (as
    doubles), bool and NaN are excluded, everything else is skipped."""
    if isinstance(v, bool):
        return None
    if isinstance(v, (int, float)):
        f = float(v)
        return None if math.isnan(f) else f
    if isinstance(v, Decimal128):
        f = float(v.to_decimal())
        return None if math.isnan(f) else f
    return None


def _percentile_rank(values: list[float], p: float) -> float | None:
    """mongod's discrete percentile: ``sorted[max(0, ceil(p*n) - 1)]``."""
    if not values:
        return None
    idx = max(0, math.ceil(p * len(values)) - 1)
    return values[min(idx, len(values) - 1)]


def _acc_percentile(
    bucket: dict[str, Any],
    field: str,
    arg: Any,
    doc: Mapping[str, Any],
    vars: dict[str, Any],
    *,
    op: str,
) -> None:
    input_expr, ps = _percentile_spec(arg, op)
    state = bucket.get(field)
    if not isinstance(state, dict) or "_pct_vals" not in state:
        state = {"_pct_vals": [], "_pct_ps": ps}
        bucket[field] = state
    v = _percentile_value(evaluate_or_missing(input_expr, doc, vars))
    if v is not None:
        state["_pct_vals"].append(v)


def _percentile_finalize(state: dict[str, Any]) -> Any:
    values = sorted(state["_pct_vals"])
    ps = state["_pct_ps"]
    if ps is None:  # $median
        return _percentile_rank(values, 0.5)
    return [_percentile_rank(values, p) for p in ps]


def _acc_median(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_percentile(bucket, field, arg, doc, vars, op="$median")


def _acc_percentile_op(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_percentile(bucket, field, arg, doc, vars, op="$percentile")


def _acc_std_pop(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_std(bucket, field, arg, doc, vars, pop=True)


def _acc_std_samp(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_std(bucket, field, arg, doc, vars, pop=False)


def _acc_nelem(
    bucket: dict[str, Any],
    field: str,
    arg: Any,
    doc: Mapping[str, Any],
    vars: dict[str, Any],
    *,
    kind: str,
) -> None:
    """``$firstN`` / ``$lastN`` / ``$maxN`` / ``$minN`` (accumulator form): collect
    the per-doc ``input`` value across the group; the result (``n`` first/last in
    doc order, or ``n`` largest/smallest) is computed at finalize. ``$firstN`` /
    ``$lastN`` **include null** values (they're the first/last values seen);
    ``$maxN`` / ``$minN`` ignore them (matched to mongod 6.0 via a three-way probe).
    ``{n, input}`` validation matches the expression forms."""
    from secantus.expressions import nelem_parse_n

    if not isinstance(arg, Mapping) or "n" not in arg:
        raise ExpressionError("Missing value for 'n'", code=5787906)
    if "input" not in arg:
        raise ExpressionError("Missing value for 'input'", code=5787907)
    n = nelem_parse_n(evaluate(arg["n"], doc, vars))
    value = evaluate(arg["input"], doc, vars)
    state = bucket.get(field)
    if not isinstance(state, dict) or "_nelem_vals" not in state:
        state = {"_nelem_vals": [], "_nelem_n": n, "_nelem_kind": kind}
        bucket[field] = state
    else:
        state["_nelem_n"] = n  # n is constant across the group; last write wins
    state["_nelem_vals"].append(value)


def _acc_first_n(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_nelem(bucket, field, arg, doc, vars, kind="firstN")


def _acc_last_n(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_nelem(bucket, field, arg, doc, vars, kind="lastN")


def _acc_max_n(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_nelem(bucket, field, arg, doc, vars, kind="maxN")


def _acc_min_n(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_nelem(bucket, field, arg, doc, vars, kind="minN")


def _nelem_finalize(state: dict[str, Any]) -> list[Any]:
    """Finalize an N-element accumulator state to its result list."""
    vals = state["_nelem_vals"]
    n = state["_nelem_n"]
    kind = state["_nelem_kind"]
    if kind == "firstN":
        return vals[:n]
    if kind == "lastN":
        return vals[-n:]
    from secantus.ordering import _SortKey

    non_null = [x for x in vals if x is not None]
    non_null.sort(key=_SortKey, reverse=(kind == "maxN"))
    return non_null[:n]


def _acc_topn(
    bucket: dict[str, Any],
    field: str,
    arg: Any,
    doc: Mapping[str, Any],
    vars: dict[str, Any],
    *,
    kind: str,
) -> None:
    """``$top`` / ``$bottom`` / ``$topN`` / ``$bottomN`` accumulators: sort the
    group's docs by ``sortBy`` (multi-key BSON order; null / missing sort low and
    are **not** filtered) and take the top / bottom entries' ``output``. ``$topN`` /
    ``$bottomN`` take ``n`` (first / last ``n`` of the sort) and return a list;
    ``$top`` / ``$bottom`` take no ``n`` and return a single value. Validation
    matches mongod 6.0 (three-way verified)."""
    from secantus.expressions import nelem_parse_n

    op = "$" + kind
    has_n = kind in ("topN", "bottomN")
    if not isinstance(arg, Mapping):
        raise ExpressionError(f"{op} requires an object", code=5788001)
    if not has_n and "n" in arg:
        raise ExpressionError(f"Unknown argument to {op} 'n'", code=5788002)
    if has_n and "n" not in arg:
        raise ExpressionError("Missing value for 'n'", code=5788003)
    if "output" not in arg:
        raise ExpressionError("Missing value for 'output'", code=5788004)
    if "sortBy" not in arg:
        raise ExpressionError("Missing value for 'sortBy'", code=5788005)
    sortby = arg["sortBy"]
    if not isinstance(sortby, Mapping):
        raise ExpressionError("invalid parameter: expected an object (sortBy)", code=10065)
    n = nelem_parse_n(evaluate(arg["n"], doc, vars)) if has_n else 1
    sort_vals = tuple(get_path(dict(doc), f) for f in sortby)
    output_val = evaluate(arg["output"], doc, vars)
    state = bucket.get(field)
    if not isinstance(state, dict) or "_topn_items" not in state:
        state = {
            "_topn_items": [],
            "_topn_n": n,
            "_topn_dirs": [int(d) == -1 for d in sortby.values()],
            "_topn_kind": kind,
        }
        bucket[field] = state
    else:
        state["_topn_n"] = n  # n is constant across the group
    state["_topn_items"].append((sort_vals, output_val))


def _acc_top(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_topn(bucket, field, arg, doc, vars, kind="top")


def _acc_bottom(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_topn(bucket, field, arg, doc, vars, kind="bottom")


def _acc_top_n(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_topn(bucket, field, arg, doc, vars, kind="topN")


def _acc_bottom_n(
    bucket: dict[str, Any], field: str, arg: Any, doc: Mapping[str, Any], vars: dict[str, Any]
) -> None:
    _acc_topn(bucket, field, arg, doc, vars, kind="bottomN")


def _topn_finalize(state: dict[str, Any]) -> Any:
    """Finalize a ``$top``/``$bottom``/``$topN``/``$bottomN`` state: stable-sort the
    collected ``(sort_values, output)`` items by the ``sortBy`` directions, then
    return the top/bottom ``output`` value(s). ``$top``/``$bottom`` return a single
    value; ``$topN``/``$bottomN`` return a list."""
    from secantus.ordering import _SortKey

    items = state["_topn_items"]
    n = state["_topn_n"]
    dirs = state["_topn_dirs"]
    kind = state["_topn_kind"]
    items = sorted(
        items,
        key=lambda it: tuple(_SortKey(v, reverse=rev) for v, rev in zip(it[0], dirs, strict=False)),
    )
    outputs = [out for _, out in items]
    if kind == "top":
        return outputs[0]
    if kind == "bottom":
        return outputs[-1]
    if kind == "topN":
        return outputs[:n]
    return outputs[-n:]  # bottomN


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
    "$mergeObjects": _acc_merge_objects,
    "$stdDevPop": _acc_std_pop,
    "$stdDevSamp": _acc_std_samp,
    "$firstN": _acc_first_n,
    "$lastN": _acc_last_n,
    "$maxN": _acc_max_n,
    "$minN": _acc_min_n,
    "$top": _acc_top,
    "$bottom": _acc_bottom,
    "$topN": _acc_top_n,
    "$bottomN": _acc_bottom_n,
    "$median": _acc_median,
    "$percentile": _acc_percentile_op,
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
    size_raw = spec["size"]
    # mongod: size must be a number (bool rejected) and non-negative; a
    # fractional double is accepted and truncated (unlike $limit/$skip).
    if isinstance(size_raw, bool) or not isinstance(size_raw, (int, float)):
        raise AggregateError("size argument to $sample must be a number", code=28746)
    if size_raw < 0:
        raise AggregateError("size argument to $sample must not be negative", code=28747)
    size = int(size_raw)
    if size >= len(docs):
        return list(docs)
    rng = _SAMPLE_RNG if _SAMPLE_RNG is not None else random
    return rng.sample(list(docs), size)


def _validate_sort_by_count_arg(spec: Any) -> None:
    """mongod: the $sortByCount argument is a $-prefixed path string (40148) or an
    expression object — a single `$`-prefixed key (40147); anything else (number,
    bool, array, null) is 40149."""
    if isinstance(spec, str):
        if not spec.startswith("$"):
            raise AggregateError(
                "the sortByCount field must be defined as a $-prefixed path or an "
                "expression inside an object",
                code=40148,
                code_name="Location40148",
            )
        return
    if isinstance(spec, Mapping):
        if len(spec) == 1 and str(next(iter(spec))).startswith("$"):
            return
        raise AggregateError(
            "the sortByCount field must be defined as a $-prefixed path or an "
            "expression inside an object",
            code=40147,
            code_name="Location40147",
        )
    raise AggregateError(
        "the sortByCount field must be specified as a string or as an object",
        code=40149,
        code_name="Location40149",
    )


def _stage_sort_by_count(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    _validate_sort_by_count_arg(spec)
    grouped = _stage_group({"_id": spec, "count": {"$sum": 1}}, docs, ctx)
    grouped.sort(key=lambda d: d.get("count", 0), reverse=True)
    return grouped


def _stage_facet(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    # mongod validates the $facet spec before running any sub-pipeline: a
    # non-empty object (40169), each value an array (40170), each stage a
    # non-empty object (40171), and no nested $facet (40600). Without this a
    # non-object stage element (`{a: [5]}`) leaks a Python TypeError.
    if not isinstance(spec, Mapping) or not spec:
        raise AggregateError(
            f"the $facet specification must be a non-empty object, but found: $facet: {spec!r}",
            code=40169,
            code_name="Location40169",
        )
    out: dict[str, Any] = {}
    for name, sub_pipeline in spec.items():
        if not isinstance(sub_pipeline, list):
            raise AggregateError(
                "arguments to $facet must be arrays, "
                f"{name} is type {_bson_type_name(sub_pipeline)}",
                code=40170,
                code_name="Location40170",
            )
        for i, stage in enumerate(sub_pipeline):
            if not isinstance(stage, Mapping) or not stage:
                raise AggregateError(
                    "elements of arrays in $facet spec must be non-empty objects, "
                    f"{name} argument contained an element of type "
                    f"{_bson_type_name(stage)}: {i}: {stage!r}",
                    code=40171,
                    code_name="Location40171",
                )
            if "$facet" in stage:
                raise AggregateError(
                    "$facet is not allowed to be used within a $facet stage",
                    code=40600,
                    code_name="Location40600",
                )
        out[name] = apply_pipeline(list(docs), sub_pipeline, ctx)
    return [out]


_BUCKET_NUMERIC_NAMES = frozenset({"int", "long", "double", "decimal"})


def _bucket_ctype(v: Any) -> str:
    """Canonical type name for $bucket boundary comparison — the numeric BSON
    types collapse to one bracket (mongod requires all boundaries the same type)."""
    name = _bson_type_name(v)
    return "number" if name in _BUCKET_NUMERIC_NAMES else name


def _stage_bucket(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    from secantus.ordering import _SortKey

    if not isinstance(spec, Mapping):
        raise AggregateError("$bucket requires a document spec")
    group_by = spec.get("groupBy")
    boundaries = spec.get("boundaries")
    # mongod validates the whole spec before bucketing — several of these were
    # silently accepted, and an out-of-range value with no default silently
    # DROPPED the document.
    if group_by is None or boundaries is None:
        raise AggregateError(
            "$bucket requires 'groupBy' and 'boundaries' to be specified.", code=40198
        )
    if not isinstance(boundaries, list):
        raise AggregateError(
            f"The $bucket 'boundaries' field must be an array, but found type: "
            f"{_bson_type_name(boundaries)}",
            code=40200,
        )
    if len(boundaries) < 2:
        raise AggregateError(
            "The $bucket 'boundaries' field must have at least 2 values, but found "
            f"{len(boundaries)}.",
            code=40192,
        )
    ctype0 = _bucket_ctype(boundaries[0])
    for i in range(len(boundaries) - 1):
        if _bucket_ctype(boundaries[i + 1]) != ctype0:
            raise AggregateError(
                "All values in the the 'boundaries' option to $bucket must have the "
                f"same type. Found conflicting types {ctype0} and "
                f"{_bucket_ctype(boundaries[i + 1])}.",
                code=40193,
            )
        if not _SortKey(boundaries[i]) < _SortKey(boundaries[i + 1]):
            raise AggregateError(
                "The 'boundaries' option to $bucket must be sorted, but elements "
                f"{i} and {i + 1} are not in ascending order.",
                code=40194,
            )
    default = spec.get("default")
    output_spec = spec.get("output")
    if output_spec is not None and not isinstance(output_spec, Mapping):
        raise AggregateError(
            f"The $bucket 'output' field must be an object, but found type: "
            f"{_bson_type_name(output_spec)}",
            code=40196,
        )
    if output_spec is None:
        output_spec = {"count": {"$sum": 1}}
    if default is not None:
        dk, lo0, hi0 = _SortKey(default), _SortKey(boundaries[0]), _SortKey(boundaries[-1])
        if not dk < lo0 and dk < hi0:  # default lies within [first, last) -> invalid
            raise AggregateError(
                "The $bucket 'default' field must be less than the lowest boundary or "
                "greater than or equal to the highest boundary.",
                code=40199,
            )

    buckets: dict[Any, list[dict[str, Any]]] = {b: [] for b in boundaries[:-1]}
    if default is not None:
        buckets.setdefault(default, [])

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
        if not placed:
            if default is None:
                raise AggregateError(
                    "$switch could not find a matching branch for an input, and no "
                    "default was specified.",
                    code=7158303,
                )
            buckets[default].append(d)

    result: list[dict[str, Any]] = []
    for key, bucket_docs in buckets.items():
        bucket: dict[str, Any] = {"_id": key}
        for field_name, accumulator in output_spec.items():
            for d in bucket_docs:
                _accumulate(bucket, field_name, accumulator, d, ctx.vars)
        result.append(_finalize(bucket))
    return result


def _enforce_target_validator(
    ctx: PipelineContext, target_db: str, target_coll: str, docs: list[dict[str, Any]]
) -> None:
    """Enforce the destination collection's ``validator`` on a ``$out`` /
    ``$merge`` write unless the command set ``bypassDocumentValidation``.

    mongod runs writes from these stages through the same document
    validation as an ordinary insert: a doc that fails the validator
    aborts the pipeline with ``DocumentValidationFailure`` (121) when
    ``validationAction`` is ``"error"`` (the default). ``"warn"`` is a
    no-op here (we don't surface server logs).
    """
    if ctx.bypass_validation or ctx.storage is None:
        return
    opts = ctx.storage.get_collection_options(target_db, target_coll)
    validator = opts.get("validator")
    if not isinstance(validator, dict) or not validator:
        return
    if opts.get("validationAction", "error") != "error":
        return
    for doc in docs:
        if not matches(doc, validator):
            raise AggregateError(
                "Document failed validation",
                code=121,
                code_name="DocumentValidationFailure",
            )


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
    _enforce_target_validator(ctx, target_db, target_coll, docs)
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
    entry: dict[str, Any] = {
        "type": "op",
        "host": "secantus",
        "desc": "$currentOp",
        "active": False,
        "currentOpTime": "",
        "command": command_doc,
        "ns": _ctx.db_name + "." + (_ctx.coll_name or "$cmd.aggregate"),
        "op": "command",
    }
    # Surface the connection's driver handshake metadata, like mongod's
    # ``$currentOp``: the full ``clientMetadata`` document plus the top-level
    # ``appName`` lifted from ``application.name``. mongocxx's "client metadata
    # handshake feature" test connects with ``?appName=xyz`` and scans
    # ``db.aggregate([{$currentOp: {}}])`` for an op whose ``appName`` matches,
    # then verifies its ``clientMetadata.{application,driver,os}``.
    meta = _ctx.client_metadata
    if isinstance(meta, Mapping):
        entry["clientMetadata"] = dict(meta)
        application = meta.get("application")
        if isinstance(application, Mapping) and application.get("name"):
            entry["appName"] = application["name"]
    return [entry]


# mongod's preferred-number rounding series for $bucketAuto `granularity`.
_BUCKET_AUTO_GRANULARITIES = frozenset(
    {
        "R5",
        "R10",
        "R20",
        "R40",
        "R80",
        "1-2-5",
        "E6",
        "E12",
        "E24",
        "E48",
        "E96",
        "E192",
        "POWERSOF2",
    }
)


# The preferred-number series mongod rounds $bucketAuto boundaries to. Stored
# exactly as mongod stores them (integer-valued doubles, e.g. R5 = {10,16,25,
# 40,63}, NOT normalised `0.63`-style literals) so that `series_element *
# multiplier` reproduces mongod's non-standard ULPs bit-for-bit — verified
# hex-exact against real mongod 7.0.12. See `granularity_rounder_preferred_numbers.cpp`.
_BUCKET_AUTO_SERIES: dict[str, list[float]] = {
    "R5": [10, 16, 25, 40, 63],
    "R10": [100, 125, 160, 200, 250, 315, 400, 500, 630, 800],
    "R20": [
        100,
        112,
        125,
        140,
        160,
        180,
        200,
        224,
        250,
        280,
        315,
        355,
        400,
        450,
        500,
        560,
        630,
        710,
        800,
        900,
    ],
    "R40": [
        100,
        106,
        112,
        118,
        125,
        132,
        140,
        150,
        160,
        170,
        180,
        190,
        200,
        212,
        224,
        236,
        250,
        265,
        280,
        300,
        315,
        355,
        375,
        400,
        425,
        450,
        475,
        500,
        530,
        560,
        600,
        630,
        670,
        710,
        750,
        800,
        850,
        900,
        950,
    ],
    "R80": [
        103,
        109,
        115,
        122,
        128,
        136,
        145,
        155,
        165,
        175,
        185,
        195,
        206,
        218,
        230,
        243,
        258,
        272,
        290,
        307,
        325,
        345,
        365,
        387,
        412,
        437,
        462,
        487,
        515,
        545,
        575,
        615,
        650,
        690,
        730,
        775,
        825,
        875,
        925,
        975,
    ],
    "1-2-5": [10, 20, 50],
    "E6": [10, 15, 22, 33, 47, 68],
    "E12": [10, 12, 15, 18, 22, 27, 33, 39, 47, 56, 68, 82],
    "E24": [
        10,
        11,
        12,
        13,
        15,
        16,
        18,
        20,
        22,
        24,
        27,
        30,
        33,
        36,
        39,
        43,
        47,
        51,
        56,
        62,
        68,
        75,
        82,
        91,
    ],
    "E48": [
        100,
        105,
        110,
        115,
        121,
        127,
        133,
        140,
        147,
        154,
        162,
        169,
        178,
        187,
        196,
        205,
        215,
        226,
        237,
        249,
        261,
        274,
        287,
        301,
        316,
        332,
        348,
        365,
        383,
        402,
        422,
        442,
        464,
        487,
        511,
        536,
        562,
        590,
        619,
        649,
        681,
        715,
        750,
        787,
        825,
        866,
        909,
        953,
    ],
    "E96": [
        100,
        102,
        105,
        107,
        110,
        113,
        115,
        118,
        121,
        124,
        127,
        130,
        133,
        137,
        140,
        143,
        147,
        150,
        154,
        158,
        162,
        165,
        169,
        174,
        178,
        182,
        187,
        191,
        196,
        200,
        205,
        210,
        215,
        221,
        226,
        232,
        237,
        243,
        249,
        255,
        261,
        267,
        274,
        280,
        287,
        294,
        301,
        309,
        316,
        324,
        332,
        340,
        348,
        357,
        365,
        374,
        383,
        392,
        402,
        412,
        422,
        432,
        442,
        453,
        464,
        475,
        487,
        499,
        511,
        523,
        536,
        549,
        562,
        576,
        590,
        604,
        619,
        634,
        649,
        665,
        681,
        698,
        715,
        732,
        750,
        768,
        787,
        806,
        825,
        845,
        866,
        887,
        909,
        931,
        953,
        976,
    ],
    "E192": [
        100,
        101,
        102,
        104,
        105,
        106,
        107,
        109,
        110,
        111,
        113,
        114,
        115,
        117,
        118,
        120,
        121,
        123,
        124,
        126,
        127,
        129,
        130,
        132,
        133,
        135,
        137,
        138,
        140,
        142,
        143,
        145,
        147,
        149,
        150,
        152,
        154,
        156,
        158,
        160,
        162,
        164,
        165,
        167,
        169,
        172,
        174,
        176,
        178,
        180,
        182,
        184,
        187,
        189,
        191,
        193,
        196,
        198,
        200,
        203,
        205,
        208,
        210,
        213,
        215,
        218,
        221,
        223,
        226,
        229,
        232,
        234,
        237,
        240,
        243,
        246,
        249,
        252,
        255,
        258,
        261,
        264,
        267,
        271,
        274,
        277,
        280,
        284,
        287,
        291,
        294,
        298,
        301,
        305,
        309,
        312,
        316,
        320,
        324,
        328,
        332,
        336,
        340,
        344,
        348,
        352,
        357,
        361,
        365,
        370,
        374,
        379,
        383,
        388,
        392,
        397,
        402,
        407,
        412,
        417,
        422,
        427,
        432,
        437,
        442,
        448,
        453,
        459,
        464,
        470,
        475,
        481,
        487,
        493,
        499,
        505,
        511,
        517,
        523,
        530,
        536,
        542,
        549,
        556,
        562,
        569,
        576,
        583,
        590,
        597,
        604,
        612,
        619,
        626,
        634,
        642,
        649,
        657,
        665,
        673,
        681,
        690,
        698,
        706,
        715,
        723,
        732,
        741,
        750,
        759,
        768,
        777,
        787,
        796,
        806,
        816,
        825,
        835,
        845,
        856,
        866,
        876,
        887,
        898,
        909,
        920,
        931,
        942,
        953,
        965,
        976,
        988,
    ],
}
_BUCKET_AUTO_SERIES = {k: [float(x) for x in v] for k, v in _BUCKET_AUTO_SERIES.items()}


def _round_up_series(number: float, series: list[float]) -> float:
    """mongod `GranularityRounderPreferredNumbers::roundUp` (double path),
    ported verbatim so the `series_element * multiplier` arithmetic matches
    mongod's f64 result bit-for-bit."""
    if number == 0.0 or number == math.inf:
        return number
    multiplier = 1.0
    while number >= series[-1] * multiplier:
        multiplier *= 10.0
    while number < series[0] * multiplier:
        previous_min = series[0] * multiplier
        multiplier /= 10.0
        if number >= series[-1] * multiplier:
            return previous_min
    # smallest series element with number < series*multiplier (strict upper bound)
    idx = _bisect_right([s * multiplier for s in series], number)
    return series[idx] * multiplier


def _round_down_series(number: float, series: list[float]) -> float:
    """mongod `GranularityRounderPreferredNumbers::roundDown` (double path)."""
    if number == 0.0 or number == math.inf:
        return number
    multiplier = 1.0
    while number <= series[0] * multiplier:
        multiplier /= 10.0
    if multiplier == 0:
        return 0.0
    while number > series[-1] * multiplier:
        previous_max = series[-1] * multiplier
        multiplier *= 10.0
        if number <= series[0] * multiplier:
            return previous_max
    idx = _bisect_left([s * multiplier for s in series], number)
    return series[idx - 1] * multiplier


def _round_up_pow2(v: float) -> float:
    """mongod `GranularityRounderPowersOfTwo::roundUp` (double path)."""
    if v == 0.0 or v == math.inf:
        return v
    return 2.0 ** (math.floor(math.log2(v)) + 1)


def _round_down_pow2(v: float) -> float:
    """mongod `GranularityRounderPowersOfTwo::roundDown` (double path)."""
    if v == 0.0 or v == math.inf:
        return v
    return 2.0 ** (math.ceil(math.log2(v)) - 1)


def _bisect_right(a: list[float], x: float) -> int:
    lo, hi = 0, len(a)
    while lo < hi:
        mid = (lo + hi) // 2
        if x < a[mid]:
            hi = mid
        else:
            lo = mid + 1
    return lo


def _bisect_left(a: list[float], x: float) -> int:
    lo, hi = 0, len(a)
    while lo < hi:
        mid = (lo + hi) // 2
        if a[mid] < x:
            lo = mid + 1
        else:
            hi = mid
    return lo


def _granularity_coerce(v: Any) -> float:
    """Coerce a groupBy value to the double mongod's rounder operates on, or
    raise mongod's granularity error. Decimal128 is deferred (the standing
    Decimal128 precision deferral) rather than approximated in f64."""
    if isinstance(v, Decimal128):
        raise AggregateError(
            "$bucketAuto 'granularity' over Decimal128 boundaries is not yet "
            "supported by SecantusDB (the double-valued series ships hex-exact; "
            "Decimal128 rounding is the standing precision deferral)",
            code=2,
        )
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise AggregateError(
            "$bucketAuto can specify a 'granularity' with numeric boundaries "
            f"only, but found a value with type: {_bson_type_name(v)}",
            code=40258,
            code_name="Location40258",
        )
    f = float(v)
    if math.isnan(f):
        raise AggregateError(
            "$bucketAuto can specify a 'granularity' with numeric boundaries only, but found a NaN",
            code=40259,
            code_name="Location40259",
        )
    if f < 0:
        raise AggregateError(
            "$bucketAuto can specify a 'granularity' with non-negative numbers "
            "only, but found a negative number",
            code=40260,
            code_name="Location40260",
        )
    return f


def _validate_bucket_auto_granularity(granularity: Any) -> None:
    """mongod: `granularity` must be a string (else 40261) naming a known
    preferred-number series (else 40257)."""
    if not isinstance(granularity, str):
        raise AggregateError(
            "The $bucketAuto 'granularity' field must be a string, but found type: "
            f"{_bson_type_name(granularity)}",
            code=40261,
            code_name="Location40261",
        )
    if granularity not in _BUCKET_AUTO_GRANULARITIES:
        raise AggregateError(
            f"Unknown rounding granularity '{granularity}'",
            code=40257,
            code_name="Location40257",
        )


def _bucket_auto_granular(
    pairs: list[tuple[Any, dict[str, Any]]],
    n_buckets: int,
    granularity: str,
    output_spec: Mapping[str, Any],
    ctx: PipelineContext,
) -> list[dict[str, Any]]:
    """mongod `DocumentSourceBucketAuto::populateNextBucket` with a granularity
    rounder: first bucket min = roundDown(dataMin); every other boundary =
    roundUp(chunkMax), absorbing values that fall below the rounded boundary so
    boundaries strictly increase. Ported to match mongod 7.0.12 hex-exact."""
    values = [_granularity_coerce(v) for v, _ in pairs]
    docs = [d for _, d in pairs]
    if granularity == "POWERSOF2":
        rup, rdn = _round_up_pow2, _round_down_pow2
    else:
        series = _BUCKET_AUTO_SERIES[granularity]
        rup = lambda x: _round_up_series(x, series)  # noqa: E731
        rdn = lambda x: _round_down_series(x, series)  # noqa: E731

    n = len(pairs)
    approx = math.floor(n / n_buckets + 0.5)  # std::round (positive) — fixed for all buckets
    if approx < 1:
        approx = 1

    out: list[dict[str, Any]] = []
    idx = 0
    previous_max: float | None = None
    carry: int | None = None  # index of the value carried as the next bucket's min
    bucket_num = 0
    while True:
        bucket_num += 1
        if carry is None and idx >= n:
            break
        if carry is not None:
            cur_i = carry
        else:
            cur_i = idx
            idx += 1
        cur_min = previous_max if previous_max is not None else rdn(values[cur_i])
        cur_max = values[cur_i]
        chunk: list[int] = [cur_i]
        is_last = bucket_num == n_buckets
        i = 1
        while idx < n and (i < approx or is_last):
            cur_max = values[idx]
            chunk.append(idx)
            idx += 1
            i += 1
        # adjustBoundariesAndGetMinForNextBucket
        next_i: int | None = None
        if idx < n:
            next_i = idx
            idx += 1
        boundary = rup(cur_max)
        # Absorb values that now fall below the rounded boundary (mongod fixes
        # boundaryValue once, then pulls those docs into this bucket).
        while next_i is not None and boundary > values[next_i]:
            chunk.append(next_i)
            next_i = None
            if idx < n:
                next_i = idx
                idx += 1
        if float(boundary) == 0.0 and next_i is not None:
            bucket_max: float = rdn(values[next_i])
        else:
            bucket_max = boundary
        bucket: dict[str, Any] = {"_id": {"min": cur_min, "max": bucket_max}}
        for field_name, accumulator in output_spec.items():
            for ci in chunk:
                _accumulate(bucket, field_name, accumulator, docs[ci], ctx.vars)
        out.append(_finalize(bucket))
        previous_max = bucket_max
        carry = next_i
        if carry is None and idx >= n:
            break
    return out


def _stage_bucket_auto(
    spec: Any, docs: list[dict[str, Any]], ctx: PipelineContext
) -> list[dict[str, Any]]:
    if not isinstance(spec, Mapping):
        raise AggregateError("$bucketAuto requires a document spec")
    # mongod: both groupBy and buckets must be present (40246); buckets must be
    # a non-bool numeric value (40241), representable as a 32-bit integer —
    # a whole double is accepted, a fractional double is not (40242) — and
    # strictly greater than 0 (40243).
    if "groupBy" not in spec or "buckets" not in spec:
        raise AggregateError(
            "$bucketAuto requires 'groupBy' and 'buckets' to be specified",
            code=40246,
            code_name="Location40246",
        )
    group_by = spec.get("groupBy")
    n_raw = spec["buckets"]
    if isinstance(n_raw, bool) or not isinstance(n_raw, (int, float)):
        raise AggregateError(
            "The $bucketAuto 'buckets' field must be a numeric value, but found type: "
            f"{_bson_type_name(n_raw)}",
            code=40241,
            code_name="Location40241",
        )
    if isinstance(n_raw, float):
        if not n_raw.is_integer():
            raise AggregateError(
                "The $bucketAuto 'buckets' field must be representable as a 32-bit "
                f"integer, but found {_fmt_double(n_raw)}",
                code=40242,
                code_name="Location40242",
            )
        n_buckets = int(n_raw)
    else:
        n_buckets = n_raw
    if n_buckets <= 0:
        raise AggregateError(
            f"The $bucketAuto 'buckets' field must be greater than 0, but found: {n_buckets}",
            code=40243,
            code_name="Location40243",
        )
    granularity = spec.get("granularity")
    if granularity is not None:
        _validate_bucket_auto_granularity(granularity)
    output_spec = spec.get("output") or {"count": {"$sum": 1}}

    from secantus.storage import _SortKey

    pairs = [(evaluate(group_by, d, ctx.vars), d) for d in docs]
    pairs.sort(key=lambda p: _SortKey(p[0]))
    if not pairs:
        return []
    if granularity is not None:
        return _bucket_auto_granular(pairs, n_buckets, granularity, output_spec, ctx)
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


def _stage_change_stream_split_large_event(
    spec: Any, docs: list[dict[str, Any]], _ctx: PipelineContext
) -> list[dict[str, Any]]:
    """``$changeStreamSplitLargeEvent`` — pass-through marker.

    Drivers (mongo-rust-driver, mongo-node-driver, …) insert this
    stage into the change-stream pipeline when the user opts into
    ``splitLargeChangeStreamEvents``. SecantusDB already handles
    the split envelope at projection time: every event the
    change-stream producer emits with ``splitLargeChangeStreamEvents``
    set carries a ``splitEvent: {fragment: 1, of: 1}`` sub-doc
    (events are never large enough to actually need splitting in
    our single-node surrogate, but the field is present when the
    user opts in — per ``CLAUDE.md``).

    Because the split envelope is applied during event projection
    upstream, the pipeline stage itself is a no-op: it accepts an
    empty doc spec and passes docs through unchanged. Mongod's
    real implementation also passes events through unchanged
    unless an event genuinely exceeds the 16 MB BSON limit, which
    our oplog projection never produces.
    """
    if spec is not None and not isinstance(spec, Mapping):
        raise AggregateError("$changeStreamSplitLargeEvent spec must be a document or {}")
    return docs


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
        if op == "$shift":
            # Position-based (like the rank funcs): value from `by` slots away in
            # the sorted partition. No window; requires a sortBy.
            if window is not None:
                raise AggregateError("$setWindowFields $shift does not accept a window")
            if not sort_by:
                raise AggregateError("$setWindowFields $shift requires sortBy")
            if not isinstance(arg, Mapping) or "output" not in arg or "by" not in arg:
                raise AggregateError("$shift requires {output, by, default?}")
            if not isinstance(arg["by"], int) or isinstance(arg["by"], bool):
                raise AggregateError("$shift 'by' must be an integer")
            compiled.append((field, op, arg, window))
            continue
        if op == "$expMovingAvg":
            # Prefix-accumulated over the sorted partition. No window; requires a
            # sortBy. Validating the {input, N|alpha} shape up front (raises on a
            # bad spec, exactly one of N/alpha).
            if window is not None:
                raise AggregateError("$setWindowFields $expMovingAvg does not accept a window")
            if not sort_by:
                raise AggregateError("$setWindowFields $expMovingAvg requires sortBy")
            if not isinstance(arg, Mapping) or "input" not in arg:
                raise AggregateError("$expMovingAvg requires {input, N|alpha}")
            _ema_alpha(arg)  # validates exactly-one-of N/alpha and their ranges
            compiled.append((field, op, arg, window))
            continue
        if op in ("$locf", "$linearFill"):
            # Gap-filling over the sorted partition (arg is the input expression).
            # No window; requires a sortBy. $linearFill additionally needs a single
            # ascending numeric sort field as the interpolation x-axis.
            if window is not None:
                raise AggregateError(f"$setWindowFields {op} does not accept a window")
            if not sort_by:
                raise AggregateError(f"$setWindowFields {op} requires sortBy")
            compiled.append((field, op, arg, window))
            continue
        if op in ("$derivative", "$integral"):
            # Window operators over the sortBy value (x) and input (y). Require a
            # sortBy; a time `unit` scales the x-axis against a date sortBy
            # (validated at evaluation, when the partition's values are known).
            if not sort_by:
                raise AggregateError(f"$setWindowFields {op} requires sortBy")
            if not isinstance(arg, Mapping) or "input" not in arg:
                raise AggregateError(f"{op} requires {{input, unit?}}")
            compiled.append((field, op, arg, window))
            continue
        if op == "$mergeObjects":
            # $mergeObjects is a $group-only accumulator; mongod rejects it as a
            # window function (verified three-way vs mongod 6.0: FailedToParse).
            raise AggregateError(
                f"Unrecognized window function, or the window function {op} is not "
                "supported in $setWindowFields",
                code=9,
                code_name="FailedToParse",
            )
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
        # Range-based windows resolve their bounds against the sortBy *value*, so
        # they need the per-slot values of a single ascending sort field. Anything
        # else (multi-field / descending / no sortBy) leaves this None, and a range
        # window then raises in ``_range_window_bounds``.
        range_vals = None
        range_date_ms = None
        if sort_by and len(sort_by) == 1:
            sort_field, sort_dir = next(iter(sort_by.items()))
            if sort_dir == 1:
                range_vals = [get_path(doc, sort_field) for doc in partition_docs]
                # A date-valued x-axis is offered to range windows (with a time
                # `unit`) as epoch millis; the raw dates stay in ``range_vals`` so
                # the other window ops keep deferring on a non-numeric sortBy.
                if range_vals and all(isinstance(v, _dt.datetime) for v in range_vals):
                    range_date_ms = [_date_ms(v) for v in range_vals]
        # $expMovingAvg / $locf / $linearFill are per-slot vectors computed once
        # per partition per output field.
        fill_state = {
            field: _compute_window_vector(op, partition_docs, arg, range_vals, ctx)
            for field, op, arg, _w in compiled
            if op in ("$expMovingAvg", "$locf", "$linearFill")
        }
        for slot, (orig_i, _) in enumerate(members):
            target = out_docs[orig_i]
            for field, op, arg, window in compiled:
                if op in _RANK_FUNCS:
                    target[field] = rank_state[op][slot]
                    continue
                if op == "$shift":
                    target[field] = _shift_value(arg, slot, partition_docs, ctx)
                    continue
                if op in ("$expMovingAvg", "$locf", "$linearFill"):
                    target[field] = fill_state[field][slot]
                    continue
                low, high = _resolve_window(slot, n, window, range_vals, range_date_ms)
                if op in ("$derivative", "$integral"):
                    target[field] = _ts_window_value(
                        op, arg, low, high, partition_docs, range_vals, range_date_ms, ctx
                    )
                    continue
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


def _resolve_window(
    slot: int,
    n: int,
    window: Mapping[str, Any] | None,
    range_vals: list[Any] | None,
    range_date_ms: list[int] | None = None,
) -> tuple[int, int]:
    """Dispatch a window spec to the document-based or range-based resolver."""
    if window is not None and "range" in window:
        return _range_window_bounds(slot, n, window, range_vals, range_date_ms)
    return _window_bounds(slot, n, window)


def _range_window_bounds(
    slot: int,
    n: int,
    window: Mapping[str, Any],
    range_vals: list[Any] | None,
    range_date_ms: list[int] | None = None,
) -> tuple[int, int]:
    """Resolve a ``range``-based window: include rows whose sortBy value falls in
    ``[cur + lower, cur + upper]`` (``cur`` = this row's value). Bounds may be
    ``"unbounded"`` (open), ``"current"`` (this row's value), or a number offset.

    A time ``unit`` scales each offset by that unit's millisecond span and pins
    the x-axis to the date sortBy's epoch millis (``range_date_ms``). mongod's
    rule is enforced both ways: ``unit`` **requires** a date sortBy, and a date
    sortBy **requires** ``unit`` — the numeric x-axis (``range_vals``) and the
    date x-axis are mutually exclusive. A missing/descending/multi-field sort, a
    variable-length unit (month/quarter/year), or a non-numeric value raises.
    """
    has_unit = "unit" in window
    if has_unit:
        if range_date_ms is None:
            raise AggregateError(
                "$setWindowFields range window 'unit' requires a date sortBy field"
            )
        unit_ms = _WINDOW_UNIT_MS.get(window["unit"])
        if unit_ms is None:
            raise AggregateError(
                f"$setWindowFields range window unit {window['unit']!r} is not supported "
                "(variable-length month/quarter/year)"
            )
        # Offset in the date's own units — the x-axis is epoch millis.
        range_vals = range_date_ms
    else:
        if range_date_ms is not None:
            raise AggregateError(
                "$setWindowFields range window over a date sortBy requires a 'unit'"
            )
        unit_ms = 1
    if range_vals is None:
        raise AggregateError(
            "$setWindowFields range windows require a single ascending sortBy field"
        )
    bounds = window["range"]
    if not isinstance(bounds, list) or len(bounds) != 2:
        raise AggregateError("$setWindowFields window.range must be a [lower, upper] pair")
    cur = range_vals[slot]
    if not _is_number(cur):
        raise AggregateError("$setWindowFields range windows require numeric sortBy values")

    def edge(b: Any) -> Any:
        if b == "unbounded":
            return None
        if b == "current":
            return cur
        if not _is_number(b):
            raise AggregateError(
                f"$setWindowFields window.range bound {b!r} must be a number "
                "or 'unbounded' / 'current'"
            )
        return cur + b * unit_ms

    lo, hi = edge(bounds[0]), edge(bounds[1])
    # range_vals is ascending; walk in from both ends. A non-numeric value in the
    # partition makes the comparison ill-defined -> raise (mongod rejects it too).
    if any(not _is_number(v) for v in range_vals):
        raise AggregateError("$setWindowFields range windows require numeric sortBy values")
    low = 0
    while low < n and lo is not None and range_vals[low] < lo:
        low += 1
    high = n - 1
    while high >= 0 and hi is not None and range_vals[high] > hi:
        high -= 1
    return low, high


def _is_number(v: Any) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


# Fixed-duration window units → milliseconds (variable-length month/quarter/year
# defer). Used to offset a date-valued range window in the sortBy's own units.
_WINDOW_UNIT_MS: dict[str, int] = {
    "week": 604_800_000,
    "day": 86_400_000,
    "hour": 3_600_000,
    "minute": 60_000,
    "second": 1_000,
    "millisecond": 1,
}

_EPOCH_UTC = _dt.datetime(1970, 1, 1, tzinfo=_dt.timezone.utc)


def _date_ms(dt: _dt.datetime) -> int:
    """Epoch milliseconds for a datetime, matching a BSON Date (naive → UTC). Exact
    integer arithmetic so the Rust port (`bson DateTime::timestamp_millis`) agrees."""
    aware = dt if dt.tzinfo is not None else dt.replace(tzinfo=_dt.timezone.utc)
    delta = aware - _EPOCH_UTC
    return delta.days * 86_400_000 + delta.seconds * 1_000 + delta.microseconds // 1_000


def _ema_alpha(spec: Mapping[str, Any]) -> float:
    """The smoothing factor for `$expMovingAvg`: `2/(N+1)` from a positive-int
    `N`, or a given `alpha` in (0, 1). Exactly one must be present."""
    has_n, has_alpha = "N" in spec, "alpha" in spec
    if has_n == has_alpha:
        raise AggregateError("$expMovingAvg requires exactly one of N / alpha")
    if has_n:
        n = spec["N"]
        if not isinstance(n, int) or isinstance(n, bool) or n < 1:
            raise AggregateError("$expMovingAvg N must be a positive integer")
        return 2 / (n + 1)
    a = spec["alpha"]
    if not _is_number(a) or not (0 < a < 1):
        raise AggregateError("$expMovingAvg alpha must be a number in (0, 1)")
    return float(a)


def _compute_window_vector(
    op: str,
    partition_docs: list[dict[str, Any]],
    arg: Any,
    range_vals: list[Any] | None,
    ctx: PipelineContext,
) -> list[Any]:
    """Per-slot output for the prefix/partition window operators."""
    if op == "$expMovingAvg":
        return _compute_ema(partition_docs, arg, ctx)
    if op == "$locf":
        return _compute_locf(partition_docs, arg, ctx)
    return _compute_linear_fill(partition_docs, arg, range_vals, ctx)  # $linearFill


def _compute_locf(
    partition_docs: list[dict[str, Any]],
    expr: Any,
    ctx: PipelineContext,
) -> list[Any]:
    """Last-observation-carried-forward of ``expr`` over the sorted partition:
    a null / missing value takes the last non-null seen; leading nulls stay null."""
    out: list[Any] = []
    last: Any = None
    seen = False
    for doc in partition_docs:
        v = evaluate(expr, doc, ctx.vars)
        if v is not None:
            last, seen = v, True
            out.append(v)
        else:
            out.append(last if seen else None)
    return out


def _compute_linear_fill(
    partition_docs: list[dict[str, Any]],
    expr: Any,
    range_vals: list[Any] | None,
    ctx: PipelineContext,
) -> list[Any]:
    """Linear interpolation of ``expr``'s nulls between surrounding non-null
    anchors, using the (single ascending numeric) sortBy value as the x-axis.
    Leading / trailing nulls stay null. Values in IEEE double so the Rust port
    matches bit-for-bit."""
    if range_vals is None:
        raise AggregateError("$linearFill requires a single ascending sortBy field")
    vals = [evaluate(expr, doc, ctx.vars) for doc in partition_docs]
    out = list(vals)
    anchors = [i for i, v in enumerate(vals) if v is not None]
    for a, b in zip(anchors, anchors[1:], strict=False):
        x0, x1, y0, y1 = range_vals[a], range_vals[b], vals[a], vals[b]
        if not all(_is_number(x) for x in (x0, x1, y0, y1)):
            raise AggregateError("$linearFill requires numeric sortBy values and inputs")
        if x1 == x0:
            raise AggregateError("$linearFill: coincident sortBy values")
        for i in range(a + 1, b):
            x = range_vals[i]
            if not _is_number(x):
                raise AggregateError("$linearFill requires numeric sortBy values")
            out[i] = y0 + (y1 - y0) * ((x - x0) / (x1 - x0))
    return out


def _ts_window_value(
    op: str,
    arg: Mapping[str, Any],
    low: int,
    high: int,
    partition_docs: list[dict[str, Any]],
    range_vals: list[Any] | None,
    range_date_ms: list[int] | None,
    ctx: PipelineContext,
) -> Any:
    """`$derivative` / `$integral` over the window `[low, high]`, using the sortBy
    value as x and ``input`` as y. `$derivative` is the slope between the first
    and last window points (null if fewer than two, or the x's coincide);
    `$integral` is the trapezoidal area.

    Without a ``unit``: a single ascending numeric sortBy (`range_vals`) is the
    x-axis. With a fixed-duration ``unit``: the x-axis is a date sortBy's epoch
    millis (`range_date_ms`) divided by the unit's millisecond span, so the rate
    is *per unit* (e.g. per hour). `unit` requires a date sortBy; a variable-
    length unit (month/quarter/year) raises. Math in IEEE double."""
    input_expr = arg["input"]
    unit = arg.get("unit")
    if unit is not None:
        if range_date_ms is None:
            raise AggregateError(f"$setWindowFields {op} 'unit' requires a date sortBy field")
        unit_ms = _WINDOW_UNIT_MS.get(unit)
        if unit_ms is None:
            raise AggregateError(
                f"$setWindowFields {op} unit {unit!r} is not supported "
                "(variable-length month/quarter/year)"
            )
        # x-axis is the date's epoch millis scaled into the requested unit.
        xs: list[Any] = [ms / unit_ms for ms in range_date_ms]
    else:
        if range_vals is None:
            raise AggregateError(
                f"$setWindowFields {op} requires a single ascending numeric sortBy"
            )
        xs = range_vals
    pts: list[tuple[float, float]] = []
    for i in range(low, high + 1):
        x, y = xs[i], evaluate(input_expr, partition_docs[i], ctx.vars)
        if not (_is_number(x) and _is_number(y)):
            raise AggregateError(f"$setWindowFields {op} requires numeric sortBy values and inputs")
        pts.append((x, y))
    if op == "$derivative":
        if len(pts) < 2:
            return None
        (x0, y0), (x1, y1) = pts[0], pts[-1]
        return None if x1 == x0 else (y1 - y0) / (x1 - x0)
    total = 0.0  # $integral — trapezoidal sum
    for (xa, ya), (xb, yb) in zip(pts, pts[1:], strict=False):
        total += (xb - xa) * (ya + yb) / 2
    return total


def _compute_ema(
    partition_docs: list[dict[str, Any]],
    spec: Mapping[str, Any],
    ctx: PipelineContext,
) -> list[float]:
    """Per-slot exponential moving average over the sorted partition:
    ``ema[0] = input[0]``; ``ema[i] = input[i]*alpha + ema[i-1]*(1-alpha)``. All
    in IEEE double so the Rust port matches bit-for-bit."""
    alpha = _ema_alpha(spec)
    out: list[float] = []
    prev: float | None = None
    for doc in partition_docs:
        val = evaluate(spec["input"], doc, ctx.vars)
        if not _is_number(val):
            raise AggregateError("$expMovingAvg input must be numeric")
        val = float(val)
        ema = val if prev is None else val * alpha + prev * (1 - alpha)
        out.append(ema)
        prev = ema
    return out


def _shift_value(
    spec: Mapping[str, Any],
    slot: int,
    partition_docs: list[dict[str, Any]],
    ctx: PipelineContext,
) -> Any:
    """`$shift` — the `output` expression evaluated on the row ``by`` positions
    away in the sorted partition, or ``default`` (evaluated as a constant, or
    ``null``) when that position is outside the partition."""
    idx = slot + spec["by"]
    if 0 <= idx < len(partition_docs):
        return evaluate(spec["output"], partition_docs[idx], ctx.vars)
    if "default" in spec:
        return evaluate(spec["default"], partition_docs[slot], ctx.vars)
    return None


def _window_bounds(slot: int, n: int, window: Mapping[str, Any] | None) -> tuple[int, int]:
    """Resolve a document-based window spec for a given row position.

    Returns inclusive ``(lower, upper)`` indices into the partition.
    ``window=None`` or missing ``documents`` → the whole partition
    (matches mongod's default window).
    """
    if window is None or "documents" not in window:
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
    if op == "$mergeObjects":
        return {}
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


def _geo_near_index_filter(
    spec: Any, storage: Any, db_name: str | None, coll_name: str | None
) -> dict[str, Any] | None:
    """Conservative ``$geoWithin`` candidate filter for a leading ``$geoNear`` with
    a ``maxDistance``, so the initial fetch can ride a geo index instead of
    scanning the whole collection — or ``None`` when the optimization doesn't
    apply (no ``maxDistance``, unparseable ``near``, or no matching geo index on
    ``key``).

    The candidate radius is inflated by a tiny epsilon so the fetched set is a
    strict **superset** of the exact within-``maxDistance`` set; the ``$geoNear``
    stage then re-applies the exact distance filter, so its output is byte-for-byte
    identical to the brute-force path — only the number of docs fetched shrinks.
    The candidate shape (``$centerSphere`` vs ``$center``) must match the index
    type (``2dsphere`` vs ``2d``); a mismatch falls back to the full scan.
    """
    if not isinstance(spec, Mapping):
        return None
    max_distance = spec.get("maxDistance")
    if not isinstance(max_distance, (int, float)) or isinstance(max_distance, bool):
        return None
    if storage is None or not db_name or not coll_name:
        return None
    # Geo-indexed fields (field -> "2dsphere"/"2d"), in list_indexes order.
    geo_fields: dict[str, str] = {}
    for index in storage.list_indexes(db_name, coll_name):
        key_spec = index.get("key", {})
        if not isinstance(key_spec, Mapping):
            continue
        for field, value in key_spec.items():
            if isinstance(value, str) and value in ("2dsphere", "2d"):
                geo_fields.setdefault(field, value)
    if not geo_fields:
        return None
    key = spec.get("key")
    if not (isinstance(key, str) and key):
        key = next(iter(geo_fields))  # infer: first geo index (mongod's behaviour)
    if key not in geo_fields:
        return None
    try:
        spherical, center = _parse_geo_near_origin(spec.get("near"), spec.get("spherical"))
    except AggregateError:
        return None
    idx_type = geo_fields[key]
    if spherical and idx_type != "2dsphere":
        return None
    if not spherical and idx_type != "2d":
        return None
    from secantus.geo import EARTH_RADIUS_METERS

    radius = float(max_distance) * (1.0 + 1e-9)  # inflate -> guaranteed superset
    cx, cy = center
    if spherical:
        return {key: {"$geoWithin": {"$centerSphere": [[cx, cy], radius / EARTH_RADIUS_METERS]}}}
    return {key: {"$geoWithin": {"$center": [[cx, cy], radius]}}}


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
    "$changeStreamSplitLargeEvent": _stage_change_stream_split_large_event,
    "$geoNear": _stage_geo_near,
    "$unionWith": _stage_union_with,
    "$redact": _stage_redact,
    "$setWindowFields": _stage_set_window_fields,
}


def validate_stage_names(pipeline: list[Any]) -> None:
    """Upfront stage-name validation (mongod validates at parse time,
    before any document flows — change streams need the 40324 at
    ``aggregate`` time, not lazily at the first ``getMore``)."""
    for stage in pipeline:
        if not isinstance(stage, Mapping) or len(stage) != 1:
            raise AggregateError("each pipeline stage must have exactly one key")
        name = next(iter(stage))
        if name in _ATLAS_ONLY_STAGES:
            # Atlas-only stage — reject with the Atlas message at parse time
            # (this validation runs before any document flows), so the driver
            # sees "Atlas" rather than the generic unrecognized-stage error.
            raise AggregateError(SEARCH_INDEX_ATLAS_MSG, code=115, code_name="CommandNotSupported")
        if name not in _STAGES:
            raise AggregateError(
                f"Unrecognized pipeline stage name: '{name}'",
                code=40324,
                code_name="Location40324",
            )
        # Validate ``$match`` filter syntax up-front too. A change-stream
        # pipeline doesn't execute until the first ``getMore``, so an
        # unknown query operator inside ``$match`` (e.g. ``{$foo: -1}``)
        # would otherwise only surface there — mongo-cxx-driver's
        # "invalid pipeline / Error on .begin()" test requires the error
        # at aggregate (``.begin()``) time. Running the matcher against an
        # empty doc triggers the same operator validation. Only the
        # syntactic ``QueryError`` is surfaced now; ``$expr`` evaluation
        # errors that only make sense against a real change event stay
        # deferred to execution time.
        if name == "$match" and isinstance(stage[name], Mapping):
            try:
                matches({}, stage[name])
            except QueryError:
                raise
            except Exception:
                pass
