"""FastAPI app factory for the SecantusDB Ops Board.

Tests construct the app directly with ``create_app(repo_root=..., token=...)``
and drive it via ``httpx.AsyncClient(transport=ASGITransport(app))``.
"""

from __future__ import annotations

from pathlib import Path

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

import secantus
from secantus.jobkit import Journal
from secantus.opsboard.github import GitHubClient
from secantus.opsboard.middleware import TokenAuthMiddleware
from secantus.opsboard.routers import ci, dashboard, gauges, health, jobs, release
from secantus.opsboard.runner import JobRunner

_PKG = Path(__file__).resolve().parent
_STATIC_DIR = _PKG / "static"
_TEMPLATES_DIR = _PKG / "templates"


def _default_repo_root() -> Path:
    # The Ops Board runs from a checkout; the repo root is four levels up from
    # this file (src/secantus/opsboard/app.py → repo root).
    return _PKG.parents[2]


def create_app(
    *,
    repo_root: str | Path | None = None,
    token: str,
    journal_path: str | Path | None = None,
    github_client: GitHubClient | None = None,
    readiness_runner: object | None = None,
) -> FastAPI:
    app = FastAPI(
        title="SecantusDB Ops Board",
        version=secantus.__version__,
        docs_url=None,
        redoc_url=None,
        openapi_url=None,
    )
    root = Path(repo_root) if repo_root is not None else _default_repo_root()
    app.state.repo_root = str(root)
    app.state.token = token
    app.state.templates = Jinja2Templates(directory=str(_TEMPLATES_DIR))
    journal = Journal(journal_path)
    app.state.journal = journal
    app.state.runner = JobRunner(repo_root=root, journal=journal)
    # Read-only GitHub Actions observation (Tier-1 cross-session tracking).
    # Injectable so tests never shell out to gh or touch the network.
    app.state.github = github_client or GitHubClient(repo_root=str(root))
    # Injectable git runner for the release readiness checks (tests supply a
    # fake; production uses the real `git`).
    app.state.readiness_runner = readiness_runner

    app.mount("/static", StaticFiles(directory=_STATIC_DIR), name="static")
    app.add_middleware(TokenAuthMiddleware, token=token)

    app.include_router(health.router)
    app.include_router(dashboard.router)
    app.include_router(jobs.router)
    app.include_router(gauges.router)
    app.include_router(ci.router)
    app.include_router(release.router)
    return app


__all__ = ["create_app"]
