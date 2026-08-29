"""Databases + collections list pages, plus collection lifecycle.

* ``GET /db`` — table of databases with size + name
* ``GET /db/{db}`` — table of collections in ``db`` with collStats
  summary (count, dataSize, indexSize)
* ``POST /db/{db}/collections`` — ``create``, with options
* ``POST /db/{db}/{coll}/collmod`` — ``collMod``
* ``POST /db/{db}/{coll}/rename`` — ``renameCollection``

The collection viewer for individual documents lives in
``routers/collection.py`` (slice 2.2).

The three lifecycle endpoints take their options as one Extended-JSON
document rather than a field per option. ``create`` and ``collMod`` both
accept an open-ended, server-version-dependent option set (validators,
capped sizing, pre/post images, TTL changes); a fixed set of form fields
would silently cap what the UI can express and go stale the moment the
server learns a new one. The document is forwarded as-is and the
server's own error is what the user sees.
"""

from __future__ import annotations

from typing import Any

from bson import json_util
from fastapi import APIRouter, Form, Request
from fastapi.responses import HTMLResponse, RedirectResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError
from secantus.admin.format import humanize_bytes

router = APIRouter()


def _parse_options(raw: str, *, field: str) -> dict[str, Any]:
    """Parse an Extended-JSON options document. Empty means ``{}``."""
    if not raw or not raw.strip():
        return {}
    try:
        parsed = json_util.loads(raw)
    except (ValueError, TypeError) as exc:
        raise MongoError(f"{field} is not valid Extended JSON: {exc}") from exc
    if not isinstance(parsed, dict):
        raise MongoError(f"{field} must be a JSON object.")
    return parsed


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
            "notice": request.query_params.get("notice") or None,
        },
    )


# ---- collection lifecycle --------------------------------------------------


def _back_to_collections(db: str, *, notice: str) -> RedirectResponse:
    """PRG back to the collection list.

    303 (not the default 307) so the browser re-issues as GET — a 307
    would replay the POST on refresh and run the DDL twice.
    """
    from urllib.parse import quote

    return RedirectResponse(f"/db/{db}?notice={quote(notice)}", status_code=303)


def _collections_with_error(request: Request, db: str, message: str) -> HTMLResponse:
    """Re-render the collection list carrying an error, status 400."""
    templates = _templates(request)
    mongo = request.app.state.mongo
    try:
        colls = mongo.list_collections_with_stats(db)
    except MongoError:
        colls = []
    return templates.TemplateResponse(
        request,
        "pages/collections.html",
        {
            "title": f"Collections in {db}",
            "active": "databases",
            "db_name": db,
            "collections": colls,
            "error": message,
            "notice": None,
        },
        status_code=400,
    )


@router.post("/db/{db}/collections", response_class=HTMLResponse)
def create_collection(
    request: Request,
    db: str,
    name: str = Form(...),
    options: str = Form(""),
) -> HTMLResponse:
    coll = name.strip()
    if not coll:
        return _collections_with_error(request, db, "Collection name is required.")
    try:
        opts = _parse_options(options, field="Options")
        request.app.state.mongo.create_collection(db, coll, options=opts)
    except MongoError as exc:
        return _collections_with_error(request, db, str(exc))
    return _back_to_collections(db, notice=f"Created {db}.{coll}")


@router.post("/db/{db}/{coll}/collmod", response_class=HTMLResponse)
def collmod_collection(
    request: Request,
    db: str,
    coll: str,
    changes: str = Form(...),
) -> HTMLResponse:
    try:
        parsed = _parse_options(changes, field="Changes")
        if not parsed:
            raise MongoError("Changes document is required — collMod with no changes is a no-op.")
        request.app.state.mongo.coll_mod(db, coll, changes=parsed)
    except MongoError as exc:
        return _collections_with_error(request, db, str(exc))
    return _back_to_collections(db, notice=f"Modified {db}.{coll}")


@router.post("/db/{db}/{coll}/rename", response_class=HTMLResponse)
def rename_collection(
    request: Request,
    db: str,
    coll: str,
    target: str = Form(...),
    drop_target: str = Form(""),
) -> HTMLResponse:
    to = target.strip()
    if not to:
        return _collections_with_error(request, db, "Target name is required.")
    try:
        request.app.state.mongo.rename_collection(
            db,
            coll,
            target=to,
            # An unchecked HTML checkbox submits nothing at all, so absence
            # is False; presence is any truthy value the browser sends.
            drop_target=bool(drop_target),
        )
    except MongoError as exc:
        return _collections_with_error(request, db, str(exc))
    return _back_to_collections(db, notice=f"Renamed {db}.{coll} to {to}")
