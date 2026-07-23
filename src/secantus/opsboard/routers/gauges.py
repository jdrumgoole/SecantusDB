"""Gauge matrix: every driver-conformance gauge × every server.

Data-driven from ``registry.GAUGES`` so adding a gauge is a one-line edit there
— the page never carries its own capability table.
"""

from __future__ import annotations

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse

from secantus.opsboard import registry, reports
from secantus.opsboard.estimates import estimate_for

router = APIRouter()


@router.get("/gauges", response_class=HTMLResponse)
def gauges_page(request: Request) -> HTMLResponse:
    journal = request.app.state.journal
    journal.reap_stale()

    # rows[i] = (spec, {server: (task, estimate, last_job)})
    rows = []
    for spec in registry.GAUGES:
        per_server = {}
        for server, _name in registry.SERVERS:
            task = registry.gauge_task(spec.key, server)
            if task is None:  # pragma: no cover - registry is generated
                continue
            est = estimate_for(journal.completed_durations(task.argv, limit=20), task.est_seconds)
            per_server[server] = {
                "task": task,
                "est": est,
                "report": reports.load(request.app.state.repo_root, spec.key, server),
            }
        rows.append({"spec": spec, "servers": per_server})

    return request.app.state.templates.TemplateResponse(
        request,
        "pages/gauges.html",
        {
            "title": "Gauges",
            "active": "gauges",
            "rows": rows,
            "servers": registry.SERVERS,
        },
    )
