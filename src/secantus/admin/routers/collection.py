"""Collection viewer + (slice 2.3+) document edit/delete endpoints.

``GET /db/{db}/{coll}`` renders a paged-by-``_id`` table of documents.
Query parameters:

* ``filter`` — Extended JSON; parsed via ``bson.json_util.loads``. May
  not include ``_id`` (see ``secantus.admin.pagination`` for rationale).
* ``sort`` — ``asc`` (default) or ``desc``.
* ``cursor`` — opaque page token from a prior response.
* ``page_size`` — clamped to ``[1, 200]``; default 50.
"""

from __future__ import annotations

from typing import Any

from bson import json_util
from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import DEFAULT_PAGE_SIZE, MongoError
from secantus.admin.format import humanize_bytes
from secantus.admin.pagination import decode_cursor, decode_doc_id, encode_doc_id

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    t = Jinja2Templates(directory=request.app.state.templates_dir)
    t.env.filters["humanize_bytes"] = humanize_bytes
    return t


def _parse_filter(raw: str) -> tuple[dict[str, Any], str | None]:
    """Return (filter_doc, error_message)."""
    if not raw or not raw.strip():
        return {}, None
    try:
        parsed = json_util.loads(raw)
    except (ValueError, TypeError) as exc:
        return {}, f"Filter is not valid JSON: {exc}"
    if not isinstance(parsed, dict):
        return {}, "Filter must be a JSON object."
    return parsed, None


def _clamp_page_size(raw: int | None) -> int:
    if raw is None:
        return DEFAULT_PAGE_SIZE
    try:
        n = int(raw)
    except (TypeError, ValueError):
        return DEFAULT_PAGE_SIZE
    return max(1, min(n, 200))


@router.get("/db/{db}/{coll}", response_class=HTMLResponse)
def view_collection(
    request: Request,
    db: str,
    coll: str,
) -> HTMLResponse:
    raw_filter = request.query_params.get("filter", "") or ""
    raw_sort = request.query_params.get("sort", "asc") or "asc"
    raw_cursor = request.query_params.get("cursor")
    raw_page_size = request.query_params.get("page_size")

    sort_dir = -1 if raw_sort == "desc" else 1
    page_size = _clamp_page_size(raw_page_size)

    filter_doc, filter_error = _parse_filter(raw_filter)

    cursor = None
    cursor_error: str | None = None
    if raw_cursor and not filter_error:
        try:
            cursor = decode_cursor(raw_cursor)
        except ValueError as exc:
            cursor_error = f"Invalid page cursor — restarting from the first page ({exc})."

    rows: list[dict[str, Any]] = []
    next_cursor: str | None = None
    runtime_error: str | None = None
    if filter_error is None:
        try:
            rows, next_cursor = request.app.state.mongo.paged_collection(
                db,
                coll,
                filter_doc=filter_doc,
                sort_dir=sort_dir,
                cursor=cursor,
                page_size=page_size,
            )
        except ValueError as exc:
            runtime_error = str(exc)
        except MongoError as exc:
            runtime_error = str(exc)

    # Pretty-print each doc's Extended JSON so ObjectId / dates render
    # round-trippably. ``json_options=None`` lets pymongo pick its default
    # (relaxed extended JSON), readable for humans and parseable on
    # round-trip via json_util.loads in slice 2.3.
    formatted_rows = [
        {
            "id": row.get("_id"),
            "id_token": encode_doc_id(row["_id"]) if "_id" in row else None,
            "json": json_util.dumps(row, indent=2),
        }
        for row in rows
    ]

    error_lines = [m for m in (filter_error, cursor_error, runtime_error) if m]

    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/collection.html",
        {
            "title": f"{db}.{coll}",
            "active": "databases",
            "db_name": db,
            "coll_name": coll,
            "raw_filter": raw_filter,
            "sort": "desc" if sort_dir == -1 else "asc",
            "page_size": page_size,
            "rows": formatted_rows,
            "next_cursor": next_cursor,
            "is_first_page": not raw_cursor,
            "errors": error_lines,
        },
    )


def _resolve_doc(request: Request, db: str, coll: str, id_token: str) -> tuple[Any, dict[str, Any]]:
    """Decode the URL token and fetch the doc; raise 404 on either failure."""
    try:
        doc_id = decode_doc_id(id_token)
    except ValueError as exc:
        raise HTTPException(status_code=404, detail=f"bad document id token: {exc}") from exc
    try:
        doc = request.app.state.mongo.get_doc(db, coll, doc_id)
    except MongoError as exc:
        raise HTTPException(status_code=502, detail=str(exc)) from exc
    if doc is None:
        raise HTTPException(status_code=404, detail="document not found")
    return doc_id, doc


@router.get(
    "/db/{db}/{coll}/docs/{id_token}/edit",
    response_class=HTMLResponse,
)
def edit_doc_form(request: Request, db: str, coll: str, id_token: str) -> HTMLResponse:
    doc_id, doc = _resolve_doc(request, db, coll, id_token)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/doc_edit_modal.html",
        {
            "db_name": db,
            "coll_name": coll,
            "id_token": id_token,
            "doc_id": doc_id,
            "doc_json": json_util.dumps(doc, indent=2),
            "error": None,
        },
    )


@router.post(
    "/db/{db}/{coll}/docs/{id_token}",
    response_class=HTMLResponse,
)
def replace_doc(
    request: Request,
    db: str,
    coll: str,
    id_token: str,
    body: str = Form(...),
) -> HTMLResponse:
    try:
        doc_id = decode_doc_id(id_token)
    except ValueError as exc:
        raise HTTPException(status_code=404, detail=f"bad document id token: {exc}") from exc

    templates = _templates(request)

    def _modal_with_error(msg: str, body_text: str) -> HTMLResponse:
        return templates.TemplateResponse(
            request,
            "partials/doc_edit_modal.html",
            {
                "db_name": db,
                "coll_name": coll,
                "id_token": id_token,
                "doc_id": doc_id,
                "doc_json": body_text,
                "error": msg,
            },
            status_code=400,
        )

    try:
        new_doc = json_util.loads(body)
    except (ValueError, TypeError) as exc:
        return _modal_with_error(f"Document is not valid Extended JSON: {exc}", body)
    if not isinstance(new_doc, dict):
        return _modal_with_error("Document must be a JSON object.", body)
    if "_id" in new_doc and new_doc["_id"] != doc_id:
        return _modal_with_error("_id is immutable.", body)
    new_doc.setdefault("_id", doc_id)

    try:
        matched = request.app.state.mongo.replace_doc(db, coll, doc_id, new_doc)
    except (ValueError, MongoError) as exc:
        return _modal_with_error(str(exc), body)
    if matched == 0:
        return _modal_with_error("Document no longer exists.", body)

    # Return the updated row partial; HTMX swaps it into the table.
    return templates.TemplateResponse(
        request,
        "partials/doc_row.html",
        {
            "db_name": db,
            "coll_name": coll,
            "row": {
                "id": doc_id,
                "id_token": id_token,
                "json": json_util.dumps(new_doc, indent=2),
            },
        },
        headers={"HX-Trigger": "doc-saved"},
    )


@router.get(
    "/db/{db}/{coll}/docs/{id_token}/delete-confirm",
    response_class=HTMLResponse,
)
def delete_doc_confirm(request: Request, db: str, coll: str, id_token: str) -> HTMLResponse:
    doc_id, doc = _resolve_doc(request, db, coll, id_token)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/doc_delete_modal.html",
        {
            "db_name": db,
            "coll_name": coll,
            "id_token": id_token,
            "doc_id": doc_id,
            "doc_json": json_util.dumps(doc, indent=2),
        },
    )


@router.delete(
    "/db/{db}/{coll}/docs/{id_token}",
    response_class=HTMLResponse,
)
def delete_doc(
    request: Request,
    db: str,
    coll: str,
    id_token: str,
) -> HTMLResponse:
    try:
        doc_id = decode_doc_id(id_token)
    except ValueError as exc:
        raise HTTPException(status_code=404, detail=f"bad document id token: {exc}") from exc
    try:
        request.app.state.mongo.delete_doc(db, coll, doc_id)
    except MongoError as exc:
        raise HTTPException(status_code=502, detail=str(exc)) from exc
    # Empty body — HTMX with hx-swap="outerHTML" then removes the row.
    return HTMLResponse("", headers={"HX-Trigger": "doc-deleted"})
