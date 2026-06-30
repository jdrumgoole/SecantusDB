"""Window-function evaluation for the per-row (evaluated) SELECT path.

Window functions (``func(...) OVER (PARTITION BY … ORDER BY …)``) are per-row
outputs that depend on a whole partition, so they can't lower to a Mongo
``$project``/``$group``. The evaluated-select executor fetches the rows, and this
module computes each window value over those rows in Python — partition, order
within the partition, then apply the function — and stores the result on each
row under a synthetic field. The scalar evaluator then resolves an ``exp.Window``
node to its precomputed value (so a window may also nest inside an expression).

Supported: ``ROW_NUMBER`` / ``RANK`` / ``DENSE_RANK``; aggregate windows
``SUM`` / ``COUNT`` / ``AVG`` / ``MIN`` / ``MAX`` (whole-partition, or a running
aggregate under the default ``RANGE`` frame when the window has its own
``ORDER BY``); and ``LAG`` / ``LEAD``. Explicit frame clauses (``ROWS`` /
``RANGE BETWEEN``) are rejected.
"""

from __future__ import annotations

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

    if w.args.get("spec") is not None:
        raise errors.feature_not_supported(
            "explicit window frames (ROWS/RANGE BETWEEN) are not supported"
        )
    func = w.this
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
        values = _window_values(func, ordered, okeys, bool(order_terms), scope_of, sctx)
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
    ordered: list[dict[str, Any]],
    okeys: list[tuple],
    has_order: bool,
    scope_of: Any,
    sctx: Any,
) -> list[Any]:

    n = len(ordered)
    if isinstance(func, exp.RowNumber):
        return [i + 1 for i in range(n)]
    if isinstance(func, (exp.Rank, exp.DenseRank)):
        return _rank_values(func, okeys)
    if isinstance(func, (exp.Lag, exp.Lead)):
        return _lag_lead_values(func, ordered, scope_of, sctx)
    agg = _AGG_WINDOWS.get(type(func))
    if agg is not None:
        return _agg_window_values(agg, func, ordered, okeys, has_order, scope_of, sctx)
    raise errors.feature_not_supported(f"window function {type(func).__name__} is not supported")


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
    okeys: list[tuple],
    has_order: bool,
    scope_of: Any,
    sctx: Any,
) -> list[Any]:
    from secantus.sql import scalar

    arg = None if isinstance(func.this, (exp.Star, type(None))) else func.this
    count_star = agg == "count" and arg is None
    raw = [None if count_star else scalar.evaluate(arg, scope_of(d), sctx) for d in ordered]
    n = len(ordered)
    out: list[Any] = [None] * n
    if not has_order:
        whole = _reduce(agg, raw, count_star)
        return [whole] * n
    # Default RANGE frame: rows with equal ORDER BY keys (peers) share the value
    # computed through the end of their peer group (UNBOUNDED PRECEDING → CURRENT).
    i = 0
    while i < n:
        j = i
        while j + 1 < n and okeys[j + 1] == okeys[i]:
            j += 1
        gval = _reduce(agg, raw[: j + 1], count_star)
        for t in range(i, j + 1):
            out[t] = gval
        i = j + 1
    return out


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
