"""Jobs: start, paginated history, live log tail, cancel.

Every start funnels through the shared jobkit runner, so these jobs are the
same journaled processes a developer's ``./inv`` produces — the history and log
tail show CLI-started and UI-started runs identically.
"""

from __future__ import annotations

from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse, RedirectResponse

from secantus.opsboard import registry

router = APIRouter()

# Tail cap so a multi-megabyte gate log never ships whole on every poll.
_LOG_TAIL_BYTES = 200_000
_PAGE = 50


def _templates(request: Request):  # noqa: ANN202
    return request.app.state.templates


@router.get("/jobs", response_class=HTMLResponse)
def jobs_page(request: Request, before: int | None = None) -> HTMLResponse:
    journal = request.app.state.journal
    journal.reap_stale()
    jobs, next_cursor = journal.list(limit=_PAGE, before_id=before)
    template = "partials/job_rows.html" if before is not None else "pages/jobs.html"
    return _templates(request).TemplateResponse(
        request,
        template,
        {
            "title": "Jobs",
            "active": "jobs",
            "jobs": jobs,
            "next_cursor": next_cursor,
        },
    )


@router.post("/jobs/start")
def start_job(
    request: Request,
    task_key: str = Form(...),
    extra: str = Form(""),
    confirm: str = Form(""),
) -> RedirectResponse:
    resolved = registry.resolve_task(task_key)
    if resolved is None:
        raise HTTPException(status_code=404, detail=f"unknown task {task_key!r}")
    _target, task = resolved
    if task.confirm and confirm.strip().lower() not in ("yes", "y"):
        # Outward-facing / irreversible tasks require an explicit typed "yes".
        raise HTTPException(
            status_code=400,
            detail=f"task {task.key!r} is release-class; resubmit with confirm=yes",
        )
    argv = list(task.argv)
    if extra.strip():
        argv.extend(extra.split())
    job = request.app.state.runner.start(argv)
    token = request.app.state.token
    return RedirectResponse(url=f"/jobs/{job.id}?t={token}", status_code=303)


@router.get("/jobs/{job_id}", response_class=HTMLResponse)
def job_detail(request: Request, job_id: int) -> HTMLResponse:
    job = request.app.state.journal.get(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="no such job")
    return _templates(request).TemplateResponse(
        request,
        "pages/job_detail.html",
        {"title": f"Job #{job.id}", "active": "jobs", "job": job},
    )


@router.get("/jobs/{job_id}/log", response_class=HTMLResponse)
def job_log(request: Request, job_id: int) -> HTMLResponse:
    """Return the log-tail partial. Self-repolls (HTMX) only while running."""
    runner = request.app.state.runner
    text, _offset, done = runner.tail(job_id, 0)
    if len(text) > _LOG_TAIL_BYTES:
        text = "…(truncated)…\n" + text[-_LOG_TAIL_BYTES:]
    job = request.app.state.journal.get(job_id)
    return _templates(request).TemplateResponse(
        request,
        "partials/log_box.html",
        {"job_id": job_id, "log_text": text, "done": done, "job": job},
    )


@router.post("/jobs/{job_id}/cancel")
def cancel_job(request: Request, job_id: int) -> RedirectResponse:
    request.app.state.runner.cancel(job_id)
    token = request.app.state.token
    return RedirectResponse(url=f"/jobs/{job_id}?t={token}", status_code=303)
