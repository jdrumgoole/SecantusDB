"""Databases + collections list pages.

* ``GET /db`` — table of databases with size + name
* ``GET /db/{db}`` — table of collections in ``db`` with collStats
  summary (count, dataSize, indexSize)

The collection viewer for individual documents lives in
``routers/collection.py`` (slice 2.2).
"""

from __future__ import annotations

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError
from secantus.admin.format import humanize_bytes

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    t = Jinja2Templates(directory=request.app.state.templates_dir)
    t.env.filters["humanize_bytes"] = humanize_bytes
    return t


@router.get("/db", response_class=HTMLResponse)
def list_databases_page(request: Request) -> HTMLResponse:
    templates = _templates(request)
    mongo = request.app.state.mongo
    try:
        dbs = mongo.list_databases()
        error: str | None = None
    except MongoError as exc:
        dbs = []
        error = str(exc)
    return templates.TemplateResponse(
        request,
        "pages/databases.html",
        {
            "title": "Databases",
            "active": "databases",
            "databases": dbs,
            "error": error,
        },
    )


@router.get("/db/{db}", response_class=HTMLResponse)
def list_collections_page(request: Request, db: str) -> HTMLResponse:
    templates = _templates(request)
    mongo = request.app.state.mongo
    try:
        colls = mongo.list_collections_with_stats(db)
        error: str | None = None
    except MongoError as exc:
        colls = []
        error = str(exc)
    return templates.TemplateResponse(
        request,
        "pages/collections.html",
        {
            "title": f"Collections in {db}",
            "active": "databases",
            "db_name": db,
            "collections": colls,
            "error": error,
        },
    )
