"""Profiler page.

* ``GET /profiler[?db=...]`` — settings form + paged ``system.profile``
  entries for ``db`` (default ``admin``).
* ``POST /profiler[?db=...]`` — apply level / slowms / sampleRate via
  the ``profile`` command.

Entries are rendered with the same Extended-JSON serializer the
collection viewer uses, sorted descending by ``ts`` so the most recent
ops show first.
"""

from __future__ import annotations

from typing import Any

from bson import json_util
from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates
from pymongo.errors import PyMongoError

from secantus.admin.client import MongoError, friendly_error

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _render(
    request: Request,
    *,
    db: str,
    error: str | None = None,
    flash: str | None = None,
    status_code: int = 200,
) -> HTMLResponse:
    state: dict[str, Any] = {"level": 0, "slowms": 100, "sampleRate": 1.0}
    rows: list[dict[str, Any]] = []
    fetch_error: str | None = None
    try:
        state = request.app.state.mongo.get_profile(db)
    except MongoError as exc:
        fetch_error = str(exc)

    try:
        # Pull recent entries, newest-first. The cap is 10 MB so a few
        # hundred entries are typical; keep the page tight at 50.
        client = request.app.state.mongo._get_client()
        rows = [dict(d) for d in client[db]["system.profile"].find().sort("ts", -1).limit(50)]
    except PyMongoError as exc:
        # ``system.profile`` not existing yet (level 0 since boot) is fine —
        # find() on a missing collection returns an empty cursor, no error.
        # But a real PyMongoError (server unreachable, auth failure) needs
        # to surface, not get swallowed as "no entries yet".
        rows = []
        if fetch_error is None:
            fetch_error = friendly_error(exc)

    formatted = [
        {
            "ts": r.get("ts"),
            "op": r.get("op"),
            "ns": r.get("ns"),
            "millis": r.get("millis"),
            "ok": r.get("ok"),
            "user": r.get("user"),
            "errMsg": r.get("errMsg"),
            "json": json_util.dumps(r, indent=2),
        }
        for r in rows
    ]

    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/profiler.html",
        {
            "title": "Profiler",
            "active": "profiler",
            "db_name": db,
            "state": state,
            "rows": formatted,
            "error": error or fetch_error,
            "flash": flash,
        },
        status_code=status_code,
    )


@router.get("/profiler", response_class=HTMLResponse)
def profiler_page(request: Request) -> HTMLResponse:
    db = request.query_params.get("db") or "admin"
    return _render(request, db=db)


@router.post("/profiler", response_class=HTMLResponse)
def update_profiler(
    request: Request,
    level: int = Form(...),
    slowms: int = Form(100),
    sample_rate: float = Form(1.0),
) -> HTMLResponse:
    db = request.query_params.get("db") or "admin"
    if level not in (0, 1, 2):
        raise HTTPException(status_code=400, detail="level must be 0, 1, or 2")
    try:
        request.app.state.mongo.set_profile(db, level=level, slowms=slowms, sample_rate=sample_rate)
    except MongoError as exc:
        return _render(request, db=db, error=str(exc), status_code=400)
    return _render(
        request,
        db=db,
        flash=(
            f"Profile settings applied: level={level}, slowms={slowms}, sampleRate={sample_rate}."
        ),
    )
