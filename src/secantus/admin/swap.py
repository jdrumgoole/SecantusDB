"""Live-rebind the admin app's target server.

Exposed via the ``/connection`` page: stops the running sampler thread,
closes the existing pymongo client, swaps the facade for one pointed
at the new URI, and restarts a fresh sampler against the new target.

The swap is serialised on ``app.state.swap_lock`` so concurrent calls
can't interleave half-baked state. WebSocket clients (metrics,
change-stream) keep their queues across the swap — the hub instance
is preserved, only the producer changes.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from secantus.admin.client import MongoError, MongoFacade, display_uri
from secantus.admin.sampler import Sampler

logger = logging.getLogger(__name__)


class SwapError(Exception):
    """Validation or connectivity failure during target swap."""


def swap_target(app: Any, new_uri: str, *, ping: bool = True) -> None:
    """Rebind the FastAPI app to ``new_uri``.

    Steps:

    1. Build a new ``MongoFacade`` for the URI.
    2. Optionally ping it to confirm reachability — fast feedback for
       a typo'd URI before we tear down the existing sampler / facade.
    3. Acquire the swap lock.
    4. Stop the running sampler.
    5. Close the old facade.
    6. Replace ``app.state.mongo`` / ``mongo_uri`` / ``mongo_uri_display``.
    7. Build + start a new sampler against the new facade. Reuses the
       existing hub so subscribed WebSocket clients keep streaming.
    8. Record the URI in the targets store.

    Raises :class:`SwapError` on parse / connectivity failure; in that
    case the app's state is unchanged.
    """
    new_uri = (new_uri or "").strip()
    if not new_uri:
        raise SwapError("URI is required")

    try:
        new_facade = MongoFacade(new_uri)
    except Exception as exc:  # pragma: no cover — pymongo is permissive
        raise SwapError(f"could not parse URI: {exc}") from exc

    if ping:
        result = new_facade.ping()
        if not result.ok:
            new_facade.close()
            raise SwapError(f"could not reach target: {result.detail}")

    # The actual swap mutates several pieces of state; serialise.
    with app.state.swap_lock:
        old_facade = app.state.mongo
        old_sampler: Sampler | None = getattr(app.state, "sampler", None)

        if old_sampler is not None:
            old_sampler.stop()

        try:
            old_facade.close()
        except Exception as exc:  # pragma: no cover — defensive
            logger.warning("failed to close prior facade cleanly: %s", exc)

        app.state.mongo = new_facade
        app.state.mongo_uri = new_uri
        app.state.mongo_uri_display = display_uri(new_uri)

        loop = getattr(app.state, "_swap_loop", None)
        if loop is None:
            try:
                loop = asyncio.get_running_loop()
            except RuntimeError:
                loop = None

        if loop is not None:
            new_sampler = Sampler(
                snapshot_fn=new_facade.server_status,
                hub=app.state.hub,
                loop=loop,
            )
            new_sampler.start()
            app.state.sampler = new_sampler
        else:
            # ``swap_target`` was called outside an event loop (probably a
            # test). Lifespan startup will create the sampler later.
            app.state.sampler = None

    try:
        app.state.targets.record(new_uri)
    except Exception as exc:  # pragma: no cover — telemetry only
        logger.warning("failed to record target in store: %s", exc)


__all__ = ["swap_target", "SwapError", "MongoError"]
