"""Dashboard: one card per managed server + a recent-jobs strip."""

from __future__ import annotations

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse

from secantus.opsboard import registry

router = APIRouter()


@router.get("/", response_class=HTMLResponse)
def dashboard(request: Request) -> HTMLResponse:
    journal = request.app.state.journal
    # Opportunistically reap jobs whose process died without recording an exit.
    journal.reap_stale()
    recent, _ = journal.list(limit=10)
    return request.app.state.templates.TemplateResponse(
        request,
        "pages/dashboard.html",
        {
            "title": "Dashboard",
            "active": "dashboard",
            "targets": registry.TARGETS,
            "recent": recent,
        },
    )
