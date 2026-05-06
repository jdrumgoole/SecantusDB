"""FastAPI app factory for the SecantusDB admin UI.

``create_app`` wires up:

* a ``MongoFacade`` for the target SecantusDB,
* token-gate middleware,
* the ``/healthz`` route (unauthenticated),
* the dashboard route (template-rendered, polls serverStatus via HTMX),
* static files at ``/static``.

Tests construct the app directly with ``create_app(mongo_uri=..., token=...)``
and drive it via ``httpx.AsyncClient(transport=ASGITransport(app))``.
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

import secantus
from secantus.admin.client import MongoFacade
from secantus.admin.middleware import TokenAuthMiddleware
from secantus.admin.routers import (
    collection,
    dashboard,
    databases,
    health,
    indexes,
    metrics,
    users,
)
from secantus.admin.sampler import Hub, Sampler

_ADMIN_PKG = Path(__file__).resolve().parent
_STATIC_DIR = _ADMIN_PKG / "static"
_TEMPLATES_DIR = _ADMIN_PKG / "templates"


@asynccontextmanager
async def _lifespan(app: FastAPI) -> AsyncIterator[None]:
    # Bind the asyncio loop the sampler will broadcast onto, then start
    # the polling thread. The hub stays the same instance across the
    # app's lifetime; it was constructed in ``create_app``.
    loop = asyncio.get_running_loop()
    sampler = Sampler(
        snapshot_fn=app.state.mongo.server_status,
        hub=app.state.hub,
        loop=loop,
    )
    app.state.sampler = sampler
    sampler.start()
    try:
        yield
    finally:
        sampler.stop()
        app.state.mongo.close()


def create_app(*, mongo_uri: str, token: str) -> FastAPI:
    app = FastAPI(
        title="SecantusDB admin",
        version=secantus.__version__,
        docs_url=None,
        redoc_url=None,
        openapi_url=None,
        lifespan=_lifespan,
    )
    app.state.mongo = MongoFacade(mongo_uri)
    app.state.templates_dir = _TEMPLATES_DIR
    # Token is exposed on app.state so WS handlers can verify it (the
    # HTTP middleware doesn't see WebSocket scopes, so per-route checks
    # need the same token reference).
    app.state.token = token
    app.state.hub = Hub()
    # Sampler is started inside lifespan so the asyncio loop is live;
    # set to None here for the test path that constructs the app
    # without going through lifespan.
    app.state.sampler = None

    # Static files first so /static/* lookups don't pay the middleware
    # cost — middleware bypasses /static/ already, but mounting before
    # routers keeps the URL space tidy.
    app.mount("/static", StaticFiles(directory=_STATIC_DIR), name="static")

    app.add_middleware(TokenAuthMiddleware, token=token)

    app.include_router(health.router)
    app.include_router(dashboard.router)
    app.include_router(databases.router)
    app.include_router(collection.router)
    app.include_router(indexes.router)
    app.include_router(metrics.router)
    app.include_router(users.router)

    return app


__all__ = ["create_app"]
