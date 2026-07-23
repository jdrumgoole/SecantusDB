"""Release page — the confirm-gated launcher for release-class tasks.

Releases are irreversible and outward-facing: ``release-prepare`` pushes a tag
that triggers publication to PyPI. So this router deliberately does NOT reuse
the generic ``/jobs/start`` path. It enforces, server-side:

1. the task is a known release-class task;
2. a syntactically valid version;
3. a **typed confirmation that exactly matches the version** — not a checkbox,
   not "yes"; you have to type the thing you're about to publish;
4. no blocking readiness failures (wrong branch, dirty tree, out of sync with
   origin, no changelog fragment) unless explicitly overridden.

The board never invents release mechanics — it runs the project's own sanctioned
``invoke`` tasks.
"""

from __future__ import annotations

import re

from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse, RedirectResponse

from secantus.opsboard import readiness, registry, versions

router = APIRouter()

# Same shape release-prepare itself accepts.
_VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:[ab]\d+|rc\d+)?$")

# Release-class tasks that take a version argument.
_VERSIONED = {"py-release-prepare", "py-release-finalize"}


def _checks(request: Request) -> list[readiness.Check]:
    state = request.app.state
    return readiness.collect(
        state.repo_root,
        github=getattr(state, "github", None),
        runner=getattr(state, "readiness_runner", None),
    )


@router.get("/release", response_class=HTMLResponse)
def release_page(request: Request) -> HTMLResponse:
    checks = _checks(request)
    tasks = [task for target in registry.TARGETS for task in target.tasks if task.confirm]
    return request.app.state.templates.TemplateResponse(
        request,
        "pages/release.html",
        {
            "title": "Release",
            "active": "release",
            "checks": checks,
            "blockers": readiness.blockers(checks),
            "tasks": tasks,
            "versions": versions.collect(request.app.state.repo_root),
        },
    )


@router.post("/release/start")
def start_release(
    request: Request,
    task_key: str = Form(...),
    version: str = Form(""),
    confirm: str = Form(""),
    override: str = Form(""),
) -> RedirectResponse:
    resolved = registry.resolve_task(task_key)
    if resolved is None:
        raise HTTPException(status_code=404, detail=f"unknown task {task_key!r}")
    _target, task = resolved
    if not task.confirm:
        raise HTTPException(
            status_code=400,
            detail=f"{task_key!r} is not a release-class task; use /jobs/start",
        )

    version = version.strip()
    confirm = confirm.strip()
    needs_version = task_key in _VERSIONED

    if needs_version:
        if not _VERSION_RE.match(version):
            raise HTTPException(
                status_code=400,
                detail=f"version {version!r} must look like X.Y.Z, X.Y.ZbN or X.Y.ZrcN",
            )
        expected = version
    else:
        expected = task.label

    # The typed confirmation must match exactly — this is the last gate before
    # an irreversible, outward-facing action.
    if confirm != expected:
        raise HTTPException(
            status_code=400,
            detail=f"confirmation must exactly match {expected!r} to proceed",
        )

    checks = _checks(request)
    blocking = readiness.blockers(checks)
    if blocking and override.strip().lower() not in ("yes", "y"):
        names = ", ".join(c.name for c in blocking)
        raise HTTPException(
            status_code=400,
            detail=f"release blocked by readiness checks: {names}",
        )

    argv = list(task.argv)
    if needs_version:
        argv.append(version)
    job = request.app.state.runner.start(argv)
    token = request.app.state.token
    return RedirectResponse(url=f"/jobs/{job.id}?t={token}", status_code=303)
