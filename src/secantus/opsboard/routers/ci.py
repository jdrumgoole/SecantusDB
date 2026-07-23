"""CI monitor: recent GitHub Actions runs + local version drift.

Tier-1 cross-session tracking — these runs were triggered by whoever pushed,
not necessarily by this board, so a parallel session's CI shows up here too.
"""

from __future__ import annotations

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse

from secantus.opsboard import versions

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
        },
    )
