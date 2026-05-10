"""Maintenance page.

Five buttons grouped into two zones:

* **Safe** — fsync (force a WiredTiger checkpoint), prune_oplog, prune_ttl.
* **Destructive** — drop database, drop collection, both gated by
  typed-confirmation modals that require the user to type the target
  name verbatim.

Each action POSTs to its own endpoint and re-renders the page with a
flash message describing the outcome.
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _render(
    request: Request,
    *,
    flash: dict[str, str] | None = None,
    error: str | None = None,
) -> HTMLResponse:
    databases: list[dict[str, Any]] = []
    db_error: str | None = None
    try:
        databases = request.app.state.mongo.list_databases()
    except MongoError as exc:
        db_error = str(exc)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/maintenance.html",
        {
            "title": "Maintenance",
            "active": "maintenance",
            "databases": databases,
            "db_error": db_error,
            "flash": flash,
            "error": error,
        },
    )


@router.get("/maintenance", response_class=HTMLResponse)
def maintenance_page(request: Request) -> HTMLResponse:
    return _render(request)


# ---- Safe actions -----------------------------------------------------------


@router.post("/maintenance/fsync", response_class=HTMLResponse)
def post_fsync(request: Request) -> HTMLResponse:
    try:
        out = request.app.state.mongo.fsync()
    except MongoError as exc:
        return _render(request, error=str(exc))
    return _render(
        request,
        flash={"kind": "ok", "msg": f"fsync ok — numFiles={out.get('numFiles', 0)}"},
    )


@router.post("/maintenance/prune-oplog", response_class=HTMLResponse)
def post_prune_oplog(request: Request) -> HTMLResponse:
    try:
        n = request.app.state.mongo.prune_oplog()
    except MongoError as exc:
        return _render(request, error=str(exc))
    return _render(request, flash={"kind": "ok", "msg": f"pruned {n} oplog row(s)"})


@router.post("/maintenance/prune-ttl", response_class=HTMLResponse)
def post_prune_ttl(request: Request) -> HTMLResponse:
    try:
        n = request.app.state.mongo.prune_ttl()
    except MongoError as exc:
        return _render(request, error=str(exc))
    return _render(request, flash={"kind": "ok", "msg": f"pruned {n} TTL doc(s)"})


# ---- Destructive actions ----------------------------------------------------


@router.get(
    "/maintenance/drop-database/{db}/confirm",
    response_class=HTMLResponse,
)
def drop_database_confirm(request: Request, db: str) -> HTMLResponse:
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/maintenance_drop_db_modal.html",
        {"db_name": db},
    )


@router.post("/maintenance/drop-database", response_class=HTMLResponse)
def post_drop_database(
    request: Request,
    db: str = Form(...),
) -> HTMLResponse:
    if not db.strip():
        raise HTTPException(status_code=400, detail="db name required")
    try:
        request.app.state.mongo.drop_database(db)
    except MongoError as exc:
        return _render(request, error=str(exc))
    return _render(request, flash={"kind": "ok", "msg": f"dropped database {db}"})


@router.get(
    "/maintenance/drop-collection/{db}/{coll}/confirm",
    response_class=HTMLResponse,
)
def drop_collection_confirm(request: Request, db: str, coll: str) -> HTMLResponse:
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/maintenance_drop_coll_modal.html",
        {"db_name": db, "coll_name": coll},
    )


@router.post("/maintenance/drop-collection", response_class=HTMLResponse)
def post_drop_collection(
    request: Request,
    db: str = Form(...),
    coll: str = Form(...),
) -> HTMLResponse:
    if not db.strip() or not coll.strip():
        raise HTTPException(status_code=400, detail="db and coll required")
    try:
        request.app.state.mongo.drop_collection(db, coll)
    except MongoError as exc:
        return _render(request, error=str(exc))
    return _render(request, flash={"kind": "ok", "msg": f"dropped collection {db}.{coll}"})
