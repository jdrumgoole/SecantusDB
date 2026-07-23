"""Jobs: start, paginated history, live log tail, cancel.

Every start funnels through the shared jobkit runner, so these jobs are the
same journaled processes a developer's ``./inv`` produces — the history and log
tail show CLI-started and UI-started runs identically.
"""

from __future__ import annotations

from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse, RedirectResponse

from secantus.jobkit import PASSED
from secantus.opsboard import discovery, registry
from secantus.opsboard.progress import parse_progress

router = APIRouter()

# Tail cap so a multi-megabyte gate log never ships whole on every poll.
_LOG_TAIL_BYTES = 200_000
_PAGE = 50


def _templates(request: Request):  # noqa: ANN202
    return request.app.state.templates


def _fmt_elapsed(seconds: float) -> str:
    total = int(seconds)
    h, rem = divmod(total, 3600)
    m, s = divmod(rem, 60)
    if h:
        return f"{h}:{m:02d}:{s:02d}"
    return f"{m}:{s:02d}"


@router.get("/jobs", response_class=HTMLResponse)
def jobs_page(request: Request, before: int | None = None) -> HTMLResponse:
    journal = request.app.state.journal
    journal.reap_stale()
    # History excludes in-flight jobs so paging through finished work is stable
    # — a running row would otherwise drift between pages as it completes.
    jobs, next_cursor = journal.list(limit=_PAGE, before_id=before, include_running=False)
    running = journal.running()
    template = "partials/job_rows.html" if before is not None else "pages/jobs.html"
    return _templates(request).TemplateResponse(
        request,
        template,
        {
            "title": "Jobs",
            "active": "jobs",
            "jobs": jobs,
            "next_cursor": next_cursor,
            "running": running,
            "running_count": len(running),
            "external": discovery.scan(
                known_pids=[j.host_pid for j in journal.running()], limit=25
            ),
        },
    )


# Hard ceiling on the parallelism factor so the UI can't dispatch an absurd
# number of gauge daemons. CLAUDE.md recommends <= 4 (timing flakes above that).
_MAX_JOBS = 16


@router.post("/jobs/start")
def start_job(
    request: Request,
    task_key: str = Form(...),
    extra: str = Form(""),
    confirm: str = Form(""),
    jobs: str = Form(""),
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
    if task.jobs_option and jobs.strip():
        try:
            n = int(jobs)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail="jobs must be an integer") from exc
        n = max(1, min(n, _MAX_JOBS))
        argv.extend(["--jobs", str(n)])
    if extra.strip():
        argv.extend(extra.split())
    job = request.app.state.runner.start(argv)
    token = request.app.state.token
    return RedirectResponse(url=f"/jobs/{job.id}?t={token}", status_code=303)


@router.get("/jobs/running", response_class=HTMLResponse)
def running_partial(request: Request) -> HTMLResponse:
    """The live 'Running now' block; self-repolls every 3s via HTMX."""
    journal = request.app.state.journal
    journal.reap_stale()
    running = journal.running()
    return _templates(request).TemplateResponse(
        request,
        "partials/running.html",
        {"running": running, "running_count": len(running)},
    )


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


@router.get("/jobs/{job_id}/view", response_class=HTMLResponse)
def job_view(request: Request, job_id: int) -> HTMLResponse:
    """The graphical job view: overall bar + phase stepper + collapsible log.

    Self-repolls (HTMX) every second while the job runs; the partial drops the
    poll trigger once the job is done.
    """
    journal = request.app.state.journal
    job = journal.get(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="no such job")
    text, _offset, done = request.app.state.runner.tail(job_id, 0)
    task = registry.find_task_by_argv(job.argv)
    labels = task.phase_labels if task and task.phase_labels else None
    prog = parse_progress(
        text,
        known_labels=labels,
        done=done,
        passed=(job.status == PASSED),
    )
    log_text = text
    if len(log_text) > _LOG_TAIL_BYTES:
        log_text = "…(truncated)…\n" + log_text[-_LOG_TAIL_BYTES:]
    return _templates(request).TemplateResponse(
        request,
        "partials/job_view.html",
        {
            "job": job,
            "prog": prog,
            "done": done,
            "log_text": log_text,
            "elapsed": _fmt_elapsed(job.duration),
        },
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


@router.post("/jobs/cancel-all")
def cancel_all_jobs(request: Request) -> RedirectResponse:
    """Stop every running job (and its whole process tree)."""
    request.app.state.runner.cancel_all()
    token = request.app.state.token
    return RedirectResponse(url=f"/jobs?t={token}", status_code=303)


@router.post("/jobs/{job_id}/cancel")
def cancel_job(request: Request, job_id: int) -> RedirectResponse:
    request.app.state.runner.cancel(job_id)
    token = request.app.state.token
    return RedirectResponse(url=f"/jobs/{job_id}?t={token}", status_code=303)
