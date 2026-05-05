"""Dashboard route: KPI tiles polled via HTMX every 2 seconds.

The full page (``GET /``) renders the chrome plus an empty tile
container that HTMX immediately backfills via
``GET /_partials/dashboard-tiles``. Same partial is hit on every
2-second tick. Slice 4 will replace the polling with a WebSocket push.
"""

from __future__ import annotations

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError

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


@router.get("/", response_class=HTMLResponse)
def dashboard(request: Request) -> HTMLResponse:
    templates = Jinja2Templates(directory=request.app.state.templates_dir)
    return templates.TemplateResponse(
        request,
        "pages/dashboard.html",
        {"title": "Dashboard"},
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
