"""Connections + cursors pages.

Both views read from ``currentOp``. Connections are the
``type: "op"`` records (one per active client connection); cursors are
the ``type: "idleCursor"`` records (one per live tailable / batched
cursor). Connection-close is deferred until ``killOp`` lands as a
proper command — for now the connections table is read-only. Cursors
support kill via the existing wire-level ``killCursors``.
"""

from __future__ import annotations

import datetime as _dt
from typing import Any

from fastapi import APIRouter, HTTPException, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin import capabilities
from secantus.admin.client import MongoError

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _split(inprog: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Partition currentOp entries into (connections, cursors)."""
    conns = [e for e in inprog if e.get("type") == "op"]
    cursors = [e for e in inprog if e.get("type") == "idleCursor"]
    return conns, cursors


def _format_iso(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, _dt.datetime):
        return value.isoformat()
    return ""


def _connection_rows(request: Request) -> tuple[list[dict[str, Any]], str | None]:
    rows: list[dict[str, Any]] = []
    try:
        inprog = request.app.state.mongo.current_op()
    except MongoError as exc:
        return rows, str(exc)
    conns, _ = _split(inprog)
    for c in conns:
        rows.append(
            {
                "conn_id": int(c.get("connectionId", 0) or 0),
                "client": c.get("client", "") or "",
                "user": (c.get("effectiveUsers") or [{}])[0],
                "op": c.get("op", "") or "",
                "active": bool(c.get("active", False)),
                "opened_at": _format_iso(c.get("currentOpTime")),
            }
        )
    rows.sort(key=lambda r: r["conn_id"])
    return rows, None


def _cursor_rows(request: Request) -> tuple[list[dict[str, Any]], str | None]:
    rows: list[dict[str, Any]] = []
    try:
        inprog = request.app.state.mongo.current_op()
    except MongoError as exc:
        return rows, str(exc)
    _, cursors = _split(inprog)
    for c in cursors:
        rows.append(
            {
                "cursor_id": int(c.get("cursorId", 0) or 0),
                "ns": c.get("ns", "") or "",
                "tailable": bool(c.get("tailable", False)),
                "await_data": bool(c.get("awaitData", False)),
                "killable": bool(c.get("ns")),
            }
        )
    rows.sort(key=lambda r: r["cursor_id"])
    return rows, None


@router.get("/connections", response_class=HTMLResponse)
def connections_page(request: Request) -> HTMLResponse:
    rows, error = _connection_rows(request)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/connections.html",
        {
            "title": "Connections",
            "active": "connections",
            "rows": rows,
            "error": error,
        },
    )


@router.get("/connections/_rows", response_class=HTMLResponse)
def connections_rows(request: Request) -> HTMLResponse:
    rows, _ = _connection_rows(request)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/connections_rows.html",
        {"rows": rows},
    )


@router.get(
    "/connections/{conn_id}/kill-confirm",
    response_class=HTMLResponse,
)
def kill_connection_confirm(request: Request, conn_id: int) -> HTMLResponse:
    # Look up the connection's client address for the modal copy so
    # the user has context for the typed-confirm. Falls back gracefully
    # if the connection just disappeared between the page render and
    # the modal click.
    rows, _ = _connection_rows(request)
    client = ""
    for r in rows:
        if r["conn_id"] == conn_id:
            client = r["client"]
            break
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/connection_kill_modal.html",
        {"conn_id": conn_id, "client": client or "(disconnected)"},
    )


@router.delete("/connections/{conn_id}", response_class=HTMLResponse)
def kill_connection(request: Request, conn_id: int) -> HTMLResponse:
    try:
        request.app.state.mongo.kill_connection(conn_id)
    except MongoError as exc:
        if capabilities.is_command_not_found(exc):
            capabilities.record_unsupported(request.app, "kill_op")
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    return HTMLResponse("", headers={"HX-Trigger": "connection-killed"})


@router.get("/cursors", response_class=HTMLResponse)
def cursors_page(request: Request) -> HTMLResponse:
    rows, error = _cursor_rows(request)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/cursors.html",
        {
            "title": "Cursors",
            "active": "cursors",
            "rows": rows,
            "error": error,
        },
    )


@router.get("/cursors/_rows", response_class=HTMLResponse)
def cursors_rows(request: Request) -> HTMLResponse:
    rows, _ = _cursor_rows(request)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/cursors_rows.html",
        {"rows": rows},
    )


@router.get(
    "/cursors/{cursor_id}/kill-confirm",
    response_class=HTMLResponse,
)
def kill_cursor_confirm(request: Request, cursor_id: int) -> HTMLResponse:
    ns = request.query_params.get("ns") or ""
    if not ns:
        raise HTTPException(status_code=400, detail="ns query parameter required")
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/cursor_kill_modal.html",
        {"cursor_id": cursor_id, "ns": ns},
    )


@router.delete("/cursors/{cursor_id}", response_class=HTMLResponse)
def kill_cursor(request: Request, cursor_id: int) -> HTMLResponse:
    ns = request.query_params.get("ns") or ""
    if not ns:
        raise HTTPException(status_code=400, detail="ns query parameter required")
    try:
        request.app.state.mongo.kill_cursor(ns, cursor_id)
    except MongoError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    return HTMLResponse("", headers={"HX-Trigger": "cursor-killed"})
