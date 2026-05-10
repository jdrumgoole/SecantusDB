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
import threading
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

import secantus
from secantus.admin.client import MongoFacade, display_uri
from secantus.admin.history import HistoryStore
from secantus.admin.middleware import TokenAuthMiddleware
from secantus.admin.routers import (
    backup,
    changestream,
    collection,
    connections,
    console,
    dashboard,
    databases,
    extras,
    health,
    indexes,
    maintenance,
    metrics,
    profiler,
    server as server_router,
    users,
)
from secantus.admin.sampler import Hub, Sampler
from secantus.admin.targets import TargetStore

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


_DEFAULT_HISTORY_PATH = Path.home() / ".secantus" / "admin.db"


def create_app(
    *,
    mongo_uri: str,
    token: str,
    history_path: Path | str | None = None,
    backup_root: Path | str | None = None,
) -> FastAPI:
    app = FastAPI(
        title="SecantusDB admin",
        version=secantus.__version__,
        docs_url=None,
        redoc_url=None,
        openapi_url=None,
        lifespan=_lifespan,
    )
    app.state.mongo = MongoFacade(mongo_uri)
    app.state.mongo_uri = mongo_uri
    # Sanitised version for the page-header badge — strips password and
    # trailing query string. Templates read it via
    # ``request.app.state.mongo_uri_display``.
    app.state.mongo_uri_display = display_uri(mongo_uri)
    app.state.templates_dir = _TEMPLATES_DIR
    # Token is exposed on app.state so WS handlers can verify it (the
    # HTTP middleware doesn't see WebSocket scopes, so per-route checks
    # need the same token reference).
    app.state.token = token
    app.state.hub = Hub()
    # Persistent ad-hoc query history (sqlite). Tests pass a per-test
    # path; production defaults to the same dir as the persisted token.
    app.state.history = HistoryStore(history_path or _DEFAULT_HISTORY_PATH)
    # Recently-used target URIs — drives the /connection page's "switch
    # to..." list. Reuses the history DB file so we don't sprout a
    # second sqlite path; the table lives in ``connection_targets``.
    app.state.targets = TargetStore(history_path or _DEFAULT_HISTORY_PATH)
    # Lock that ``swap_target`` uses to serialise reconfigurations of
    # the sampler / facade. Constructed once; held briefly during the
    # rebind.
    app.state.swap_lock = threading.Lock()
    if backup_root is not None:
        app.state.backup_root = Path(backup_root)
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
    app.include_router(changestream.router)
    app.include_router(console.router)
    app.include_router(connections.router)
    app.include_router(profiler.router)
    app.include_router(maintenance.router)
    app.include_router(extras.router)
    app.include_router(backup.router)
    app.include_router(server_router.router)

    return app


__all__ = ["create_app"]
