"""Set-returning functions as a query row source (#125).

Two shapes are handled here:

* a base-less ``FROM`` table function —
  ``SELECT * FROM generate_series(1, 5) [WITH ORDINALITY] [AS t(a, ord)]`` and
  the same for ``unnest`` / ``jsonb_array_elements`` / ``jsonb_object_keys`` /
  ``regexp_split_to_table``; and
* a base-less SELECT-list SRF — ``SELECT generate_series(1, 5)``.

Both materialize the generated rows and run the rest of the query (projection /
``WHERE`` / ``ORDER BY`` / ``LIMIT``) over an in-memory table, reusing the normal
select planner + executor. The ``FROM t, <srf>(...)`` *join* form (one row per
outer row × element) stays in the pipeline planner's ``_unnest_join_stage``.
"""

from __future__ import annotations

import datetime as _dt
import re
from typing import Any

from sqlglot import exp

from . import errors, intervals
from .catalog import Column, TableDef

# Named SRFs (as an ``Anonymous`` call). ``generate_series`` parses to its own
# ``ExplodingGenerateSeries`` node and is handled separately.
_NAMED_SRFS = frozenset(
    {
        "unnest",
        "generate_subscripts",
        "jsonb_array_elements",
        "json_array_elements",
        "jsonb_array_elements_text",
        "json_array_elements_text",
        "jsonb_object_keys",
        "json_object_keys",
        "regexp_split_to_table",
        "regexp_matches",
    }
)


class SrfSource:
    """A resolved SRF row source: the callable node, its ``WITH ORDINALITY`` flag,
    and the table / column aliases from ``AS name(col, …)``."""

    def __init__(
        self,
        node: exp.Expression,
        ordinality: bool,
        table_alias: str | None,
        column_aliases: list[str],
    ) -> None:
        self.node = node
        self.ordinality = ordinality
        self.table_alias = table_alias
        self.column_aliases = column_aliases


def _is_srf_node(node: exp.Expression) -> bool:
    if isinstance(node, (exp.ExplodingGenerateSeries, exp.Unnest, exp.Explode)):
        return True
    if isinstance(node, exp.Anonymous):
        return str(node.this).rsplit(".", 1)[-1].lower() in _NAMED_SRFS
    return False


def _alias_parts(alias_node: exp.Expression | None) -> tuple[str | None, list[str]]:
    if alias_node is None:
        return None, []
    name = alias_node.this.name if alias_node.this is not None else None
    cols = [c.name for c in (alias_node.args.get("columns") or [])]
    return name, cols


def from_source(stmt: exp.Select) -> SrfSource | None:
    """The SRF row source if ``stmt``'s ``FROM`` is a base-less table function
    (no real table, no joins), else None."""
    if stmt.args.get("joins") or stmt.args.get("group") or stmt.args.get("having"):
        return None
    from_node = stmt.args.get("from_")
    if from_node is None:
        return None
    src = from_node.this
    if isinstance(src, exp.Unnest):
        name, cols = _alias_parts(src.args.get("alias"))
        return SrfSource(src, bool(src.args.get("offset")), name, cols)
    if isinstance(src, exp.Table) and _is_srf_node(src.this):
        name, cols = _alias_parts(src.args.get("alias"))
        return SrfSource(src.this, bool(src.args.get("ordinality")), name, cols)
    return None


def fromless_projection(stmt: exp.Select) -> SrfSource | None:
    """A base-less ``SELECT <srf>(...)`` (no FROM) whose single projection is an
    SRF, else None."""
    if stmt.args.get("from_") is not None or len(stmt.expressions) != 1:
        return None
    e = stmt.expressions[0]
    alias = e.alias if isinstance(e, exp.Alias) else None
    target = e.this if isinstance(e, exp.Alias) else e
    if not _is_srf_node(target):
        return None
    return SrfSource(target, False, None, [alias] if alias else [])


# --------------------------------------------------------------------------- #
# Row generation
# --------------------------------------------------------------------------- #


def _default_name(node: exp.Expression) -> str:
    if isinstance(node, exp.ExplodingGenerateSeries):
        return "generate_series"
    if isinstance(node, (exp.Unnest, exp.Explode)):
        return "unnest"
    if isinstance(node, exp.Anonymous):
        base = str(node.this).rsplit(".", 1)[-1].lower()
        return "unnest" if base == "unnest" else base
    return "?column?"


def _values_and_tag(node: exp.Expression, ctx: Any) -> tuple[list[Any], str]:
    """Generate the SRF's element values plus the value column's type tag."""
    from secantus.sql import scalar

    def ev(n: exp.Expression | None) -> Any:
        return scalar.evaluate(n, _empty_scope, ctx) if n is not None else None

    if isinstance(node, exp.ExplodingGenerateSeries):
        return _generate_series(
            ev(node.args.get("start")), ev(node.args.get("end")), ev(node.args.get("step"))
        )
    if isinstance(node, (exp.Unnest, exp.Explode)):
        arr = ev(node.expressions[0] if node.expressions else node.this)
        return (
            list(arr) if isinstance(arr, (list, tuple)) else ([] if arr is None else [arr])
        ), "any"
    if isinstance(node, exp.Anonymous):
        name = str(node.this).rsplit(".", 1)[-1].lower()
        args = node.expressions
        val = ev(args[0]) if args else None
        if name in ("unnest",):
            return (
                list(val) if isinstance(val, (list, tuple)) else ([] if val is None else [val])
            ), "any"
        if name == "generate_subscripts":
            n = len(val) if isinstance(val, (list, tuple)) else 0
            return list(range(1, n + 1)), "int4"
        if name in ("jsonb_array_elements", "json_array_elements"):
            return _as_json_list(val), "json"
        if name in ("jsonb_array_elements_text", "json_array_elements_text"):
            return [None if v is None else str(v) for v in _as_json_list(val)], "text"
        if name in ("jsonb_object_keys", "json_object_keys"):
            doc = _as_json(val)
            return (list(doc.keys()) if isinstance(doc, dict) else []), "text"
        if name == "regexp_split_to_table":
            pattern = ev(args[1]) if len(args) > 1 else ""
            text = "" if val is None else str(val)
            return re.split(str(pattern), text), "text"
        if name == "regexp_matches":
            # Set-returning: one row per match, each a text[] of the capture groups
            # (or the whole match when the pattern has none). The `g` flag yields
            # every match; without it, at most the first. NULL input -> no rows.
            pattern = ev(args[1]) if len(args) > 1 else None
            if val is None or pattern is None:
                return [], "text[]"
            flags = scalar._as_text(ev(args[2])) if len(args) > 2 else ""
            rx = scalar._re_compile(scalar._as_text(pattern), flags)
            rows: list[Any] = []
            for m in rx.finditer(scalar._as_text(val)):
                rows.append(list(m.groups()) if m.groups() else [m.group(0)])
                if "g" not in flags:
                    break
            return rows, "text[]"
    raise errors.feature_not_supported(f"unsupported set-returning function: {node.sql()}")


def _as_json(val: Any) -> Any:
    """A jsonb value as a Python object — decode a JSON text if needed."""
    if isinstance(val, str):
        import json

        try:
            return json.loads(val)
        except (ValueError, TypeError):
            return val
    return val


def _as_json_list(val: Any) -> list[Any]:
    doc = _as_json(val)
    return list(doc) if isinstance(doc, (list, tuple)) else []


def _generate_series(start: Any, stop: Any, step: Any) -> tuple[list[Any], str]:
    """``generate_series(start, stop[, step])`` — inclusive of both ends. Numeric
    ranges (int / numeric step) and date / timestamp ranges (an ``interval``
    step) are both supported."""
    if start is None or stop is None:
        return [], "int8"
    if _is_temporal(start) or intervals.is_interval(step):
        return _generate_series_temporal(start, stop, step)
    if not isinstance(start, (int, float)) or not isinstance(stop, (int, float)):
        raise errors.feature_not_supported(
            "generate_series is supported for integer / numeric or "
            "date / timestamp (with interval step) ranges only"
        )
    step = 1 if step is None else step
    if step == 0:
        raise errors.SQLError("22023", "step size cannot equal zero")
    out: list[Any] = []
    cur = start
    if step > 0:
        while cur <= stop:
            out.append(cur)
            cur += step
    else:
        while cur >= stop:
            out.append(cur)
            cur += step
    tag = "int8" if all(isinstance(v, int) for v in out) else "numeric"
    return out, tag


# A generous backstop against a runaway series (e.g. an interval step that never
# advances). Postgres relies on memory limits; a surrogate fails loudly instead.
_MAX_SERIES_ROWS = 10_000_000


def _is_temporal(v: Any) -> bool:
    return isinstance(v, _dt.date)  # datetime is a subclass of date


def _generate_series_temporal(start: Any, stop: Any, step: Any) -> tuple[list[Any], str]:
    """``generate_series(ts_start, ts_stop, interval)`` — walk from ``start`` to
    ``stop`` (inclusive) by ``interval``. The interval carries its own sign; the
    walk direction is taken from whether one step moves forward or backward."""
    if not (_is_temporal(start) and _is_temporal(stop)):
        raise errors.SQLError(
            "42883",
            "generate_series with an interval step requires date / timestamp bounds",
        )
    if not intervals.is_interval(step):
        raise errors.SQLError("42883", "generate_series over timestamps requires an interval step")
    start_dt, stop_dt = _to_datetime(start), _to_datetime(stop)
    nxt = intervals.to_date(start_dt, step, 1)
    if nxt == start_dt:
        raise errors.SQLError("22023", "step size cannot equal zero")
    ascending = nxt > start_dt
    out: list[Any] = []
    cur = start_dt
    while (cur <= stop_dt) if ascending else (cur >= stop_dt):
        out.append(cur)
        if len(out) > _MAX_SERIES_ROWS:
            raise errors.SQLError("54000", "generate_series produced too many rows")
        cur = intervals.to_date(cur, step, 1)
    tag = "timestamptz" if start_dt.tzinfo is not None else "timestamp"
    return out, tag


def _to_datetime(v: Any) -> _dt.datetime:
    if isinstance(v, _dt.datetime):
        return v
    # a bare date -> midnight
    return _dt.datetime(v.year, v.month, v.day)


def _empty_scope(col: exp.Column) -> Any:
    raise errors.SQLError("42703", f'column "{col.name}" does not exist')


def build(source: SrfSource, ctx: Any) -> tuple[list[dict[str, Any]], TableDef]:
    """Materialize the SRF's rows as documents and a synthetic single-source
    ``TableDef`` describing the value (and optional ``WITH ORDINALITY``) columns."""
    values, tag = _values_and_tag(source.node, ctx)
    default = _default_name(source.node)
    # A single-column SRF's column takes the explicit column alias, else the table
    # alias (Postgres: ``FROM generate_series(1,5) AS g`` names the column ``g``),
    # else the function name.
    value_col = (
        source.column_aliases[0] if source.column_aliases else (source.table_alias or default)
    )
    columns = [Column(name=value_col, type_tag=tag, field=value_col, pk=False, nullable=True)]
    ord_col = None
    if source.ordinality:
        ord_col = source.column_aliases[1] if len(source.column_aliases) > 1 else "ordinality"
        columns.append(
            Column(name=ord_col, type_tag="int8", field=ord_col, pk=False, nullable=True)
        )

    rows: list[dict[str, Any]] = []
    for i, v in enumerate(values, start=1):
        row = {value_col: v}
        if ord_col is not None:
            row[ord_col] = i
        rows.append(row)

    table_name = source.table_alias or default
    return rows, TableDef(name=table_name, collection=table_name, columns=columns)
