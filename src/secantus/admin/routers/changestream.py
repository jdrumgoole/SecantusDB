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
  driver, or a rejected option. The connection closes after this frame.

Watch options are read from the query string and forwarded to
``watch()``: ``fullDocument``, ``fullDocumentBeforeChange``,
``resumeAfter`` / ``startAfter`` (Extended-JSON resume tokens),
``startAtOperationTime`` (``secs`` or ``secs,ord``), and ``pipeline``
(an Extended-JSON array of aggregation stages). Combinations the server
rejects — more than one start point, say — are *not* pre-screened here;
the driver's error is forwarded verbatim so the UI shows what a real
client would see.
"""

from __future__ import annotations

import asyncio
import contextlib
import functools
import logging
from typing import Any

from bson import Timestamp, json_util
from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from secantus.admin.middleware import verify_websocket_token

router = APIRouter()
logger = logging.getLogger(__name__)


# Cap the server-side awaitData wait per ``try_next`` poll. Small enough
# that an orphaned poll thread (left behind when the client disconnects
# mid-``to_thread``) frees quickly rather than lingering under CI load, big
# enough that a quiet stream doesn't tight-loop.
_MAX_AWAIT_MS = 500


_FULL_DOCUMENT = {"default", "updateLookup", "whenAvailable", "required"}
_FULL_DOCUMENT_BEFORE = {"off", "whenAvailable", "required"}


class WatchOptionError(ValueError):
    """A watch option was malformed. Carries a UI-facing message."""


def _parse_token(raw: str, field: str) -> dict[str, Any]:
    """Parse an Extended-JSON resume token, rejecting non-documents."""
    try:
        value = json_util.loads(raw)
    except Exception as exc:
        raise WatchOptionError(f"{field} is not valid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise WatchOptionError(f"{field} must be a document, got {type(value).__name__}")
    return value


def _parse_operation_time(raw: str) -> Timestamp:
    """Parse ``secs`` or ``secs,ord`` into a BSON ``Timestamp``.

    Extended JSON (``{"$timestamp": {"t": …, "i": …}}``) is accepted too —
    that is what a token copied out of an oplog row looks like.
    """
    if raw.startswith("{"):
        try:
            value = json_util.loads(raw)
        except Exception as exc:
            raise WatchOptionError(f"startAtOperationTime is not valid JSON: {exc}") from exc
        # ``json_util`` decodes ``{"$timestamp": …}`` straight to a
        # ``Timestamp``, so the common case never reaches the dict branch.
        if isinstance(value, Timestamp):
            return value
        if isinstance(value, dict):
            try:
                return Timestamp(int(value["t"]), int(value.get("i", 0)))
            except Exception as exc:
                raise WatchOptionError(
                    "startAtOperationTime document must carry integer 't' (and optional 'i')"
                ) from exc
        raise WatchOptionError(
            f"startAtOperationTime must be a timestamp, got {type(value).__name__}"
        )
    parts = [p.strip() for p in raw.split(",")]
    try:
        secs = int(parts[0])
        ordinal = int(parts[1]) if len(parts) > 1 else 0
    except ValueError as exc:
        raise WatchOptionError(
            "startAtOperationTime must be 'secs' or 'secs,ord' (integers)"
        ) from exc
    if len(parts) > 2:
        raise WatchOptionError("startAtOperationTime takes at most 'secs,ord'")
    return Timestamp(secs, ordinal)


def watch_kwargs(query_params: Any) -> dict[str, Any]:
    """Build the ``watch()`` keyword arguments from the query string.

    Only options the user actually supplied are included, so an untouched
    UI control never overrides a driver default. Raises
    ``WatchOptionError`` with a message meant for display.
    """
    kwargs: dict[str, Any] = {}

    full_document = query_params.get("fullDocument") or ""
    if full_document and full_document != "default":
        if full_document not in _FULL_DOCUMENT:
            raise WatchOptionError(
                f"fullDocument must be one of {sorted(_FULL_DOCUMENT)}, got {full_document!r}"
            )
        kwargs["full_document"] = full_document

    before = query_params.get("fullDocumentBeforeChange") or ""
    if before and before != "off":
        if before not in _FULL_DOCUMENT_BEFORE:
            raise WatchOptionError(
                f"fullDocumentBeforeChange must be one of {sorted(_FULL_DOCUMENT_BEFORE)}, "
                f"got {before!r}"
            )
        kwargs["full_document_before_change"] = before

    if resume_after := (query_params.get("resumeAfter") or "").strip():
        kwargs["resume_after"] = _parse_token(resume_after, "resumeAfter")
    if start_after := (query_params.get("startAfter") or "").strip():
        kwargs["start_after"] = _parse_token(start_after, "startAfter")
    if operation_time := (query_params.get("startAtOperationTime") or "").strip():
        kwargs["start_at_operation_time"] = _parse_operation_time(operation_time)

    if pipeline := (query_params.get("pipeline") or "").strip():
        try:
            stages = json_util.loads(pipeline)
        except Exception as exc:
            raise WatchOptionError(f"pipeline is not valid JSON: {exc}") from exc
        if not isinstance(stages, list):
            raise WatchOptionError("pipeline must be a JSON array of stages")
        if not all(isinstance(stage, dict) for stage in stages):
            raise WatchOptionError("every pipeline stage must be a document")
        kwargs["pipeline"] = stages

    return kwargs


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

    # Parse options AFTER accept so a bad one can be reported as an error
    # frame the UI renders, rather than a bare 1008 close the user can only
    # read as "it didn't work".
    try:
        options = watch_kwargs(websocket.query_params)
    except WatchOptionError as exc:
        with contextlib.suppress(Exception):
            await websocket.send_json({"type": "error", "message": str(exc)})
        await websocket.close(code=1008)
        return

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
        stream = await asyncio.to_thread(
            functools.partial(target.watch, max_await_time_ms=_MAX_AWAIT_MS, **options)
        )
        await websocket.send_json(
            {
                "type": "open",
                "scope": scope,
                "namespace": namespace,
                # Echo what actually took effect so the UI can show that a
                # resume/fullDocument option was honoured, not just requested.
                "options": sorted(options),
            }
        )
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
    params = request.query_params
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
            # Echoed back so a reload (or a shared link) restores the same
            # watch options, not just the same scope.
            "full_document": params.get("fullDocument") or "default",
            "full_document_before_change": params.get("fullDocumentBeforeChange") or "off",
            "resume_after": params.get("resumeAfter") or "",
            "start_after": params.get("startAfter") or "",
            "start_at_operation_time": params.get("startAtOperationTime") or "",
            "pipeline": params.get("pipeline") or "",
        },
    )
