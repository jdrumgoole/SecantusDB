"""Dashboard route.

The page chrome renders at ``GET /`` and the live KPI tiles + charts
are pushed from ``ws/metrics`` (handled in ``routers/ws.py``), so this
router is just the page-shell endpoint.
"""

from __future__ import annotations

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

router = APIRouter()


@router.get("/", response_class=HTMLResponse)
def dashboard(request: Request) -> HTMLResponse:
    templates = Jinja2Templates(directory=request.app.state.templates_dir)
    return templates.TemplateResponse(
        request,
        "pages/dashboard.html",
        {
            "title": "Dashboard",
            "active": "dashboard",
        },
    )
