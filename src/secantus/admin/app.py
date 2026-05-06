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

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

import secantus
from secantus.admin.client import MongoFacade
from secantus.admin.middleware import TokenAuthMiddleware
from secantus.admin.routers import dashboard, health

_ADMIN_PKG = Path(__file__).resolve().parent
_STATIC_DIR = _ADMIN_PKG / "static"
_TEMPLATES_DIR = _ADMIN_PKG / "templates"


@asynccontextmanager
async def _lifespan(app: FastAPI) -> AsyncIterator[None]:
    # MongoFacade is set up in ``create_app`` (so tests can construct
    # the app without going through lifespan); we just close it here.
    try:
        yield
    finally:
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

    # Static files first so /static/* lookups don't pay the middleware
    # cost — middleware bypasses /static/ already, but mounting before
    # routers keeps the URL space tidy.
    app.mount("/static", StaticFiles(directory=_STATIC_DIR), name="static")

    app.add_middleware(TokenAuthMiddleware, token=token)

    app.include_router(health.router)
    app.include_router(dashboard.router)

    return app


__all__ = ["create_app"]
