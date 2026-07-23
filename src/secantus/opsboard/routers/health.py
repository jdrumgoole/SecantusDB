"""Unauthenticated ``/healthz`` endpoint for the launcher readiness probe."""

from __future__ import annotations

from fastapi import APIRouter, Request

import secantus

router = APIRouter()


@router.get("/healthz")
def healthz(request: Request) -> dict[str, object]:
    journal = request.app.state.journal
    return {
        "ok": True,
        "version": secantus.__version__,
        "repo_root": request.app.state.repo_root,
        "running_jobs": len(journal.running()),
    }
