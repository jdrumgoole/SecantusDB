"""End-to-end smoke tests for the FastAPI admin skeleton.

Boots an in-process SecantusDB on ``port=0`` + a freshly constructed
admin FastAPI app, drives the app via httpx's ASGI transport (no real
network, no pywebview window). Verifies routing, token enforcement,
cookie hand-off, and template rendering against a live ``serverStatus``.
"""

from __future__ import annotations

import pytest
from httpx import ASGITransport, AsyncClient

import secantus
from secantus import SecantusDBServer
from secantus.admin import create_app
from secantus.admin.middleware import COOKIE_NAME, HEADER_NAME, QUERY_NAME

pytestmark = pytest.mark.anyio


@pytest.fixture
def anyio_backend():
    return "asyncio"


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def app(server: SecantusDBServer):
    app = create_app(mongo_uri=server.uri, token="testtoken")
    yield app
    app.state.mongo.close()


@pytest.fixture
async def http(app):
    async with AsyncClient(
        transport=ASGITransport(app=app),
        base_url="http://testserver",
    ) as c:
        yield c


# ---- /healthz ---------------------------------------------------------------


async def test_healthz_no_token_required(http: AsyncClient) -> None:
    r = await http.get("/healthz")
    assert r.status_code == 200
    body = r.json()
    assert body["ok"] is True
    assert body["version"] == secantus.__version__
    assert body["mongo_ok"] is True


# ---- token enforcement ------------------------------------------------------


async def test_dashboard_without_token_rejected(http: AsyncClient) -> None:
    r = await http.get("/")
    assert r.status_code == 401


async def test_dashboard_with_query_token_ok(http: AsyncClient) -> None:
    r = await http.get(f"/?{QUERY_NAME}=testtoken")
    assert r.status_code == 200
    assert "Dashboard" in r.text


async def test_dashboard_with_header_token_ok(http: AsyncClient) -> None:
    r = await http.get("/", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200


async def test_query_token_sets_cookie(http: AsyncClient) -> None:
    r = await http.get(f"/?{QUERY_NAME}=testtoken")
    assert r.cookies.get(COOKIE_NAME) == "testtoken"


async def test_subsequent_request_uses_cookie(http: AsyncClient) -> None:
    # Same client carries the cookie across calls.
    await http.get(f"/?{QUERY_NAME}=testtoken")
    r = await http.get("/")
    assert r.status_code == 200


async def test_static_files_skip_token(http: AsyncClient) -> None:
    # Static assets need to load before the cookie is set on first paint.
    r = await http.get("/static/css/admin.css")
    assert r.status_code == 200
    assert "tile" in r.text


# ---- dashboard tiles --------------------------------------------------------


async def test_dashboard_tiles_render_serverstatus(http: AsyncClient) -> None:
    r = await http.get(
        "/_partials/dashboard-tiles", headers={HEADER_NAME: "testtoken"}
    )
    assert r.status_code == 200
    # Expect labels for the KPI grid we render today.
    for label in ("Uptime", "Connections", "Inserts", "Queries", "Wire requests"):
        assert label in r.text


async def test_dashboard_tiles_show_error_when_mongo_unreachable() -> None:
    # Point at a port nothing's listening on; the facade's ServerSelectionTimeoutError
    # surfaces as a translated MongoError → "Could not reach server" in the HTML.
    app = create_app(mongo_uri="mongodb://127.0.0.1:1", token="testtoken")
    try:
        async with AsyncClient(
            transport=ASGITransport(app=app),
            base_url="http://testserver",
        ) as c:
            r = await c.get(
                "/_partials/dashboard-tiles", headers={HEADER_NAME: "testtoken"}
            )
            assert r.status_code == 200
            assert "Could not reach server" in r.text
    finally:
        app.state.mongo.close()
