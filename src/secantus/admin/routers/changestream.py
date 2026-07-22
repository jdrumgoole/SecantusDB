"""Change-stream WebSocket tail.

Drives a pymongo ``watch()`` against the target SecantusDB and forwards
each event to the connected admin UI client. Three scopes:

* ``coll`` — ``mc[db][coll].watch()``
* ``db`` — ``mc[db].watch()``
* ``cluster`` — ``mc.watch()``

PyMongo's ``ChangeStream`` is sync; we bridge to async by racing
``asyncio.to_thread(stream.try_next)`` against a disconnect watcher so a
*quiet* stream notices the client is gone instead of only discovering it
on the next ``send``. The per-poll ``try_next`` is bounded server-side
(``max_await_time_ms``) because ``asyncio.to_thread`` cannot be cancelled
mid-flight — an unbounded poll left orphaned on disconnect would linger.
The change stream itself is closed in a ``finally`` so the upstream cursor
isn't left dangling.

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


# Cap the server-side awaitData wait per ``try_next`` poll. Small enough
# that an orphaned poll thread (left behind when the client disconnects
# mid-``to_thread``) frees quickly rather than lingering under CI load, big
# enough that a quiet stream doesn't tight-loop.
_MAX_AWAIT_MS = 500


async def _wait_for_disconnect(websocket: WebSocket) -> None:
    """Return once the client disconnects.

    The admin change-stream client never sends application frames after
    connect, so we drain ``receive()`` purely to observe the disconnect
    message. Racing this against each ``try_next`` poll lets a quiet stream
    notice a gone client immediately instead of looping until the next event
    (or app shutdown) forces a ``send``.
    """
    try:
        while True:
            message = await websocket.receive()
            if message.get("type") == "websocket.disconnect":
                return
    except WebSocketDisconnect:
        return


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

    stream = None
    disconnect_task = asyncio.ensure_future(_wait_for_disconnect(websocket))
    try:
        # Establish the change stream BEFORE announcing "open". The frame is a
        # client-visible promise that writes from here on will be observed, and
        # ``watch()`` is a round-trip to the server (it opens a cursor). Sending
        # "open" first leaves a window in which a client that writes the moment
        # it sees the frame loses that event forever — the stream's start point
        # is only fixed once ``watch()`` returns. Widen that window with CI load
        # and it stops being theoretical: it is why the ws tests, which do
        # exactly this (receive "open" -> insert -> await the event), waited for
        # an event that was never coming.
        stream = await asyncio.to_thread(target.watch, max_await_time_ms=_MAX_AWAIT_MS)
        await websocket.send_json({"type": "open", "scope": scope, "namespace": namespace})
        while True:
            poll_task = asyncio.ensure_future(asyncio.to_thread(stream.try_next))
            done, _pending = await asyncio.wait(
                {poll_task, disconnect_task},
                return_when=asyncio.FIRST_COMPLETED,
            )
            if disconnect_task in done:
                # Client is gone. Wait for the in-flight poll thread to
                # actually finish (bounded to ~``_MAX_AWAIT_MS`` by the
                # server-side awaitData cap) before breaking — the ``finally``
                # closes the stream from *this* thread, and pymongo cursors
                # aren't thread-safe, so ``try_next`` and ``close`` must never
                # overlap. Cancelling the future wouldn't stop the thread;
                # only awaiting it does. The result/exception is discarded.
                with contextlib.suppress(Exception):
                    await poll_task
                break
            event = poll_task.result()
            if event is None:
                # ``try_next`` returns None when the tailable cursor had no
                # event ready within the server's awaitData window; loop.
                # ``_MAX_AWAIT_MS`` paces this so it isn't a tight loop.
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
        disconnect_task.cancel()
        with contextlib.suppress(Exception):
            await disconnect_task
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
