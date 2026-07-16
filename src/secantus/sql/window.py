"""Window-function evaluation for the per-row (evaluated) SELECT path.

Window functions (``func(...) OVER (PARTITION BY … ORDER BY …)``) are per-row
outputs that depend on a whole partition, so they can't lower to a Mongo
``$project``/``$group``. The evaluated-select executor fetches the rows, and this
module computes each window value over those rows in Python — partition, order
within the partition, then apply the function — and stores the result on each
row under a synthetic field. The scalar evaluator then resolves an ``exp.Window``
node to its precomputed value (so a window may also nest inside an expression).

Supported: ``ROW_NUMBER`` / ``RANK`` / ``DENSE_RANK`` / ``NTILE``; the value
functions ``FIRST_VALUE`` / ``LAST_VALUE`` / ``NTH_VALUE``; aggregate windows
``SUM`` / ``COUNT`` / ``AVG`` / ``MIN`` / ``MAX``; and ``LAG`` / ``LEAD``.
Explicit frame clauses are supported: ``ROWS`` frames with any
``UNBOUNDED`` / ``CURRENT ROW`` / ``n PRECEDING`` / ``n FOLLOWING`` bound, and
``RANGE`` frames with ``UNBOUNDED`` / ``CURRENT ROW`` bounds *and* a numeric
``n PRECEDING`` / ``n FOLLOWING`` offset **or** an ``INTERVAL`` offset over a
date/timestamp key (a row is in-frame when its ORDER BY key is within the offset
of the current row's key along the sort order; Postgres requires exactly one
ORDER BY column for an offset ``RANGE`` frame). The default frame matches
Postgres: whole partition with no
``ORDER BY``, else ``RANGE UNBOUNDED PRECEDING`` to ``CURRENT ROW`` (peers tied
on the order key share the value).
"""

from __future__ import annotations

import datetime as _dt
from typing import Any

from sqlglot import exp

from secantus.paths import get_path
from secantus.sql import errors

# func node -> aggregate name, for the aggregate windows.
_AGG_WINDOWS: dict[type, str] = {
    exp.Sum: "sum",
    exp.Count: "count",
    exp.Avg: "avg",
    exp.Min: "min",
    exp.Max: "max",
}


def collect_windows(out_exprs: list[exp.Expression]) -> list[exp.Window]:
    """All distinct ``exp.Window`` nodes appearing in the output expressions
    (including nested inside a larger expression), in first-seen order."""
    found: list[exp.Window] = []
    seen: set[int] = set()
    for e in out_exprs:
        for w in e.find_all(exp.Window):
            if id(w) not in seen:
                seen.add(id(w))
                found.append(w)
    return found


def compute_windows(
    out_exprs: list[exp.Expression], docs: list[dict[str, Any]], resolve: Any, sctx: Any
) -> dict[int, str]:
    """Compute every window function over ``docs``, storing each one's per-row
    value on the doc under a synthetic ``__win_<k>`` field. Returns a map from the
    window node's id() to that field name, for the scope to resolve against."""

    def scope_of(doc: dict[str, Any]):
        def scope(node: Any) -> Any:
            if isinstance(node, exp.Window):
                return get_path(doc, win_field[id(node)])
            path, _ = resolve(node)
            return get_path(doc, path)

        return scope

    win_field: dict[int, str] = {}
    for k, w in enumerate(collect_windows(out_exprs)):
        field = f"__win_{k}"
        win_field[id(w)] = field
        values = _eval_window(w, docs, scope_of, sctx)
        for doc, value in zip(docs, values, strict=True):
            doc[field] = value
    return win_field


def _eval_window(w: exp.Window, docs: list[dict[str, Any]], scope_of: Any, sctx: Any) -> list[Any]:
    """Return the window's value for each doc (parallel to ``docs``)."""
    from secantus.sql import scalar

    func = w.this
    spec = w.args.get("spec")
    partition_by = w.args.get("partition_by") or []
    order_node = w.args.get("order")
    order_terms = (
        [(o.this, -1 if o.args.get("desc") else 1) for o in order_node.expressions]
        if order_node is not None
        else []
    )

    result: dict[int, Any] = {}
    for part in _partitions(docs, partition_by, scope_of, sctx):
        ordered = _order_partition(part, order_terms, scope_of, sctx)
        okeys = [
            tuple(scalar.evaluate(oe, scope_of(d), sctx) for oe, _ in order_terms) for d in ordered
        ]
        order_dirs = [d for _, d in order_terms]
        values = _window_values(
            func, spec, ordered, okeys, bool(order_terms), order_dirs, scope_of, sctx
        )
        for d, v in zip(ordered, values, strict=True):
            result[id(d)] = v
    return [result[id(d)] for d in docs]


def _partitions(
    docs: list[dict[str, Any]], partition_by: list[exp.Expression], scope_of: Any, sctx: Any
) -> list[list[dict[str, Any]]]:
    from secantus.sql import scalar

    if not partition_by:
        return [list(docs)]
    groups: dict[tuple, list[dict[str, Any]]] = {}
    order: list[tuple] = []
    for doc in docs:
        scope = scope_of(doc)
        key = tuple(repr(scalar.evaluate(p, scope, sctx)) for p in partition_by)
        if key not in groups:
            groups[key] = []
            order.append(key)
        groups[key].append(doc)
    return [groups[k] for k in order]


def _order_partition(
    part: list[dict[str, Any]],
    order_terms: list[tuple[exp.Expression, int]],
    scope_of: Any,
    sctx: Any,
) -> list[dict[str, Any]]:
    from secantus.sql import scalar

    ordered = list(part)
    # Stable multi-key sort: apply each key from least to most significant. NULLs
    # sort last for ASC (Postgres default NULLS LAST), reversed for DESC.
    for oe, direction in reversed(order_terms):
        ordered.sort(
            key=lambda d, oe=oe: _null_key(scalar.evaluate(oe, scope_of(d), sctx)),
            reverse=(direction == -1),
        )
    return ordered


def _null_key(v: Any) -> tuple[int, Any]:
    return (v is None, v)


def _window_values(
    func: exp.Expression,
    spec: exp.Expression | None,
    ordered: list[dict[str, Any]],
    okeys: list[tuple],
    has_order: bool,
    order_dirs: list[int],
    scope_of: Any,
    sctx: Any,
) -> list[Any]:
    n = len(ordered)
    # Rank-like functions are frame-insensitive (Postgres ignores any frame on
    # them), so they don't consult the frame at all.
    if isinstance(func, exp.RowNumber):
        return [i + 1 for i in range(n)]
    if isinstance(func, (exp.Rank, exp.DenseRank)):
        return _rank_values(func, okeys)
    if isinstance(func, exp.Ntile):
        return _ntile_values(func, n, scope_of, sctx)
    if isinstance(func, (exp.Lag, exp.Lead)):
        return _lag_lead_values(func, ordered, scope_of, sctx)

    # Everything else is frame-sensitive: build each row's frame [lo, hi].
    frames = _frames(spec, n, okeys, has_order, order_dirs)
    if isinstance(func, (exp.FirstValue, exp.LastValue, exp.NthValue)):
        return _value_window(func, ordered, frames, scope_of, sctx)
    agg = _AGG_WINDOWS.get(type(func))
    if agg is not None:
        return _agg_window_values(agg, func, ordered, frames, scope_of, sctx)
    raise errors.feature_not_supported(f"window function {type(func).__name__} is not supported")


def _ntile_values(func: exp.Expression, n: int, scope_of: Any, sctx: Any) -> list[Any]:
    """NTILE(k): split the ordered partition into ``k`` buckets as evenly as
    possible (the first ``n % k`` buckets get one extra row) and label each row
    with its 1-based bucket number."""
    from secantus.sql import scalar

    if n == 0:
        return []
    buckets = int(scalar.evaluate(func.this, scope_of({}), sctx))
    if buckets <= 0:
        raise errors.feature_not_supported("NTILE requires a positive bucket count")
    base, rem = divmod(n, buckets)
    out: list[int] = []
    for b in range(1, buckets + 1):
        size = base + (1 if b <= rem else 0)
        out.extend([b] * size)
    return out


def _frames(
    spec: exp.Expression | None,
    n: int,
    okeys: list[tuple],
    has_order: bool,
    order_dirs: list[int],
) -> list[tuple[int, int]]:
    """The inclusive ``[lo, hi]`` frame index range for each row. With no explicit
    frame the Postgres default applies: the whole partition when there's no
    ORDER BY, else ``RANGE UNBOUNDED PRECEDING`` to ``CURRENT ROW`` (peer-shared)."""
    if spec is None:
        if not has_order:
            return [(0, n - 1) for _ in range(n)]
        return [(0, _peer_end(okeys, i, n)) for i in range(n)]
    kind = (spec.args.get("kind") or "RANGE").upper()
    start, start_side = spec.args.get("start"), spec.args.get("start_side")
    # A frame with only a start bound runs through CURRENT ROW.
    end = spec.args.get("end") if "end" in spec.args else "CURRENT ROW"
    end_side = spec.args.get("end_side")
    frames: list[tuple[int, int]] = []
    for i in range(n):
        if kind == "ROWS":
            lo = _rows_bound(start, start_side, i, n, is_start=True)
            hi = _rows_bound(end, end_side, i, n, is_start=False)
        else:
            lo = _range_bound(start, start_side, i, n, okeys, order_dirs, is_start=True)
            hi = _range_bound(end, end_side, i, n, okeys, order_dirs, is_start=False)
        frames.append((max(0, lo), min(n - 1, hi)))
    return frames


def _peer_end(okeys: list[tuple], i: int, n: int) -> int:
    j = i
    while j + 1 < n and okeys[j + 1] == okeys[i]:
        j += 1
    return j


def _peer_start(okeys: list[tuple], i: int) -> int:
    j = i
    while j > 0 and okeys[j - 1] == okeys[i]:
        j -= 1
    return j


def _rows_bound(val: Any, side: Any, i: int, n: int, *, is_start: bool) -> int:
    if val is None or val == "CURRENT ROW":
        return i
    if val == "UNBOUNDED":
        return 0 if side == "PRECEDING" else n - 1
    k = int(val.this) if isinstance(val, exp.Literal) else int(val)
    return i - k if side == "PRECEDING" else i + k


def _range_bound(
    val: Any,
    side: Any,
    i: int,
    n: int,
    okeys: list[tuple],
    order_dirs: list[int],
    *,
    is_start: bool,
) -> int:
    if isinstance(val, exp.Literal):
        return _range_offset_bound(val, side, i, n, okeys, order_dirs, is_start=is_start)
    if isinstance(val, exp.Interval):
        return _range_interval_bound(val, side, i, n, okeys, order_dirs, is_start=is_start)
    if val is None or val == "CURRENT ROW":
        return _peer_start(okeys, i) if is_start else _peer_end(okeys, i, n)
    if val == "UNBOUNDED":
        return 0 if side == "PRECEDING" else n - 1
    raise errors.feature_not_supported(f"unsupported RANGE frame bound: {val}")


def _interval_subdoc(node: exp.Interval) -> dict:
    """An ``exp.Interval`` frame bound (``INTERVAL '1 day'``) -> an interval
    subdocument, mirroring the planner's literal conversion."""
    from secantus.sql import intervals as _intervals

    raw = node.this.this if isinstance(node.this, exp.Literal) else node.this
    unit = node.args.get("unit")
    if unit is not None:
        return _intervals.from_unit(float(raw), unit.name)
    return _intervals.parse(str(raw))


def _range_interval_bound(
    val: exp.Interval,
    side: Any,
    i: int,
    n: int,
    okeys: list[tuple],
    order_dirs: list[int],
    *,
    is_start: bool,
) -> int:
    """An ``INTERVAL`` ``RANGE`` offset bound (``INTERVAL '1 day' PRECEDING`` /
    ``FOLLOWING``) over a date/timestamp ORDER BY key. The boundary is the current
    row's key shifted by the interval along the sort direction — ``PRECEDING`` back,
    ``FOLLOWING`` forward — and a row is in-frame when its key sits on the in-frame
    side of that boundary. Like the numeric offset form, Postgres requires exactly
    one ORDER BY column."""
    from secantus.sql import intervals as _intervals

    if len(order_dirs) != 1:
        raise errors.feature_not_supported(
            "RANGE with an offset requires exactly one ORDER BY column"
        )
    subdoc = _interval_subdoc(val)
    months, days, micros = _intervals._fields(subdoc)
    if months < 0 or days < 0 or micros < 0:
        raise errors.SQLError("22013", "invalid preceding or following size in window function")
    cur = okeys[i][0]
    if cur is None:
        # A NULL order key has no distance; its frame is its NULL peers.
        return _peer_start(okeys, i) if is_start else _peer_end(okeys, i, n)
    if not isinstance(cur, (_dt.date, _dt.datetime)):
        raise errors.feature_not_supported(
            "RANGE with an interval offset requires a date/timestamp ORDER BY key"
        )
    direction = order_dirs[0]
    sign = -1 if side == "PRECEDING" else 1
    boundary = _intervals.to_date(cur, subdoc, direction * sign)
    if is_start:
        for j in range(n):
            k = okeys[j][0]
            if k is not None and (k >= boundary if direction == 1 else k <= boundary):
                return j
        return n  # no row satisfies the lower bound → empty frame
    hi = -1
    for j in range(n):
        k = okeys[j][0]
        if k is not None and (k <= boundary if direction == 1 else k >= boundary):
            hi = j
    return hi


def _range_offset_bound(
    val: exp.Literal,
    side: Any,
    i: int,
    n: int,
    okeys: list[tuple],
    order_dirs: list[int],
    *,
    is_start: bool,
) -> int:
    """A numeric ``RANGE`` offset bound (``n PRECEDING`` / ``n FOLLOWING``): a row is
    in-frame when its ORDER BY key is within ``offset`` of the current row's key
    along the sort order. Postgres requires exactly one ORDER BY column for an
    offset RANGE frame. The bound is computed by value (not row count) over the
    already-sorted ``okeys``.

    Working in a direction-normalised key space ``nk = dir * key`` (non-decreasing
    in walk order for both ASC and DESC), ``PRECEDING`` shifts the boundary down by
    ``offset`` and ``FOLLOWING`` shifts it up, so the same comparison serves every
    bound/direction combination."""
    if len(order_dirs) != 1:
        raise errors.feature_not_supported(
            "RANGE with an offset requires exactly one ORDER BY column"
        )
    if val.is_string:
        raise errors.feature_not_supported(
            "RANGE with a non-numeric (interval) offset is not supported"
        )
    offset = float(val.this)
    if offset < 0:
        raise errors.SQLError("22013", "invalid preceding or following size in window function")
    cur = okeys[i][0]
    if cur is None:
        # A NULL order key has no numeric distance; its frame is its NULL peers.
        return _peer_start(okeys, i) if is_start else _peer_end(okeys, i, n)
    direction = order_dirs[0]
    if not isinstance(cur, (int, float)):
        raise errors.feature_not_supported(
            "RANGE with a numeric offset requires a numeric ORDER BY key"
        )
    nc = direction * cur
    boundary = nc - offset if side == "PRECEDING" else nc + offset
    if is_start:
        for j in range(n):
            k = okeys[j][0]
            if k is not None and direction * k >= boundary:
                return j
        return n  # no row satisfies the lower bound → empty frame
    hi = -1
    for j in range(n):
        k = okeys[j][0]
        if k is not None and direction * k <= boundary:
            hi = j
    return hi


def _value_window(
    func: exp.Expression,
    ordered: list[dict[str, Any]],
    frames: list[tuple[int, int]],
    scope_of: Any,
    sctx: Any,
) -> list[Any]:
    """FIRST_VALUE / LAST_VALUE / NTH_VALUE: read the argument at the first, last,
    or n-th (1-based) row of each row's frame (NULL when the frame is short)."""
    from secantus.sql import scalar

    vals = [scalar.evaluate(func.this, scope_of(d), sctx) for d in ordered]
    nth = None
    if isinstance(func, exp.NthValue):
        nth = int(scalar.evaluate(func.args["offset"], scope_of(ordered[0]), sctx))
    out: list[Any] = []
    for lo, hi in frames:
        if lo > hi:
            out.append(None)
            continue
        if isinstance(func, exp.FirstValue):
            out.append(vals[lo])
        elif isinstance(func, exp.LastValue):
            out.append(vals[hi])
        else:  # NthValue — 1-based within the frame
            idx = lo + nth - 1
            out.append(vals[idx] if lo <= idx <= hi else None)
    return out


def _rank_values(func: exp.Expression, okeys: list[tuple]) -> list[Any]:
    dense = isinstance(func, exp.DenseRank)
    out: list[int] = []
    rank = 0
    dense_rank = 0
    prev: Any = object()
    for i, key in enumerate(okeys):
        if key != prev:
            rank = i + 1
            dense_rank += 1
            prev = key
        out.append(dense_rank if dense else rank)
    return out


def _lag_lead_values(
    func: exp.Expression, ordered: list[dict[str, Any]], scope_of: Any, sctx: Any
) -> list[Any]:
    from secantus.sql import scalar

    arg = func.this
    vals = [scalar.evaluate(arg, scope_of(d), sctx) for d in ordered]
    offset_node = func.args.get("offset")
    offset = int(scalar.evaluate(offset_node, scope_of(ordered[0]), sctx)) if offset_node else 1
    default_node = func.args.get("default")
    default = scalar.evaluate(default_node, scope_of(ordered[0]), sctx) if default_node else None
    step = -offset if isinstance(func, exp.Lag) else offset
    out: list[Any] = []
    for i in range(len(vals)):
        j = i + step
        out.append(vals[j] if 0 <= j < len(vals) else default)
    return out


def _agg_window_values(
    agg: str,
    func: exp.Expression,
    ordered: list[dict[str, Any]],
    frames: list[tuple[int, int]],
    scope_of: Any,
    sctx: Any,
) -> list[Any]:
    from secantus.sql import scalar

    arg = None if isinstance(func.this, (exp.Star, type(None))) else func.this
    count_star = agg == "count" and arg is None
    raw = [None if count_star else scalar.evaluate(arg, scope_of(d), sctx) for d in ordered]
    return [_reduce(agg, raw[lo : hi + 1], count_star) for lo, hi in frames]


def _reduce(agg: str, values: list[Any], count_star: bool) -> Any:
    if agg == "count":
        return len(values) if count_star else sum(1 for v in values if v is not None)
    nonnull = [v for v in values if v is not None]
    if not nonnull:
        return None
    if agg == "sum":
        return sum(nonnull)
    if agg == "avg":
        return sum(nonnull) / len(nonnull)
    if agg == "min":
        return min(nonnull)
    if agg == "max":
        return max(nonnull)
    raise errors.feature_not_supported(f"window aggregate {agg} is not supported")
