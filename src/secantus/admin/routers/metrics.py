"""WebSocket route streaming live metrics samples to the dashboard.

Frame protocol (JSON):

* ``{"type": "backlog", "samples": [<sample>, ...]}`` — sent once on
  connect with the recent samples in the sampler's history.
* ``{"type": "tick", "sample": <sample>}`` — one frame per tick after
  the backlog.

Each ``<sample>`` matches the shape produced by
:func:`secantus.admin.sampler.build_sample` minus the internal
``_raw`` field. See that module for the field contract.

Token enforcement is per-handler because ``BaseHTTPMiddleware``
doesn't intercept WebSocket scopes.
"""

from __future__ import annotations

import asyncio
import logging

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from secantus.admin.middleware import verify_websocket_token

router = APIRouter()
logger = logging.getLogger(__name__)


@router.websocket("/ws/metrics")
async def ws_metrics(websocket: WebSocket) -> None:
    app = websocket.app
    expected_token = app.state.token
    if not verify_websocket_token(
        expected=expected_token,
        query_params=websocket.query_params,
        cookies=websocket.cookies,
    ):
        await websocket.close(code=1008)  # 1008 = policy violation
        return

    await websocket.accept()
    sampler = app.state.sampler
    hub = app.state.hub
    queue = hub.subscribe()
    try:
        await websocket.send_json(sampler.backlog_frame())
        while True:
            sample = await queue.get()
            await websocket.send_json({"type": "tick", "sample": sample})
    except WebSocketDisconnect:
        pass
    except asyncio.CancelledError:
        # Server shutdown; let the disconnect propagate cleanly.
        raise
    except Exception:  # pragma: no cover — defensive
        logger.exception("metrics websocket terminated unexpectedly")
    finally:
        hub.unsubscribe(queue)
