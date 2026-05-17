"""Oplog window inspector page.

Browses the synthetic ``local.oplog.rs`` collection introduced in
v0.5.1b17. The page renders a paged entry browser with a window
selector, ``op`` / ``ns`` filters, and an expandable JSON body per
row. Auto-refreshes every 5 s on the same cadence as ``/connections``
and ``/cursors``.

* ``GET /oplog`` — full page chrome + the initial rows.
* ``GET /oplog/_rows`` — partial used by the ``hx-trigger`` poll and
  by every filter / window submit.
"""

from __future__ import annotations

from typing import Any

from bson import json_util
from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError

router = APIRouter()

# Window-selector options the user picks from. Limit applies to the
# ``find().sort("ts", -1).limit(N)`` query — newest entries first.
_WINDOW_LIMITS = [50, 500, 5000]
_DEFAULT_LIMIT = 50

# Op codes mongod (and SecantusDB) puts on each entry. Renders as a
# row of checkbox filters at the top of the page.
_ALL_OPS = ["i", "u", "d", "c", "n"]


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _parse_limit(raw: str | None) -> int:
    if raw is None:
        return _DEFAULT_LIMIT
    try:
        n = int(raw)
    except ValueError:
        return _DEFAULT_LIMIT
    if n not in _WINDOW_LIMITS:
        # Allow custom values but clamp into a sane range.
        return max(1, min(n, 50_000))
    return n


def _parse_ops(values: list[str]) -> list[str]:
    """Parse the ``op`` query-string list. Empty means "all"."""
    picked = [v for v in values if v in _ALL_OPS]
    return picked


def _build_filter(ops: list[str], ns_substring: str) -> dict[str, Any]:
    f: dict[str, Any] = {}
    if ops:
        f["op"] = {"$in": ops}
    if ns_substring:
        # Substring match via regex on ``ns``. The user types "appdb"
        # to see all collections under appdb, or "appdb.users" for one
        # specific namespace. Anchor with ``re.escape`` so dots and
        # other regex metachars stay literal.
        import re as _re

        f["ns"] = {"$regex": _re.escape(ns_substring)}
    return f


def _format_row(entry: dict[str, Any]) -> dict[str, Any]:
    """Shape one oplog entry for the row template.

    Decoded oplog rows carry binary-flavoured fields (Timestamp, UUID,
    BSON Binary) that aren't trivially JSON-serialisable, hence
    ``json_util`` for the body and a string ``ts`` so the row layout
    stays predictable. ``op_label`` is the long form of the
    single-letter op for the badge tooltip.
    """
    op = entry.get("op", "")
    return {
        "ts": str(entry.get("ts", "")),
        "op": op,
        "op_label": {
            "i": "insert",
            "u": "update",
            "d": "delete",
            "c": "command",
            "n": "noop",
        }.get(op, op),
        "ns": entry.get("ns", ""),
        "json": json_util.dumps(entry, indent=2),
    }


def _collect_rows(request: Request) -> tuple[list[dict[str, Any]], int, str | None]:
    raw_limit = request.query_params.get("limit")
    limit = _parse_limit(raw_limit)
    ops = _parse_ops(request.query_params.getlist("op"))
    ns_q = (request.query_params.get("ns") or "").strip()
    filter_doc = _build_filter(ops, ns_q)

    error: str | None = None
    rows: list[dict[str, Any]] = []
    try:
        client = request.app.state.mongo._get_client()
        cur = client["local"]["oplog.rs"].find(filter_doc).sort("ts", -1).limit(limit)
        rows = [_format_row(dict(d)) for d in cur]
    except MongoError as exc:
        error = str(exc)
    return rows, limit, error


@router.get("/oplog", response_class=HTMLResponse)
def oplog_page(request: Request) -> HTMLResponse:
    rows, limit, error = _collect_rows(request)
    ops = _parse_ops(request.query_params.getlist("op"))
    ns_q = (request.query_params.get("ns") or "").strip()
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/oplog.html",
        {
            "title": "Oplog",
            "active": "oplog",
            "rows": rows,
            "limit": limit,
            "window_limits": _WINDOW_LIMITS,
            "all_ops": _ALL_OPS,
            "selected_ops": set(ops),
            "ns_query": ns_q,
            "error": error,
        },
    )


@router.get("/oplog/_rows", response_class=HTMLResponse)
def oplog_rows(request: Request) -> HTMLResponse:
    rows, limit, _error = _collect_rows(request)
    ops = _parse_ops(request.query_params.getlist("op"))
    ns_q = (request.query_params.get("ns") or "").strip()
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/oplog_rows.html",
        {
            "rows": rows,
            "limit": limit,
            "selected_ops": set(ops),
            "ns_query": ns_q,
        },
    )
