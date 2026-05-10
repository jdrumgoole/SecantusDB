"""Dashboard route: KPI tiles polled via HTMX every 2 seconds.

The full page (``GET /``) renders the chrome plus an empty tile
container that HTMX immediately backfills via
``GET /_partials/dashboard-tiles``. Same partial is hit on every
2-second tick. Slice 4 will replace the polling with a WebSocket push.
"""

from __future__ import annotations

from fastapi import APIRouter, Form, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError
from secantus.admin.swap import SwapError, swap_target

router = APIRouter()


def _tiles_from_status(status: dict[str, object]) -> list[dict[str, object]]:
    conns = status.get("connections", {}) or {}
    opc = status.get("opcounters", {}) or {}
    network = status.get("network", {}) or {}
    return [
        {
            "label": "Uptime",
            "value": _format_uptime(int(status.get("uptime", 0) or 0)),
        },
        {
            "label": "Connections",
            "value": f"{conns.get('current', 0)} / {conns.get('totalCreated', 0)}",
            "hint": "current / total opened",
        },
        {
            "label": "Inserts",
            "value": f"{opc.get('insert', 0):,}",
        },
        {
            "label": "Queries",
            "value": f"{opc.get('query', 0):,}",
        },
        {
            "label": "Updates",
            "value": f"{opc.get('update', 0):,}",
        },
        {
            "label": "Deletes",
            "value": f"{opc.get('delete', 0):,}",
        },
        {
            "label": "Total commands",
            "value": f"{opc.get('command', 0):,}",
        },
        {
            "label": "Wire requests",
            "value": f"{network.get('numRequests', 0):,}",
        },
    ]


def _format_uptime(seconds: int) -> str:
    if seconds < 60:
        return f"{seconds}s"
    minutes, s = divmod(seconds, 60)
    if minutes < 60:
        return f"{minutes}m {s}s"
    hours, minutes = divmod(minutes, 60)
    if hours < 24:
        return f"{hours}h {minutes}m"
    days, hours = divmod(hours, 24)
    return f"{days}d {hours}h"


def _render_dashboard(
    request: Request,
    *,
    flash: dict[str, str] | None = None,
    error: str | None = None,
    pending_storage: str = "",
) -> HTMLResponse:
    templates = Jinja2Templates(directory=request.app.state.templates_dir)
    embedded_status = request.app.state.embedded.status()
    return templates.TemplateResponse(
        request,
        "pages/dashboard.html",
        {
            "title": "Dashboard",
            "active": "dashboard",
            "embedded": embedded_status,
            "embedded_default_path": str(
                request.app.state.embedded.default_storage_path
            ),
            "pending_storage": pending_storage,
            "flash": flash,
            "error": error,
        },
    )


@router.get("/", response_class=HTMLResponse)
def dashboard(request: Request) -> HTMLResponse:
    return _render_dashboard(request)


@router.post("/embedded/start", response_class=HTMLResponse)
def post_embedded_start(
    request: Request,
    storage_path: str = Form(""),
) -> HTMLResponse:
    path = storage_path.strip() or None
    try:
        uri = request.app.state.embedded.start(storage_path=path)
    except OSError as exc:
        return _render_dashboard(
            request,
            error=f"Could not start embedded server: {exc}",
            pending_storage=storage_path,
        )
    # Swap the admin app's target to the new embedded server so the
    # rest of the UI talks to it without the user having to flip the
    # /server page.
    try:
        swap_target(request.app, uri)
        flash = {"kind": "ok", "msg": f"Started embedded server at {uri}"}
    except SwapError as exc:
        flash = {
            "kind": "err",
            "msg": (
                f"Started embedded server at {uri}, but couldn't switch the "
                f"admin app to it: {exc}"
            ),
        }
    return _render_dashboard(request, flash=flash)


@router.post("/embedded/stop", response_class=HTMLResponse)
def post_embedded_stop(request: Request) -> HTMLResponse:
    request.app.state.embedded.stop()
    return _render_dashboard(
        request, flash={"kind": "ok", "msg": "Stopped embedded server."}
    )


@router.get("/_partials/dashboard-tiles", response_class=HTMLResponse)
def dashboard_tiles(request: Request) -> HTMLResponse:
    templates = Jinja2Templates(directory=request.app.state.templates_dir)
    mongo = request.app.state.mongo
    try:
        status = mongo.server_status()
        tiles = _tiles_from_status(status)
        error: str | None = None
    except MongoError as exc:
        tiles = []
        error = str(exc)
    return templates.TemplateResponse(
        request,
        "partials/dashboard_tiles.html",
        {"tiles": tiles, "error": error},
    )
