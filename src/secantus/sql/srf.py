"""Set-returning functions as a query row source (#125).

Two shapes are handled here:

* a base-less ``FROM`` table function —
  ``SELECT * FROM generate_series(1, 5) [WITH ORDINALITY] [AS t(a, ord)]`` and
  the same for ``unnest`` / ``jsonb_array_elements`` / ``jsonb_object_keys`` /
  ``regexp_split_to_table``, plus the two-column record SRFs ``jsonb_each`` /
  ``jsonb_each_text`` (``key`` / ``value``); and
* a base-less SELECT-list SRF — ``SELECT generate_series(1, 5)``.

Both materialize the generated rows and run the rest of the query (projection /
``WHERE`` / ``ORDER BY`` / ``LIMIT``) over an in-memory table, reusing the normal
select planner + executor. The ``FROM t, <srf>(...)`` *join* form (one row per
outer row × element) stays in the pipeline planner's ``_unnest_join_stage``.
"""

from __future__ import annotations

import datetime as _dt
import re
from decimal import Decimal, InvalidOperation
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
        "jsonb_each",
        "json_each",
        "jsonb_each_text",
        "json_each_text",
        "pg_get_keywords",
    }
)

# Record-returning SRFs: each row is a ``(key, value)`` pair, so the source has
# two columns (default-named ``key`` / ``value``) rather than one.
_RECORD_SRFS = frozenset(
    {
        "jsonb_each",
        "json_each",
        "jsonb_each_text",
        "json_each_text",
        # information_schema._pg_expandarray(arr) -> (x anyelement, n int):
        # each element with its 1-based subscript. pgjdbc's DatabaseMetaData
        # index/PK queries use it. NOTE: the row shape below is right, but the
        # CALL SITES pgjdbc emits are not recognised yet — a schema-qualified
        # name in FROM position, and the composite-value form
        # ``(_pg_expandarray(x)).n`` — so this is not reachable from those
        # queries. See tasks/backlog.md.
        "_pg_expandarray",
        # pg_get_keywords() -> (word, catcode, barelabel, catdesc, baredesc):
        # the server's keyword list; pgjdbc's getSQLKeywords string_aggs it.
        "pg_get_keywords",
    }
)

#: Per-record-SRF default column names (the jsonb_each family is key/value).
_RECORD_SRF_COLUMNS = {
    "_pg_expandarray": ["x", "n"],
    "pg_get_keywords": ["word", "catcode", "barelabel", "catdesc", "baredesc"],
}

#: The PG-specific keyword list served by ``pg_get_keywords()`` — the words a
#: JDBC client can't find in SQL:2003 (pgjdbc filters that standard set out and
#: asserts ``reindex`` survives). catcode: R reserved, U unreserved.
_PG_KEYWORDS: tuple[tuple[str, str], ...] = (
    ("abort", "U"),
    ("analyse", "R"),
    ("analyze", "R"),
    ("attach", "U"),
    ("backward", "U"),
    ("cluster", "U"),
    ("comment", "U"),
    ("concurrently", "R"),
    ("conflict", "U"),
    ("copy", "U"),
    ("cost", "U"),
    ("csv", "U"),
    ("current_catalog", "R"),
    ("current_schema", "R"),
    ("delimiter", "U"),
    ("detach", "U"),
    ("discard", "U"),
    ("do", "R"),
    ("enum", "U"),
    ("explain", "U"),
    ("extension", "U"),
    ("family", "U"),
    ("forward", "U"),
    ("freeze", "R"),
    ("greatest", "U"),
    ("handler", "U"),
    ("header", "U"),
    ("ilike", "R"),
    ("immutable", "U"),
    ("inherit", "U"),
    ("inherits", "U"),
    ("isnull", "R"),
    ("lateral", "R"),
    ("least", "U"),
    ("limit", "R"),
    ("listen", "U"),
    ("load", "U"),
    ("lock", "U"),
    ("logged", "U"),
    ("mode", "U"),
    ("move", "U"),
    ("notify", "U"),
    ("notnull", "R"),
    ("nowait", "U"),
    ("off", "U"),
    ("offset", "R"),
    ("oids", "U"),
    ("owned", "U"),
    ("owner", "U"),
    ("parallel", "U"),
    ("passing", "U"),
    ("password", "U"),
    ("plans", "U"),
    ("policy", "U"),
    ("prepared", "U"),
    ("procedural", "U"),
    ("publication", "U"),
    ("refresh", "U"),
    ("reindex", "U"),
    ("rename", "U"),
    ("replica", "U"),
    ("reset", "U"),
    ("restart", "U"),
    ("returning", "R"),
    ("rule", "U"),
    ("setof", "U"),
    ("share", "U"),
    ("show", "U"),
    ("skip", "U"),
    ("snapshot", "U"),
    ("stable", "U"),
    ("standalone", "U"),
    ("storage", "U"),
    ("stored", "U"),
    ("strict", "U"),
    ("subscription", "U"),
    ("support", "U"),
    ("sysid", "U"),
    ("tables", "U"),
    ("tablespace", "U"),
    ("truncate", "U"),
    ("trusted", "U"),
    ("unlisten", "U"),
    ("unlogged", "U"),
    ("vacuum", "U"),
    ("valid", "U"),
    ("validate", "U"),
    ("validator", "U"),
    ("variadic", "R"),
    ("verbose", "R"),
    ("volatile", "U"),
    ("whitespace", "U"),
    ("xmlattributes", "U"),
    ("xmlconcat", "U"),
    ("xmlelement", "U"),
    ("xmlexists", "U"),
    ("xmlforest", "U"),
    ("xmlparse", "U"),
    ("xmlpi", "U"),
    ("xmlroot", "U"),
    ("xmlserialize", "U"),
    ("yes", "U"),
)


def _is_record_srf(node: exp.Expression) -> bool:
    return (
        isinstance(node, exp.Anonymous)
        and str(node.this).rsplit(".", 1)[-1].lower() in _RECORD_SRFS
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


def _is_from_callable(node: exp.Expression) -> bool:
    """Acceptance for FROM position ONLY — wider than ``_is_srf_node``.

    Named SRFs, plus ANY function call in FROM position: pgjdbc's
    CallableStatement rewrites ``{? = call f(?)}`` into
    ``select * from f($1) as result``, so a user-defined function in
    FROM must evaluate as a one-row source (``_values_and_tag`` falls
    back to the scalar evaluator, which resolves catalog UDFs and
    raises 42883 for genuinely unknown names — a FROM item that parses
    as a call is never a real table). ``exp.Func`` rather than
    ``exp.Anonymous`` because sqlglot parses some calls into dedicated
    nodes — ``now()`` -> CurrentTimestamp — and pgjdbc's ``{call now()}``
    must still work. A FROM-less ``SELECT f()`` projection must NOT take
    this path (it would reroute every ordinary scalar call), which is why
    ``fromless_projection`` keeps the strict predicate."""
    return _is_srf_node(node) or isinstance(node, exp.Func)


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
    if isinstance(src, exp.Table) and _is_from_callable(src.this):
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
    # Record SRFs (``jsonb_each`` …) return a two-column set; a base-less
    # ``SELECT jsonb_each(x)`` would yield a single *composite* column in Postgres,
    # which we don't model — only the ``FROM jsonb_each(x)`` table form is supported.
    # ``generate_series(1, 2)::int4`` — a cast wrapping the SRF applies to each
    # generated element; unwrapped here so the SRF machinery sees the call
    # (the cast tag is re-applied per element in ``_values_and_tag``).
    if not _is_srf_node(target.this if isinstance(target, exp.Cast) else target) or _is_record_srf(
        target.this if isinstance(target, exp.Cast) else target
    ):
        return None
    return SrfSource(target, False, None, [alias] if alias else [])


# --------------------------------------------------------------------------- #
# Row generation
# --------------------------------------------------------------------------- #


def _default_name(node: exp.Expression) -> str:
    if isinstance(node, exp.Cast):
        return _default_name(node.this)
    if isinstance(node, exp.ExplodingGenerateSeries):
        return "generate_series"
    if isinstance(node, (exp.Unnest, exp.Explode)):
        return "unnest"
    if isinstance(node, exp.Anonymous):
        base = str(node.this).rsplit(".", 1)[-1].lower()
        return "unnest" if base == "unnest" else base
    if isinstance(node, exp.Func):
        return node.sql_name().lower()
    return "?column?"


def _tag_for_value(value: Any) -> str:
    import datetime as _dt
    from decimal import Decimal

    if isinstance(value, bool):
        return "bool"
    if isinstance(value, int):
        return "int4" if -(2**31) <= value < 2**31 else "int8"
    if isinstance(value, float):
        return "float8"
    if isinstance(value, Decimal):
        return "numeric"
    if isinstance(value, _dt.datetime):
        return "timestamptz" if value.tzinfo is not None else "timestamp"
    if isinstance(value, _dt.date):
        return "date"
    if isinstance(value, str):
        return "text"
    return "any"


def _values_and_tag(
    node: exp.Expression, ctx: Any, describe_only: bool = False
) -> tuple[list[Any], str]:
    """Generate the SRF's element values plus the value column's type tag.

    ``describe_only`` derives the column tag WITHOUT invoking catalog UDFs —
    extended-protocol Describe must never run a side-effecting function body
    (pgjdbc's batched ``{call f(?)}`` executed every insert twice: once at
    Describe, once at Execute)."""
    from secantus.sql import scalar, typemap

    if isinstance(node, exp.Cast):
        # ``srf(...)::tag`` — generate, then coerce each element to the target.
        values, _tag = _values_and_tag(node.this, ctx, describe_only)
        cast_tag = typemap.type_tag_for_sql(node.to)
        if cast_tag is None:
            return values, "any"
        return [v if v is None else typemap.coerce(v, cast_tag) for v in values], cast_tag

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
    if isinstance(node, exp.Anonymous):
        # Not a built-in SRF: evaluate as a scalar call (catalog UDFs included
        # — pgjdbc's ``select * from f($1) as result`` callable shape) and
        # yield its single row. The column's type tag comes from the
        # function's declared return type — pgjdbc's CallableStatement
        # cross-checks the result column's OID against the registered OUT
        # type and refuses a mismatch. RETURNS SETOF stays unsupported.
        tag = "any"
        catalog = getattr(ctx, "catalog", None)
        if catalog is not None:
            fname = str(node.this).rsplit(".", 1)[-1].lower()
            udf = catalog.get_function(ctx.db, fname, len(node.expressions or []))
            if udf is not None and udf.get("return_tag"):
                tag = udf["return_tag"]
        if describe_only:
            return [], tag
        value = scalar.evaluate(node, _empty_scope, ctx)
        return [value], tag
    if isinstance(node, exp.Func):
        # A call sqlglot parsed into a dedicated node (``now()`` ->
        # CurrentTimestamp, ``version()`` -> CurrentVersion): session-info
        # functions first, then the general scalar evaluator, tagging the
        # column from the value.
        session = getattr(ctx, "session", None)
        if session is not None:
            from secantus.sql import functions

            try:
                _name, value, tag = functions.evaluate_scalar(node, session)
                return [value], tag
            except errors.SQLError:
                pass
        value = scalar.evaluate(node, _empty_scope, ctx)
        return [value], _tag_for_value(value)
    raise errors.feature_not_supported(f"unsupported set-returning function: {node.sql()}")


def _record_values(node: exp.Expression, ctx: Any) -> tuple[list[tuple[Any, Any]], list[str]]:
    """Rows and per-column type tags for a record SRF (``jsonb_each`` family) —
    one ``(key, value)`` tuple per object member, key ``text`` and value ``json``
    (``jsonb_each``) or ``text`` (``jsonb_each_text``)."""
    from secantus.sql import scalar

    name = str(node.this).rsplit(".", 1)[-1].lower()
    arg = node.expressions[0] if node.expressions else None
    value = scalar.evaluate(arg, _empty_scope, ctx) if arg is not None else None
    if name == "_pg_expandarray":
        items_list = list(value) if isinstance(value, (list, tuple)) else []
        return [(v, i) for i, v in enumerate(items_list, start=1)], ["any", "int4"]
    if name == "pg_get_keywords":
        return [(word, code, False, None, None) for word, code in _PG_KEYWORDS], [
            "text",
            "text",
            "bool",
            "text",
            "text",
        ]
    doc = _as_json(value)
    items = list(doc.items()) if isinstance(doc, dict) else []
    if name in ("jsonb_each_text", "json_each_text"):
        return [(k, _jsonb_to_text(v)) for k, v in items], ["text", "text"]
    return [(k, v) for k, v in items], ["text", "json"]


def _jsonb_to_text(v: Any) -> Any:
    """A jsonb value rendered as text, the way ``jsonb_each_text`` / ``->>`` do:
    strings stay verbatim, booleans lower-case, containers as compact JSON, NULL
    stays SQL NULL."""
    if v is None:
        return None
    if isinstance(v, str):
        return v
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (dict, list)):
        import json

        return json.dumps(v)
    return str(v)


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


def _coerce_series_bound(val: Any) -> Any:
    """Parse a numeric-looking string bound into a number.

    Only strings are touched, and only when they parse cleanly — anything else
    is returned unchanged so the caller's type check still rejects genuinely
    unsupported bounds with its own error. Integers are preferred over Decimal
    so the common `generate_series(1, $1)` yields int8 rows rather than numeric.
    """
    if not isinstance(val, str):
        return val
    text = val.strip()
    if not text:
        return val
    try:
        return int(text)
    except ValueError:
        pass
    try:
        return Decimal(text)
    except (InvalidOperation, ValueError):
        return val


def _generate_series(start: Any, stop: Any, step: Any) -> tuple[list[Any], str]:
    """``generate_series(start, stop[, step])`` — inclusive of both ends. Numeric
    ranges (int / numeric step) and date / timestamp ranges (an ``interval``
    step) are both supported."""
    if start is None or stop is None:
        # Describe-time: a parameter bound is still unbound (None). Type from
        # the bounds we DO know, with the same int4/int8 rule as below, so the
        # RowDescription a Describe reports matches the DataRow a later
        # Execute sends. (A later $1 outside int32 range would make execute
        # rows int8 under an int4 describe — real PG errors on that input
        # outright, having typed the parameter int4 at parse.)
        known = [
            b for b in (_coerce_series_bound(start), _coerce_series_bound(stop)) if b is not None
        ]
        if all(isinstance(b, int) and -(2**31) <= b < 2**31 for b in known):
            return [], "int4"
        return [], "int8"
    # An untyped parameter (`generate_series(1, $1)` with `$1` sent as text)
    # arrives as a string: the wire gave no type OID, so nothing upstream
    # coerced it. Postgres infers the parameter's type from the argument
    # position and parses it as an integer, so a numeric-looking string is a
    # number here too. Without this, pgx's `ensureConnValid` helper — which
    # runs exactly that query and is called at the end of 66 pgconn tests —
    # failed, taking otherwise-passing tests down with it.
    #
    # Runs before the temporal branch so a coerced bound is what that branch
    # sees. Note this does NOT rescue a quoted third argument
    # (`generate_series(1, 10, '3')`): sqlglot parses that into an `Interval`
    # node at parse time, so the step arrives already an interval and never
    # reaches this coercion. That is a separate parser-level quirk.
    start = _coerce_series_bound(start)
    stop = _coerce_series_bound(stop)
    step = _coerce_series_bound(step)
    if _is_temporal(start) or intervals.is_interval(step):
        return _generate_series_temporal(start, stop, step)
    if not isinstance(start, (int, float, Decimal)) or not isinstance(stop, (int, float, Decimal)):
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
    if all(isinstance(v, int) for v in out):
        # PG picks the overload from the ARGUMENT types: int4 bounds yield
        # int4 rows, an int8 bound yields int8. The wire gives us values, not
        # declared types, so int32-range bounds mean int4 (an explicit
        # small-valued ::int8 bound diverges — PG would say int8; accepted).
        int4_bounds = all(
            b is None or (isinstance(b, int) and -(2**31) <= b < 2**31) for b in (start, stop, step)
        )
        tag = "int4" if int4_bounds else "int8"
    else:
        tag = "numeric"
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


def _build_record(source: SrfSource, ctx: Any) -> tuple[list[dict[str, Any]], TableDef]:
    """Materialize a record SRF (``jsonb_each`` family): two columns, default-named
    ``key`` / ``value``, optionally renamed by ``AS t(k, v)`` and extended by
    ``WITH ORDINALITY``."""
    pairs, tags = _record_values(source.node, ctx)
    srf_name = str(source.node.this).rsplit(".", 1)[-1].lower()
    default_names = list(_RECORD_SRF_COLUMNS.get(srf_name, ["key", "value"]))
    # ``AS t(k, v)`` renames the columns; the bare table alias does not (unlike a
    # single-column SRF, where ``AS g`` names the lone column).
    names = list(source.column_aliases) if source.column_aliases else list(default_names)
    names += default_names[len(names) :]  # pad if fewer aliases than columns
    width = len(default_names)
    columns = [
        Column(name=names[i], type_tag=tags[i], field=names[i], pk=False, nullable=True)
        for i in range(width)
    ]
    ord_col = None
    if source.ordinality:
        ord_col = (
            source.column_aliases[width] if len(source.column_aliases) > width else "ordinality"
        )
        columns.append(
            Column(name=ord_col, type_tag="int8", field=ord_col, pk=False, nullable=True)
        )
    rows: list[dict[str, Any]] = []
    for i, rec in enumerate(pairs, start=1):
        row = {names[j]: rec[j] for j in range(width)}
        if ord_col is not None:
            row[ord_col] = i
        rows.append(row)
    table_name = source.table_alias or _default_name(source.node)
    return rows, TableDef(name=table_name, collection=table_name, columns=columns)


def build(
    source: SrfSource, ctx: Any, describe_only: bool = False
) -> tuple[list[dict[str, Any]], TableDef]:
    """Materialize the SRF's rows as documents and a synthetic single-source
    ``TableDef`` describing the value (and optional ``WITH ORDINALITY``) columns.

    ``describe_only`` returns the shape without invoking catalog UDFs (empty
    rows for those) — see ``_values_and_tag``."""
    if _is_record_srf(source.node):
        return _build_record(source, ctx)
    values, tag = _values_and_tag(source.node, ctx, describe_only)
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
