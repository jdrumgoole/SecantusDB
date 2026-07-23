"""CI monitor: recent GitHub Actions runs + local version drift.

Tier-1 cross-session tracking — these runs were triggered by whoever pushed,
not necessarily by this board, so a parallel session's CI shows up here too.
"""

from __future__ import annotations

from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse, RedirectResponse

from secantus.opsboard import activity, versions

router = APIRouter()

_DEFAULT_LIMIT = 20


@router.get("/ci", response_class=HTMLResponse)
def ci_page(request: Request, limit: int = _DEFAULT_LIMIT) -> HTMLResponse:
    gh = request.app.state.github
    limit = max(1, min(int(limit), 100))
    runs = gh.recent_runs(limit=limit)
    return request.app.state.templates.TemplateResponse(
        request,
        "pages/ci.html",
        {
            "title": "CI",
            "active": "ci",
            "runs": runs,
            "limit": limit,
            "gh_error": gh.last_error if not runs else None,
            "repo": gh.repo,
            "versions": versions.collect(request.app.state.repo_root),
            "workflows": gh.workflows(),
            "activity": activity.merge(request.app.state.journal.list(limit=15)[0], runs, limit=20),
        },
    )


@router.post("/ci/dispatch")
def dispatch_workflow(
    request: Request,
    workflow: str = Form(...),
    ref: str = Form("main"),
    confirm: str = Form(""),
) -> RedirectResponse:
    """Start a GitHub Actions workflow run.

    Release-class workflows (they publish to PyPI / cut binary releases) require
    a typed confirmation matching the workflow name — the same gate the Release
    page applies, because dispatching one is equally outward-facing.
    """
    gh = request.app.state.github
    known = {w.name: w for w in gh.workflows()}
    wf = known.get(workflow)
    if wf is None:
        raise HTTPException(status_code=404, detail=f"unknown workflow {workflow!r}")
    if wf.release_class and confirm.strip() != wf.name:
        raise HTTPException(
            status_code=400,
            detail=(f"{wf.name!r} publishes; type its exact name to confirm"),
        )
    ok, message = gh.dispatch(wf.name, ref=ref.strip() or "main")
    if not ok:
        raise HTTPException(status_code=502, detail=f"dispatch failed: {message}")
    token = request.app.state.token
    return RedirectResponse(url=f"/ci?t={token}", status_code=303)
