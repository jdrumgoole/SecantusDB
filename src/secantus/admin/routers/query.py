"""Ad-hoc query page.

Three submission shapes:

* ``find`` — db, coll, filter (Extended JSON), optional sort + projection,
  limit clamped server-side
* ``aggregate`` — db, coll, pipeline (Extended JSON array)
* ``runCommand`` — db, command (Extended JSON object)

Each successful submission is recorded in the per-URI history store so
the page can render a "Recent" panel that re-populates the form on click.

The db / coll inputs are HTML5 ``<input list>`` datalists populated
from the connected target's ``listDatabases`` / ``listCollections``,
so the user can pick from existing namespaces but still type a new
one for collections that don't exist yet.
"""

from __future__ import annotations

import json
from typing import Any

from bson import json_util
from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _parse_json_field(label: str, raw: str, *, required_type=None, default=None):
    """Return (parsed, error). Empty input returns ``default``."""
    if not raw or not raw.strip():
        return default, None
    try:
        parsed = json_util.loads(raw)
    except (ValueError, TypeError) as exc:
        return None, f"{label} is not valid Extended JSON: {exc}"
    if required_type is not None and not isinstance(parsed, required_type):
        kind = required_type.__name__
        return None, f"{label} must be a JSON {kind}."
    return parsed, None


def _serialize_results(rows: list[dict[str, Any]]) -> list[str]:
    return [json_util.dumps(d, indent=2) for d in rows]


@router.get("/query", response_class=HTMLResponse)
def query_page(request: Request) -> HTMLResponse:
    history = request.app.state.history.recent(request.app.state.mongo_uri)
    databases = _list_database_names(request)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/query.html",
        {
            "title": "Query",
            "active": "query",
            "kind": request.query_params.get("kind") or "find",
            "history": history,
            "databases": databases,
            # Form values when re-populating from history (filled by JS).
            "form": {},
            "results": None,
            "errors": [],
            "stats": None,
        },
    )


def _list_database_names(request: Request) -> list[str]:
    """Best-effort list of database names; empty list when unreachable."""
    try:
        return sorted(
            d.get("name", "") for d in request.app.state.mongo.list_databases() if d.get("name")
        )
    except MongoError:
        return []


@router.get("/query/_collections")
def list_collections_for_db(request: Request, db: str) -> dict[str, Any]:
    """JSON helper — Alpine fetches this when the user picks a database."""
    if not db.strip():
        return {"collections": []}
    try:
        rows = request.app.state.mongo.list_collections_with_stats(db)
    except MongoError:
        return {"collections": []}
    return {"collections": sorted(r["name"] for r in rows)}


def _render_results(
    request: Request,
    *,
    kind: str,
    form: dict[str, str],
    results: list[str] | None,
    errors: list[str],
    stats: dict[str, Any] | None,
) -> HTMLResponse:
    templates = _templates(request)
    history = request.app.state.history.recent(request.app.state.mongo_uri)
    databases = _list_database_names(request)
    status = 200 if not errors else 400
    return templates.TemplateResponse(
        request,
        "pages/query.html",
        {
            "title": "Query",
            "active": "query",
            "kind": kind,
            "history": history,
            "databases": databases,
            "form": form,
            "results": results,
            "errors": errors,
            "stats": stats,
        },
        status_code=status,
    )


@router.post("/query/find", response_class=HTMLResponse)
def run_find(
    request: Request,
    db: str = Form(""),
    coll: str = Form(""),
    filter: str = Form(""),  # noqa: A002 — matches the form field name
    sort: str = Form(""),
    projection: str = Form(""),
    limit: int = Form(50),
) -> HTMLResponse:
    form = {
        "db": db,
        "coll": coll,
        "filter": filter,
        "sort": sort,
        "projection": projection,
        "limit": str(limit),
    }
    errors: list[str] = []

    if not db.strip():
        errors.append("Database is required.")
    if not coll.strip():
        errors.append("Collection is required.")

    filter_doc, err = _parse_json_field("Filter", filter, required_type=dict, default={})
    if err:
        errors.append(err)
    sort_doc, err = _parse_json_field("Sort", sort, required_type=dict, default=None)
    if err:
        errors.append(err)
    projection_doc, err = _parse_json_field(
        "Projection", projection, required_type=dict, default=None
    )
    if err:
        errors.append(err)

    if errors:
        return _render_results(
            request, kind="find", form=form, results=None, errors=errors, stats=None
        )

    try:
        rows = request.app.state.mongo.run_find(
            db,
            coll,
            filter_doc=filter_doc,
            sort=sort_doc,
            projection=projection_doc,
            limit=limit,
        )
    except MongoError as exc:
        return _render_results(
            request, kind="find", form=form, results=None, errors=[str(exc)], stats=None
        )

    request.app.state.history.record(
        request.app.state.mongo_uri,
        "find",
        json.dumps(form),
    )
    return _render_results(
        request,
        kind="find",
        form=form,
        results=_serialize_results(rows),
        errors=[],
        stats={"count": len(rows), "ns": f"{db}.{coll}"},
    )


@router.post("/query/aggregate", response_class=HTMLResponse)
def run_aggregate(
    request: Request,
    db: str = Form(""),
    coll: str = Form(""),
    pipeline: str = Form(""),
    limit: int = Form(200),
) -> HTMLResponse:
    form = {"db": db, "coll": coll, "pipeline": pipeline, "limit": str(limit)}
    errors: list[str] = []
    if not db.strip():
        errors.append("Database is required.")
    if not coll.strip():
        errors.append("Collection is required.")
    parsed, err = _parse_json_field("Pipeline", pipeline, required_type=list, default=None)
    if err or parsed is None:
        errors.append(err or "Pipeline is required.")
    if errors:
        return _render_results(
            request,
            kind="aggregate",
            form=form,
            results=None,
            errors=errors,
            stats=None,
        )

    try:
        rows = request.app.state.mongo.run_aggregate(db, coll, parsed, limit=limit)
    except MongoError as exc:
        return _render_results(
            request,
            kind="aggregate",
            form=form,
            results=None,
            errors=[str(exc)],
            stats=None,
        )

    request.app.state.history.record(
        request.app.state.mongo_uri,
        "aggregate",
        json.dumps(form),
    )
    return _render_results(
        request,
        kind="aggregate",
        form=form,
        results=_serialize_results(rows),
        errors=[],
        stats={"count": len(rows), "ns": f"{db}.{coll}"},
    )


@router.post("/query/runCommand", response_class=HTMLResponse)
def run_command(
    request: Request,
    db: str = Form(""),
    command: str = Form(""),
) -> HTMLResponse:
    form = {"db": db, "command": command}
    errors: list[str] = []
    if not db.strip():
        errors.append("Database is required.")
    parsed, err = _parse_json_field("Command", command, required_type=dict, default=None)
    if err or parsed is None:
        errors.append(err or "Command is required.")
    if errors:
        return _render_results(
            request,
            kind="runCommand",
            form=form,
            results=None,
            errors=errors,
            stats=None,
        )

    try:
        out = request.app.state.mongo.run_command(db, parsed)
    except MongoError as exc:
        return _render_results(
            request,
            kind="runCommand",
            form=form,
            results=None,
            errors=[str(exc)],
            stats=None,
        )

    request.app.state.history.record(
        request.app.state.mongo_uri,
        "runCommand",
        json.dumps(form),
    )
    return _render_results(
        request,
        kind="runCommand",
        form=form,
        results=[json_util.dumps(out, indent=2)],
        errors=[],
        stats={"count": 1, "ns": db},
    )


@router.get("/query/history/{entry_id}")
def history_entry(request: Request, entry_id: int) -> HTMLResponse:
    """Return a JSON blob with the entry's payload — JS uses it to refill the form."""
    rows = request.app.state.history.recent(request.app.state.mongo_uri, limit=1000)
    for row in rows:
        if row.id == entry_id:
            return HTMLResponse(row.payload, headers={"Content-Type": "application/json"})
    raise HTTPException(status_code=404, detail="history entry not found")
