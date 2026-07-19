"""Extras: schema sampler / logs viewer / geo viewer pages.

Three small pages that share a router because none of them is large
enough to justify its own module.

* ``GET /db/{db}/{coll}/schema`` — sample N docs and infer field
  schema (paths, types, presence, top values)
* ``GET /logs`` — render ``getLog`` output, polled every 2s via HTMX
* ``GET /db/{db}/{coll}/geo`` — Leaflet map for collections with a
  2dsphere / 2d index
"""

from __future__ import annotations

import datetime as _dt
import re
from typing import Any

from bson import json_util
from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin import capabilities
from secantus.admin.client import MongoError
from secantus.admin.schema import summarize

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


# ---- schema sampler --------------------------------------------------------


def _clamp_size(raw: str | None, *, default: int, lo: int, hi: int) -> int:
    if raw is None or not raw.strip():
        return default
    try:
        n = int(raw)
    except (TypeError, ValueError):
        return default
    return max(lo, min(n, hi))


@router.get("/db/{db}/{coll}/schema", response_class=HTMLResponse)
def schema_page(request: Request, db: str, coll: str) -> HTMLResponse:
    sample_size = _clamp_size(request.query_params.get("sample_size"), default=100, lo=1, hi=1000)
    error: str | None = None
    summary: dict[str, Any] = {"sample_size": 0, "fields": []}
    try:
        docs = request.app.state.mongo.sample_collection(db, coll, size=sample_size)
        summary = summarize(docs)
    except MongoError as exc:
        error = str(exc)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/schema.html",
        {
            "title": f"Schema · {db}.{coll}",
            "active": "databases",
            "db_name": db,
            "coll_name": coll,
            "sample_size": sample_size,
            "summary": summary,
            "error": error,
        },
    )


# ---- logs viewer -----------------------------------------------------------


@router.get("/logs", response_class=HTMLResponse)
def logs_page(request: Request) -> HTMLResponse:
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/logs.html",
        {"title": "Logs", "active": "logs"},
    )


@router.get("/_partials/logs", response_class=HTMLResponse)
def logs_partial(request: Request) -> HTMLResponse:
    error: str | None = None
    lines: list[str] = []
    total = 0
    try:
        out = request.app.state.mongo.get_log("global")
        lines = list(out.get("log") or [])
        total = int(out.get("totalLinesWritten", 0) or 0)
    except MongoError as exc:
        if capabilities.is_command_not_found(exc):
            capabilities.record_unsupported(request.app, "server_log")
        error = str(exc)
    fetched_at = _dt.datetime.now().strftime("%H:%M:%S")
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/logs_lines.html",
        {"lines": lines, "total": total, "error": error, "fetched_at": fetched_at},
    )


# ---- geo viewer ------------------------------------------------------------


# JSON characters dangerous to drop raw into an inline <script> block: a
# string `_id` containing `</script>` would close the block and inject JS
# (stored XSS, with access to pywebview's js_api). Escaping to \uXXXX keeps
# the value valid JSON (the browser decodes it back) while a literal
# `</script>` / `<!--` can never appear in the page source. U+2028 / U+2029
# are valid in JSON but are JS line terminators, so they're escaped too.
_SCRIPT_JSON_ESCAPES = {
    "<": "\\u003c",
    ">": "\\u003e",
    "&": "\\u0026",
    "\u2028": "\\u2028",
    "\u2029": "\\u2029",
}
_SCRIPT_JSON_RE = re.compile("[<>&\u2028\u2029]")


def _json_for_script(value: Any) -> str:
    """``json_util.dumps`` then escape characters unsafe in an inline
    ``<script>`` so document data can't break out of the block (XSS)."""
    return _SCRIPT_JSON_RE.sub(lambda m: _SCRIPT_JSON_ESCAPES[m.group()], json_util.dumps(value))


def _extract_features(docs: list[dict[str, Any]], geo_field: str) -> list[dict[str, Any]]:
    """Pluck GeoJSON-ish geometry values from each doc, ignoring misses.

    Returns a list of ``{"_id": ..., "geometry": <GeoJSON>}`` so the
    page can build a feature collection on the client side.
    """
    out: list[dict[str, Any]] = []
    for d in docs:
        geom = d.get(geo_field)
        if isinstance(geom, dict) and "type" in geom and "coordinates" in geom:
            out.append({"_id": d.get("_id"), "geometry": geom})
        elif isinstance(geom, list) and len(geom) == 2:
            # Legacy [lng, lat] pair.
            out.append(
                {
                    "_id": d.get("_id"),
                    "geometry": {"type": "Point", "coordinates": geom},
                }
            )
    return out


@router.get("/db/{db}/{coll}/geo", response_class=HTMLResponse)
def geo_page(request: Request, db: str, coll: str) -> HTMLResponse:
    sample_size = _clamp_size(request.query_params.get("sample_size"), default=200, lo=1, hi=1000)
    requested_field = request.query_params.get("field") or None
    error: str | None = None
    geo_indexes: list[dict[str, Any]] = []
    geo_fields: list[str] = []
    features_json = "[]"
    geo_field: str | None = None
    try:
        geo_indexes = request.app.state.mongo.geo_indexes(db, coll)
        # Every distinct ``(name, "2dsphere" | "2d")`` pair the collection
        # has; the page renders a ``<select>`` if there's more than one
        # so users with multiple geo fields can pick which one to view.
        for ix in geo_indexes:
            for k, v in (ix.get("key") or {}).items():
                if v in ("2dsphere", "2d") and k not in geo_fields:
                    geo_fields.append(k)
        if requested_field and requested_field in geo_fields:
            geo_field = requested_field
        elif geo_fields:
            geo_field = geo_fields[0]
        if geo_field is not None:
            docs = request.app.state.mongo.sample_collection(db, coll, size=sample_size)
            features = _extract_features(docs, geo_field)
            features_json = _json_for_script(features)
    except MongoError as exc:
        error = str(exc)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/geo.html",
        {
            "title": f"Geo · {db}.{coll}",
            "active": "databases",
            "db_name": db,
            "coll_name": coll,
            "geo_indexes": geo_indexes,
            "geo_fields": geo_fields,
            "geo_field": geo_field,
            "features_json": features_json,
            "sample_size": sample_size,
            "error": error,
        },
    )
