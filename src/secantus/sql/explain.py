"""``EXPLAIN`` for the SQL layer (#122).

Postgres' ``EXPLAIN [ANALYZE] [(options)] <statement>`` returns a result set
with a single ``QUERY PLAN`` text column, one row per plan line. SecantusDB
executes SQL against the Mongo storage, so the plan we render mirrors the
storage's own routing decision — an ``IXSCAN`` on a covering index shows as an
*Index Scan*, everything else as a *Seq Scan*. The IXSCAN/COLLSCAN call is the
authoritative one from ``Storage.explain_plan`` (the same router ``find_matching``
uses), so ``EXPLAIN`` never claims an index the real query wouldn't use.

Scope: single-relation ``SELECT`` / ``UPDATE`` / ``DELETE`` and ``INSERT`` get a
faithful scan node with ``Index Cond:`` / ``Filter:`` detail. Pipeline queries
(JOIN / GROUP BY / aggregates / set-ops) get a coarser node tree — the top
operation over the base-collection scan(s) — since Mongo runs them as an
aggregation pipeline rather than the plan-node zoo Postgres would build.

``ANALYZE`` actually runs the statement (as Postgres does) and annotates the top
node with ``actual rows``; there is no real per-node timing. ``FORMAT JSON``
emits Postgres' single-row JSON plan; ``FORMAT TEXT`` (the default) emits the
indented tree. ``VERBOSE`` / ``COSTS`` / ``BUFFERS`` / ``SETTINGS`` are accepted;
cost figures are placeholders (no statistics engine).
"""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from typing import Any

from sqlglot import exp

from . import errors, planner, reflect
from .result import ColumnDesc, SQLResult
from .typemap import PG_OID

# EXPLAIN options: the optional leading ``ANALYZE`` word and/or a
# parenthesised ``(opt [value], ...)`` list, before the statement.
_PAREN_OPTS = re.compile(r"^\s*\((?P<opts>[^)]*)\)\s*(?P<rest>.*)$", re.IGNORECASE | re.DOTALL)
_LEADING_WORDS = re.compile(r"^\s*(ANALYZE|ANALYSE|VERBOSE)\b", re.IGNORECASE)


def parse_options(tail: str) -> tuple[dict[str, Any], str]:
    """Split an ``EXPLAIN`` tail into (options, inner-statement-SQL).

    Handles both the bare ``EXPLAIN ANALYZE VERBOSE <stmt>`` word form and the
    parenthesised ``EXPLAIN (ANALYZE, FORMAT JSON) <stmt>`` form.
    """
    opts: dict[str, Any] = {"analyze": False, "verbose": False, "format": "text"}
    # Bare leading words (ANALYZE / VERBOSE), any order.
    while True:
        m = _LEADING_WORDS.match(tail)
        if m is None:
            break
        word = m.group(1).upper()
        opts["analyze" if word in ("ANALYZE", "ANALYSE") else "verbose"] = True
        tail = tail[m.end() :]
    # Parenthesised option list.
    m = _PAREN_OPTS.match(tail)
    if m is not None:
        _parse_paren_opts(m.group("opts"), opts)
        tail = m.group("rest")
    inner = tail.strip().rstrip(";").strip()
    if not inner:
        raise errors.syntax_error("EXPLAIN requires a statement")
    return opts, inner


def _parse_paren_opts(body: str, opts: dict[str, Any]) -> None:
    for item in body.split(","):
        toks = item.split()
        if not toks:
            continue
        key = toks[0].upper()
        val = toks[1].upper() if len(toks) > 1 else "TRUE"
        if key == "ANALYZE":
            opts["analyze"] = val != "FALSE"
        elif key == "VERBOSE":
            opts["verbose"] = val != "FALSE"
        elif key == "FORMAT":
            if val not in ("TEXT", "JSON"):
                raise errors.feature_not_supported(
                    f"EXPLAIN FORMAT {toks[1] if len(toks) > 1 else ''} is not supported"
                )
            opts["format"] = val.lower()
        # COSTS / BUFFERS / SETTINGS / TIMING / WAL / SUMMARY accepted, ignored.


def explain(
    tail: str,
    storage: Any,
    db: str,
    catalog: Any,
    session: Any,
    *,
    run_stmt: Callable[[exp.Expression], SQLResult],
) -> SQLResult:
    """Build the ``EXPLAIN`` result for ``tail`` (everything after ``EXPLAIN``).

    ``run_stmt`` executes a parsed statement — used only for ``ANALYZE`` (and to
    keep this module free of a circular import back to ``engine``).
    """
    opts, inner = parse_options(tail)
    stmts = planner.parse(inner)
    if len(stmts) != 1:
        raise errors.syntax_error("EXPLAIN expects a single statement")
    stmt = stmts[0]
    node = _build_node(stmt, storage, db, catalog, session)
    if opts["analyze"]:
        result = run_stmt(stmt)
        node["actual_rows"] = result.rowcount if result.rows == [] else len(result.rows)
    if opts["format"] == "json":
        rows = [(json.dumps([{"Plan": _json_node(node)}], indent=2),)]
    else:
        rows = [(line,) for line in _text_lines(node, opts)]
    return SQLResult(
        command_tag="EXPLAIN",
        columns=[ColumnDesc("QUERY PLAN", "text", PG_OID["text"])],
        rows=rows,
        rowcount=len(rows),
    )


# --------------------------------------------------------------------------- #
# Plan-node construction
# --------------------------------------------------------------------------- #


def _build_node(stmt: exp.Expression, storage: Any, db: str, catalog: Any, session: Any) -> dict:
    if isinstance(stmt, exp.Select):
        return _select_node(stmt, storage, db, catalog, session)
    if isinstance(stmt, exp.SetOperation):
        left = _build_node(stmt.this, storage, db, catalog, session)
        right = _build_node(stmt.expression, storage, db, catalog, session)
        return {"node": "Append", "children": [left, right]}
    if isinstance(stmt, exp.Insert):
        target = stmt.this
        name = target.this.name if isinstance(target, exp.Schema) else target.name
        node: dict[str, Any] = {"node": "Insert", "relation": name}
        src = stmt.expression
        if isinstance(src, (exp.Select, exp.SetOperation)):
            node["children"] = [_build_node(src, storage, db, catalog, session)]
        else:
            nrows = len(src.expressions) if isinstance(src, exp.Values) else 1
            node["children"] = [{"node": "Result", "rows": nrows}]
        return node
    if isinstance(stmt, exp.Update):
        return _modify_node("Update", stmt, storage, db, catalog)
    if isinstance(stmt, exp.Delete):
        return _modify_node("Delete", stmt, storage, db, catalog)
    return {"node": type(stmt).__name__}


def _select_node(stmt: exp.Select, storage: Any, db: str, catalog: Any, session: Any) -> dict:
    table_node = stmt.find(exp.Table)
    if table_node is None:
        return {"node": "Result", "rows": 1}
    if planner.select_needs_pipeline(stmt):
        return _pipeline_node(stmt, storage, db, catalog)
    table = catalog.get(db, table_node.name) or reflect.reflect(storage, db, table_node.name)
    if table is None:
        raise errors.undefined_table(table_node.name)
    where_sql = _where_sql(stmt.args.get("where"))
    if planner.where_needs_per_row(stmt, table):
        # A correlated / EXISTS WHERE can't push down — always a scan + filter.
        return _scan_node(storage, db, table, {}, where_sql, force_collscan=True)
    subctx = planner.SubqueryCtx(storage=storage, db=db, catalog=catalog, session=session)
    plan = planner.plan_select(stmt, table, subctx)
    return _scan_node(storage, db, table, plan.filter, where_sql, sort=_sort_spec(plan.order))


def _modify_node(kind: str, stmt: exp.Expression, storage: Any, db: str, catalog: Any) -> dict:
    table_node = stmt.find(exp.Table)
    table = catalog.get(db, table_node.name) or reflect.reflect(storage, db, table_node.name)
    if table is None:
        raise errors.undefined_table(table_node.name)
    plan = (
        planner.plan_update(stmt, table) if kind == "Update" else planner.plan_delete(stmt, table)
    )
    where_sql = _where_sql(stmt.args.get("where"))
    scan = _scan_node(storage, db, table, plan.filter, where_sql)
    return {"node": kind, "relation": table.name, "children": [scan]}


def _pipeline_node(stmt: exp.Select, storage: Any, db: str, catalog: Any) -> dict:
    """A JOIN / GROUP BY / aggregate query runs as a Mongo aggregation pipeline;
    render a coarse top node over the base-collection scan."""
    try:
        plan = planner.plan_pipeline_select(stmt, db, catalog, storage)
        base = getattr(plan, "base_collection", None)
    except Exception:
        base = None
    grouped = bool(stmt.args.get("group") or stmt.args.get("having"))
    has_join = bool(stmt.args.get("joins"))
    if grouped:
        label = "GroupAggregate"
    elif has_join:
        label = "Nested Loop"
    elif _has_aggregate(stmt):
        label = "Aggregate"
    else:
        label = "Subquery Scan"
    if base:
        child = {"node": "Seq Scan", "relation": base, "rows": _estimate_rows(storage, db, base)}
    else:
        child = {"node": "Result"}
    return {"node": label, "children": [child], "pipeline": True}


def _scan_node(
    storage: Any,
    db: str,
    table: Any,
    filt: dict,
    where_sql: str | None,
    *,
    sort: dict | None = None,
    force_collscan: bool = False,
) -> dict:
    rows = _estimate_rows(storage, db, table.collection)
    decision = (
        None if force_collscan else _index_decision(storage, db, table.collection, filt, sort)
    )
    if decision and decision.get("kind") == "IXSCAN":
        node: dict[str, Any] = {
            "node": "Index Scan",
            "relation": table.name,
            "index_name": decision.get("index_name"),
            "rows": rows,
        }
        if decision.get("direction") == "backward":
            node["scan_direction"] = "Backward"
        if where_sql:
            node["index_cond"] = where_sql
        return node
    node = {"node": "Seq Scan", "relation": table.name, "rows": rows}
    if where_sql:
        node["filter"] = where_sql
    return node


def _index_decision(storage: Any, db: str, coll: str, filt: dict, sort: dict | None) -> dict | None:
    """Ask the storage what ``find_matching`` would do. Prefer the authoritative
    ``Storage.explain_plan``; fall back to a leading-field heuristic over
    ``list_indexes`` for storages that don't expose it (e.g. the test double)."""
    explain_plan = getattr(storage, "explain_plan", None)
    if callable(explain_plan):
        try:
            return explain_plan(db, coll, filt or {}, sort=sort or None)
        except Exception:
            return {"kind": "COLLSCAN"}
    return _heuristic_decision(storage, db, coll, filt, sort)


def _heuristic_decision(storage: Any, db: str, coll: str, filt: dict, sort: dict | None) -> dict:
    fields = {f for f in (filt or {}) if not f.startswith("$")}
    if not fields and sort:
        fields = set(sort)
    if not fields:
        return {"kind": "COLLSCAN"}
    try:
        indexes = storage.list_indexes(db, coll)
    except Exception:
        return {"kind": "COLLSCAN"}
    for ix in indexes:
        key = ix.get("key") or {}
        leading = next(iter(key), None)
        if leading in fields:
            return {
                "kind": "IXSCAN",
                "index_name": ix.get("name"),
                "key_pattern": dict(key),
                "direction": "forward",
            }
    return {"kind": "COLLSCAN"}


def _estimate_rows(storage: Any, db: str, coll: str) -> int:
    counter = getattr(storage, "count_matching", None)
    if callable(counter):
        try:
            return int(counter(db, coll, {}))
        except Exception:
            pass
    try:
        return len(storage.find_matching(db, coll, {}))
    except Exception:
        return 0


def _sort_spec(order: list[tuple[str, int, bool]]) -> dict | None:
    """A single-field ORDER BY maps to a Mongo sort spec so ``explain_plan`` can
    consider sort-serving indexes; multi-field / empty falls back to None."""
    if len(order) == 1:
        field, direction, _nulls = order[0]
        return {field: direction}
    return None


def _where_sql(where: Any) -> str | None:
    if where is None:
        return None
    predicate = where.this if isinstance(where, exp.Where) else where
    return f"({predicate.sql(dialect='postgres')})"


def _has_aggregate(stmt: exp.Select) -> bool:
    return any(isinstance(e, (exp.AggFunc,)) or e.find(exp.AggFunc) for e in stmt.expressions)


# --------------------------------------------------------------------------- #
# Rendering — text tree
# --------------------------------------------------------------------------- #

_COST = "(cost=0.00..0.00 rows={rows} width=0)"


def _text_lines(node: dict, opts: dict) -> list[str]:
    lines: list[str] = []
    _emit(node, 0, is_child=False, lines=lines)
    return lines


def _emit(node: dict, indent: int, *, is_child: bool, lines: list[str]) -> None:
    pad = " " * indent
    arrow = "->  " if is_child else ""
    head = pad + arrow + _node_text(node) + "  " + _COST.format(rows=node.get("rows", 0))
    if "actual_rows" in node:
        head += f" (actual rows={node['actual_rows']} loops=1)"
    lines.append(head)
    detail_indent = indent + (6 if is_child else 2)
    dpad = " " * detail_indent
    if node.get("index_cond"):
        lines.append(f"{dpad}Index Cond: {node['index_cond']}")
    if node.get("filter"):
        lines.append(f"{dpad}Filter: {node['filter']}")
    for child in node.get("children", []):
        _emit(child, detail_indent, is_child=True, lines=lines)


def _node_text(node: dict) -> str:
    label = node["node"]
    if node.get("scan_direction") == "Backward":
        label += " Backward"
    if label.startswith("Index Scan") and node.get("index_name"):
        text = f"Index Scan using {node['index_name']}"
    else:
        text = label
    if node.get("relation"):
        text += f" on {node['relation']}"
    return text


# --------------------------------------------------------------------------- #
# Rendering — JSON (Postgres' FORMAT JSON node shape)
# --------------------------------------------------------------------------- #


def _json_node(node: dict) -> dict:
    out: dict[str, Any] = {"Node Type": _json_node_type(node)}
    if node.get("scan_direction"):
        out["Scan Direction"] = node["scan_direction"]
    if node.get("index_name"):
        out["Index Name"] = node["index_name"]
    if node.get("relation"):
        out["Relation Name"] = node["relation"]
    out["Plan Rows"] = node.get("rows", 0)
    if node.get("index_cond"):
        out["Index Cond"] = node["index_cond"]
    if node.get("filter"):
        out["Filter"] = node["filter"]
    if "actual_rows" in node:
        out["Actual Rows"] = node["actual_rows"]
    children = node.get("children")
    if children:
        out["Plans"] = [_json_node(c) for c in children]
    return out


def _json_node_type(node: dict) -> str:
    label = node["node"]
    if label == "Index Scan" and node.get("scan_direction") == "Backward":
        return "Index Scan"
    return label
