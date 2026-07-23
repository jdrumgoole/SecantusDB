"""Dashboard: one card per managed server + a recent-jobs strip."""

from __future__ import annotations

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse

from secantus.opsboard import registry
from secantus.opsboard.estimates import estimate_for

router = APIRouter()


@router.get("/", response_class=HTMLResponse)
def dashboard(request: Request) -> HTMLResponse:
    journal = request.app.state.journal
    # Opportunistically reap jobs whose process died without recording an exit.
    journal.reap_stale()
    recent, _ = journal.list(limit=10)
    # Per-task time estimate: the median of THIS machine's past successful runs
    # where we have them, else the registry's rough declared figure. The
    # Estimate carries which it is, so the dialog can say so honestly.
    estimates = {
        task.key: estimate_for(journal.completed_durations(task.argv, limit=20), task.est_seconds)
        for target in registry.TARGETS
        for task in target.tasks
    }
    return request.app.state.templates.TemplateResponse(
        request,
        "pages/dashboard.html",
        {
            "title": "Dashboard",
            "active": "dashboard",
            "targets": registry.TARGETS,
            "recent": recent,
            "estimates": estimates,
        },
    )
