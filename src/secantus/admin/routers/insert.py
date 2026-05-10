"""Insert page.

Single tab, three accepted body shapes:

* a single JSON document (``{...}``)
* a JSON array of documents (``[{...}, {...}]``)
* NDJSON — one document per line, blank lines ignored

All three pass through ``bson.json_util.loads`` so Extended JSON
type wrappers (``$oid``, ``$date``, ``$numberDecimal``, …) are honoured.
The result panel reuses the same ``<pre class="doc-body">`` shell as
/query so the JSON pretty-printer picks it up automatically.
"""

from __future__ import annotations

from typing import Any

from bson import json_util
from fastapi import APIRouter, Form, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _list_database_names(request: Request) -> list[str]:
    try:
        return sorted(
            d.get("name", "") for d in request.app.state.mongo.list_databases() if d.get("name")
        )
    except MongoError:
        return []


def _parse_documents(raw: str) -> tuple[list[dict[str, Any]], str | None]:
    """Accept a single doc, a JSON array, or NDJSON. Returns
    ``(docs, error_message)``. Either ``docs`` is non-empty or
    ``error_message`` is set."""
    text = raw.strip()
    if not text:
        return [], "Document(s) field is required."

    # Try JSON-or-array first — covers the most common cases.
    try:
        parsed = json_util.loads(text)
    except (ValueError, TypeError):
        parsed = None

    if parsed is not None:
        if isinstance(parsed, dict):
            return [parsed], None
        if isinstance(parsed, list):
            if not parsed:
                return [], "Document array is empty."
            for i, d in enumerate(parsed):
                if not isinstance(d, dict):
                    return [], f"Element {i} is not a JSON object."
            return parsed, None
        return [], "Documents must be a JSON object or array of objects."

    # Fall back to NDJSON — one document per line.
    docs: list[dict[str, Any]] = []
    for lineno, line in enumerate(text.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            d = json_util.loads(line)
        except (ValueError, TypeError) as exc:
            return [], f"Line {lineno} is not valid Extended JSON: {exc}"
        if not isinstance(d, dict):
            return [], f"Line {lineno} is not a JSON object."
        docs.append(d)
    if not docs:
        return [], "No documents to insert."
    return docs, None


def _render(
    request: Request,
    *,
    form: dict[str, str] | None = None,
    errors: list[str] | None = None,
    result: dict[str, Any] | None = None,
    status_code: int = 200,
) -> HTMLResponse:
    templates = _templates(request)
    inserted_ids_json: str | None = None
    if result is not None:
        inserted_ids_json = json_util.dumps(
            {
                "acknowledged": True,
                "insertedCount": result["inserted_count"],
                "insertedIds": result["inserted_ids"],
            },
            indent=2,
        )
    return templates.TemplateResponse(
        request,
        "pages/insert.html",
        {
            "title": "Insert",
            "active": "insert",
            "databases": _list_database_names(request),
            "form": form or {},
            "errors": errors or [],
            "result": result,
            "result_json": inserted_ids_json,
        },
        status_code=status_code,
    )


@router.get("/insert", response_class=HTMLResponse)
def insert_page(request: Request) -> HTMLResponse:
    return _render(request)


@router.get("/insert/_collections")
def list_collections_for_db(request: Request, db: str) -> dict[str, Any]:
    if not db.strip():
        return {"collections": []}
    try:
        rows = request.app.state.mongo.list_collections_with_stats(db)
    except MongoError:
        return {"collections": []}
    return {"collections": sorted(r["name"] for r in rows)}


@router.post("/insert", response_class=HTMLResponse)
def post_insert(
    request: Request,
    db: str = Form(""),
    coll: str = Form(""),
    docs: str = Form(""),
) -> HTMLResponse:
    form = {"db": db, "coll": coll, "docs": docs}
    errors: list[str] = []
    if not db.strip():
        errors.append("Database is required.")
    if not coll.strip():
        errors.append("Collection is required.")
    parsed_docs, err = _parse_documents(docs)
    if err:
        errors.append(err)
    if errors:
        return _render(request, form=form, errors=errors, status_code=400)

    try:
        result = request.app.state.mongo.insert_many(db.strip(), coll.strip(), parsed_docs)
    except MongoError as exc:
        return _render(request, form=form, errors=[str(exc)], status_code=400)

    return _render(request, form={"db": db, "coll": coll, "docs": ""}, result=result)
