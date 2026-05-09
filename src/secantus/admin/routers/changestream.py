"""Change-stream WebSocket tail.

Drives a pymongo ``watch()`` against the target SecantusDB and forwards
each event to the connected admin UI client. Three scopes:

* ``coll`` — ``mc[db][coll].watch()``
* ``db`` — ``mc[db].watch()``
* ``cluster`` — ``mc.watch()``

PyMongo's ``ChangeStream`` is sync; we bridge to async via
``asyncio.to_thread(stream.try_next)`` and a short sleep when no event
is pending. The change stream itself is closed in a ``finally`` so the
upstream cursor isn't left dangling on disconnect.

Frame protocol (JSON):

* ``{"type": "open", "scope": "...", "namespace": "..."}`` — sent once
  on accept so the UI can render a "watching <ns>" header.
* ``{"type": "event", "event": <event>}`` — one frame per change. The
  event includes ``operationType``, ``ns``, ``documentKey``,
  ``fullDocument`` (when present), ``updateDescription`` (for updates),
  and the resume token under ``_id``.
* ``{"type": "error", "message": "..."}`` — fatal error from the
  driver. The connection closes after this frame.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
from typing import Any

from bson import json_util
from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from secantus.admin.middleware import verify_websocket_token

router = APIRouter()
logger = logging.getLogger(__name__)


def _serialize_event(event: dict[str, Any]) -> dict[str, Any]:
    """Convert a pymongo change-stream event dict to a JSON-safe shape.

    BSON values (``ObjectId``, ``Timestamp``, dates, ``Binary``) round-trip
    through Extended JSON so the UI can render them faithfully.
    """
    raw = json_util.dumps(event)
    import json as _json

    return _json.loads(raw)


@router.websocket("/ws/changes/{scope}")
async def ws_changes(
    websocket: WebSocket,
    scope: str,
) -> None:
    app = websocket.app
    if not verify_websocket_token(
        expected=app.state.token,
        query_params=websocket.query_params,
        cookies=websocket.cookies,
    ):
        await websocket.close(code=1008)
        return

    if scope not in ("coll", "db", "cluster"):
        await websocket.close(code=1008)
        return
    db = websocket.query_params.get("db") or ""
    coll = websocket.query_params.get("coll") or ""
    if scope == "coll" and (not db or not coll):
        await websocket.close(code=1008)
        return
    if scope == "db" and not db:
        await websocket.close(code=1008)
        return

    await websocket.accept()

    # Build the change stream against the right target. We hold the raw
    # pymongo client here (not the facade) so we can pick the watch level.
    client = app.state.mongo._get_client()
    if scope == "cluster":
        target = client
        namespace = "<cluster>"
    elif scope == "db":
        target = client[db]
        namespace = db
    else:
        target = client[db][coll]
        namespace = f"{db}.{coll}"

    await websocket.send_json({"type": "open", "scope": scope, "namespace": namespace})

    stream = None
    try:
        stream = await asyncio.to_thread(target.watch)
        while True:
            event = await asyncio.to_thread(stream.try_next)
            if event is None:
                # ``try_next`` returns None when the underlying tailable
                # cursor has no event ready. Yield briefly so we don't
                # tight-loop on a quiet stream.
                await asyncio.sleep(0.5)
                continue
            await websocket.send_json({"type": "event", "event": _serialize_event(dict(event))})
    except WebSocketDisconnect:
        pass
    except asyncio.CancelledError:
        raise
    except Exception as exc:  # pragma: no cover — network/driver failure path
        logger.exception("change-stream tail terminated")
        with contextlib.suppress(Exception):
            await websocket.send_json({"type": "error", "message": str(exc)})
    finally:
        if stream is not None:
            with contextlib.suppress(Exception):
                await asyncio.to_thread(stream.close)


# ---- /changestream page ----------------------------------------------------


from fastapi import Request  # noqa: E402
from fastapi.responses import HTMLResponse  # noqa: E402
from fastapi.templating import Jinja2Templates  # noqa: E402


@router.get("/changestream", response_class=HTMLResponse)
def changestream_page(request: Request) -> HTMLResponse:
    scope = request.query_params.get("scope") or "cluster"
    db = request.query_params.get("db") or ""
    coll = request.query_params.get("coll") or ""
    templates = Jinja2Templates(directory=request.app.state.templates_dir)
    return templates.TemplateResponse(
        request,
        "pages/changestream.html",
        {
            "title": "Change stream",
            "active": "changestream",
            "scope": scope,
            "db_name": db,
            "coll_name": coll,
        },
    )
