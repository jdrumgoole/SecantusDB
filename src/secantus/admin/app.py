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
from contextlib import asynccontextmanager, suppress
from pathlib import Path

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import HTMLResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

import secantus
from secantus.admin import capabilities
from secantus.admin.client import MongoFacade, display_uri
from secantus.admin.embedded import EmbeddedServer
from secantus.admin.history import HistoryStore
from secantus.admin.middleware import TokenAuthMiddleware
from secantus.admin.routers import (
    backup,
    changestream,
    collection,
    connections,
    dashboard,
    databases,
    extras,
    health,
    indexes,
    insert,
    maintenance,
    metrics,
    oplog,
    profiler,
    query,
    users,
)
from secantus.admin.routers import (
    server as server_router,
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
    # Probe the target once, off the event loop, to learn which admin
    # features it supports (see secantus.admin.capabilities). Best-effort:
    # an unreachable target leaves the permissive UNKNOWN default in place
    # and a later target swap re-probes.
    with suppress(Exception):
        app.state.capabilities = await loop.run_in_executor(
            None, capabilities.probe, app.state.mongo
        )
    try:
        yield
    finally:
        sampler.stop()
        app.state.mongo.close()
        # Tear down any embedded SecantusDBServer the user spun up
        # from the dashboard. Safe when nothing is running.
        with suppress(Exception):
            app.state.embedded.stop()


_DEFAULT_HISTORY_PATH = Path.home() / ".secantus" / "admin.db"


def create_app(
    *,
    mongo_uri: str,
    token: str,
    history_path: Path | str | None = None,
    backup_root: Path | str | None = None,
    embedded_storage: Path | str | None = None,
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
    # What the current target supports, for UI feature-gating. Starts as
    # the permissive UNKNOWN set (nothing hidden); the lifespan startup
    # probe and every target swap replace it with the real capabilities.
    app.state.capabilities = capabilities.UNKNOWN
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
    # Embedded SecantusDB server controlled from the dashboard. Created
    # in stopped state; user clicks Start to spin up an in-process server.
    app.state.embedded = EmbeddedServer(default_storage_path=embedded_storage)
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
    app.include_router(query.router)
    app.include_router(insert.router)
    app.include_router(connections.router)
    app.include_router(profiler.router)
    app.include_router(maintenance.router)
    app.include_router(oplog.router)
    app.include_router(extras.router)
    app.include_router(backup.router)
    app.include_router(server_router.router)

    @app.exception_handler(RequestValidationError)
    async def _form_validation_handler(
        request: Request, exc: RequestValidationError
    ) -> HTMLResponse:
        """Render a back-aware error page instead of FastAPI's default JSON 422.

        Without this, a missing form field (e.g. blank Run command on
        the /query page) lands on a bare ``{"detail":[{...}]}`` page
        with no chrome — the user is stranded with no way back. We
        render the standard sidebar + a one-line summary + Back link.
        """
        templates = Jinja2Templates(directory=app.state.templates_dir)
        # Pull out the missing field names so the message is more
        # specific than "validation failed".
        missing = sorted(
            {err.get("loc", [""])[-1] for err in exc.errors() if err.get("type") == "missing"}
        )
        if missing:
            summary = (
                "Missing required field"
                + ("s" if len(missing) > 1 else "")
                + ": "
                + ", ".join(missing)
            )
        else:
            summary = "Form validation failed."
        # Best-effort referer for the Back link; falls back to /.
        back = request.headers.get("referer") or "/"
        return templates.TemplateResponse(
            request,
            "pages/error.html",
            {
                "title": "Form error",
                "active": "",
                "summary": summary,
                "back": back,
            },
            status_code=400,
        )

    return app


__all__ = ["create_app"]
