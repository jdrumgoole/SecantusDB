"""Indexes page + create/drop endpoints + explain visualizer.

* ``GET /db/{db}/{coll}/indexes`` — read-only list with badges
  (multikey / unique / sparse / partial / TTL / 2dsphere / 2d / hashed).
* ``POST /db/{db}/{coll}/indexes`` — create an index from form data.
* ``DELETE /db/{db}/{coll}/indexes/{name}`` — drop an index. ``_id_``
  is refused at the facade layer.
* ``GET /db/{db}/{coll}/explain`` — render the winningPlan from a
  ``find`` explain.
"""

from __future__ import annotations

from typing import Any

from bson import json_util
from fastapi import APIRouter, Form, HTTPException, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin.client import MongoError
from secantus.admin.format import humanize_bytes

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    t = Jinja2Templates(directory=request.app.state.templates_dir)
    t.env.filters["humanize_bytes"] = humanize_bytes
    return t


def _index_rows(request: Request, db: str, coll: str) -> tuple[list[dict[str, Any]], str | None]:
    rows: list[dict[str, Any]] = []
    try:
        for ix in request.app.state.mongo.list_indexes(db, coll):
            rows.append(
                {
                    "name": ix.get("name", ""),
                    "key_str": _format_key_spec(ix.get("key") or {}),
                    "key_raw": json_util.dumps(ix.get("key") or {}),
                    "badges": _index_badges(ix),
                    "droppable": ix.get("name") != "_id_",
                }
            )
    except MongoError as exc:
        return rows, str(exc)
    return rows, None


def _index_badges(idx: dict[str, Any]) -> list[str]:
    """Render the small flag badges shown on each index row.

    Order is fixed so the table reads consistently across renders.
    """
    badges: list[str] = []
    if idx.get("unique"):
        badges.append("unique")
    if idx.get("sparse"):
        badges.append("sparse")
    # No multikey badge: the flag is catalog state that ``listIndexes``
    # doesn't carry on mongod (probed 6.0.16) and no longer carries here.
    # The console talks to any MongoDB over the wire, so it can only badge
    # what the wire reports; multikey-ness shows up in the explain
    # visualiser's ``isMultiKey`` instead.
    if idx.get("partialFilterExpression"):
        badges.append("partial")
    ttl = idx.get("expireAfterSeconds")
    if isinstance(ttl, (int, float)):
        badges.append(f"TTL {int(ttl)}s")
    collation = idx.get("collation")
    if isinstance(collation, dict) and collation.get("locale"):
        strength = collation.get("strength")
        label = f"collation {collation['locale']}"
        if strength is not None:
            label = f"{label}/{strength}"
        badges.append(label)
    key = idx.get("key") or {}
    for v in key.values():
        if v == "2dsphere":
            badges.append("2dsphere")
            break
        if v == "2d":
            badges.append("2d")
            break
        if v == "hashed":
            badges.append("hashed")
            break
    return badges


def _format_key_spec(key: dict[str, Any]) -> str:
    """Render ``{a: 1, b: -1}`` style key specs for the table."""
    parts: list[str] = []
    for k, v in key.items():
        if v == 1:
            parts.append(f"{k}: 1")
        elif v == -1:
            parts.append(f"{k}: -1")
        else:
            parts.append(f"{k}: {v!r}")
    return "{ " + ", ".join(parts) + " }"


@router.get("/db/{db}/{coll}/indexes", response_class=HTMLResponse)
def indexes_page(request: Request, db: str, coll: str) -> HTMLResponse:
    rows, error = _index_rows(request, db, coll)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/indexes.html",
        {
            "title": f"Indexes on {db}.{coll}",
            "active": "databases",
            "db_name": db,
            "coll_name": coll,
            "rows": rows,
            "error": error,
        },
    )


@router.get("/db/{db}/{coll}/indexes/_rows", response_class=HTMLResponse)
def indexes_rows(request: Request, db: str, coll: str) -> HTMLResponse:
    rows, _ = _index_rows(request, db, coll)
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/indexes_rows.html",
        {"db_name": db, "coll_name": coll, "rows": rows},
    )


def _parse_key_form(raw: str) -> tuple[list[tuple[str, Any]] | None, str | None]:
    """Parse the key-spec textarea. Returns ``(parsed, error)``."""
    if not raw or not raw.strip():
        return None, "Key spec is required."
    try:
        parsed = json_util.loads(raw)
    except (ValueError, TypeError) as exc:
        return None, f"Key spec is not valid JSON: {exc}"
    if not isinstance(parsed, dict) or not parsed:
        return None, "Key spec must be a non-empty JSON object."
    return list(parsed.items()), None


@router.post("/db/{db}/{coll}/indexes", response_class=HTMLResponse)
def create_index(
    request: Request,
    db: str,
    coll: str,
    key: str = Form(...),
    name: str = Form(""),
    unique: bool = Form(False),
    sparse: bool = Form(False),
    partial: str = Form(""),
    ttl_seconds: str = Form(""),
    collation: str = Form(""),
) -> HTMLResponse:
    parsed_key, key_err = _parse_key_form(key)
    if key_err is not None:
        raise HTTPException(status_code=400, detail=key_err)
    assert parsed_key is not None  # for the type-checker

    partial_doc: dict[str, Any] | None = None
    if partial.strip():
        try:
            parsed_partial = json_util.loads(partial)
        except (ValueError, TypeError) as exc:
            raise HTTPException(
                status_code=400, detail=f"Partial filter is not valid JSON: {exc}"
            ) from exc
        if not isinstance(parsed_partial, dict):
            raise HTTPException(status_code=400, detail="Partial filter must be a JSON object.")
        partial_doc = parsed_partial

    ttl_int: int | None = None
    if ttl_seconds.strip():
        try:
            ttl_int = int(ttl_seconds)
            if ttl_int < 0:
                raise ValueError
        except ValueError as exc:
            raise HTTPException(
                status_code=400,
                detail="TTL must be a non-negative integer (seconds).",
            ) from exc

    collation_doc: dict[str, Any] | None = None
    if collation.strip():
        try:
            parsed_collation = json_util.loads(collation)
        except (ValueError, TypeError) as exc:
            raise HTTPException(
                status_code=400, detail=f"Collation is not valid JSON: {exc}"
            ) from exc
        if not isinstance(parsed_collation, dict):
            raise HTTPException(status_code=400, detail="Collation must be a JSON object.")
        if not parsed_collation.get("locale"):
            # mongod requires it, and omitting it produces a much less
            # obvious server-side error than saying so here.
            raise HTTPException(
                status_code=400, detail='Collation must include a "locale" (e.g. "en").'
            )
        collation_doc = parsed_collation

    try:
        request.app.state.mongo.create_index(
            db,
            coll,
            parsed_key,
            name=name or None,
            unique=unique,
            sparse=sparse,
            partial_filter_expression=partial_doc,
            expire_after_seconds=ttl_int,
            collation=collation_doc,
        )
    except MongoError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    # ``HX-Trigger: indexes-changed`` fires an event on ``body`` that the
    # indexes-page tbody listens for via ``hx-trigger="indexes-changed
    # from:body"`` and uses to swap its rows in place — keeps the
    # scroll position on long lists. ``Location`` is still set so a
    # non-HTMX submit (rare) round-trips back to the page.
    return HTMLResponse(
        "",
        status_code=200,
        headers={
            "HX-Trigger": "indexes-changed",
            "Location": f"/db/{db}/{coll}/indexes",
        },
    )


@router.get(
    "/db/{db}/{coll}/indexes/{name}/drop-confirm",
    response_class=HTMLResponse,
)
def drop_index_confirm(request: Request, db: str, coll: str, name: str) -> HTMLResponse:
    if name == "_id_":
        raise HTTPException(status_code=400, detail="cannot drop _id_ index")
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "partials/index_drop_modal.html",
        {
            "db_name": db,
            "coll_name": coll,
            "index_name": name,
        },
    )


@router.delete("/db/{db}/{coll}/indexes/{name}", response_class=HTMLResponse)
def drop_index(request: Request, db: str, coll: str, name: str) -> HTMLResponse:
    try:
        request.app.state.mongo.drop_index(db, coll, name)
    except MongoError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    return HTMLResponse("", headers={"HX-Trigger": "indexes-changed"})


# ---- explain visualizer ---------------------------------------------------


def _flatten_plan(plan: Any) -> list[dict[str, Any]]:
    """Walk the winningPlan tree and emit one row per stage.

    Real mongod plans nest ``inputStage`` (1:1) and ``inputStages`` (n-ary).
    A flat list with depth markers is much easier to render than a nested
    tree, and the visualiser shows the depth via indent / connector lines.
    """
    rows: list[dict[str, Any]] = []

    def walk(node: Any, depth: int) -> None:
        if not isinstance(node, dict):
            return
        rows.append(
            {
                "depth": depth,
                "stage": node.get("stage", "?"),
                "details": _stage_details(node),
            }
        )
        if "inputStage" in node:
            walk(node["inputStage"], depth + 1)
        for child in node.get("inputStages") or []:
            walk(child, depth + 1)

    walk(plan, 0)
    return rows


def _stage_details(node: dict[str, Any]) -> dict[str, Any]:
    """Pluck the interesting fields for display, omitting noisy ones."""
    out: dict[str, Any] = {}
    for k in ("indexName", "direction"):
        if k in node:
            out[k] = node[k]
    if "keyPattern" in node:
        out["keyPattern"] = _format_key_spec(node["keyPattern"])
    if "filter" in node and node["filter"]:
        out["filter"] = json_util.dumps(node["filter"])
    return out


@router.get("/db/{db}/{coll}/explain", response_class=HTMLResponse)
def explain_page(request: Request, db: str, coll: str) -> HTMLResponse:
    raw_filter = request.query_params.get("filter", "") or ""
    raw_sort = request.query_params.get("sort", "") or ""
    raw_hint = request.query_params.get("hint", "") or ""

    errors: list[str] = []

    def _parse_optional_json(label: str, raw: str) -> Any:
        if not raw.strip():
            return None
        try:
            return json_util.loads(raw)
        except (ValueError, TypeError) as exc:
            errors.append(f"{label} is not valid JSON: {exc}")
            return None

    filter_doc = _parse_optional_json("Filter", raw_filter) or {}
    sort_doc = _parse_optional_json("Sort", raw_sort)
    hint_value = _parse_optional_json("Hint", raw_hint)
    hint = hint_value

    plan_rows: list[dict[str, Any]] = []
    namespace = f"{db}.{coll}"
    if not errors:
        try:
            explain = request.app.state.mongo.explain_find(
                db,
                coll,
                filter_doc=filter_doc if isinstance(filter_doc, dict) else {},
                sort=sort_doc if isinstance(sort_doc, dict) else None,
                hint=hint,
            )
            qp = explain.get("queryPlanner", {}) or {}
            namespace = qp.get("namespace", namespace)
            plan_rows = _flatten_plan(qp.get("winningPlan"))
        except MongoError as exc:
            errors.append(str(exc))

    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/explain.html",
        {
            "title": f"Explain on {db}.{coll}",
            "active": "databases",
            "db_name": db,
            "coll_name": coll,
            "namespace": namespace,
            "raw_filter": raw_filter,
            "raw_sort": raw_sort,
            "raw_hint": raw_hint,
            "plan_rows": plan_rows,
            "errors": errors,
        },
    )
