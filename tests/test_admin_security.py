"""Security regressions for the admin UI.

- Stored XSS via document `_id` rendered into the geo page's inline
  `<script>` (issue #115).
- Token-middleware bypass via a spoofed Host header
  (CVE-2026-48710 "BadHost", issue #114).
"""

from __future__ import annotations

import pytest
from httpx import ASGITransport, AsyncClient
from pymongo import MongoClient

from secantus import SecantusDBServer
from secantus.admin import create_app
from secantus.admin.middleware import HEADER_NAME

pytestmark = pytest.mark.anyio


@pytest.fixture
def anyio_backend():
    return "asyncio"


@pytest.fixture
def server(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as srv:
        yield srv


@pytest.fixture
def app(server: SecantusDBServer, tmp_path):
    app = create_app(
        mongo_uri=server.uri,
        token="testtoken",
        history_path=tmp_path / "history.db",
        backup_root=tmp_path / "backups",
    )
    yield app
    app.state.mongo.close()


@pytest.fixture
async def http(app):
    async with AsyncClient(
        transport=ASGITransport(app=app),
        base_url="http://testserver",
    ) as c:
        yield c


_XSS_ID = "</script><script>alert(document.cookie)//"


async def test_geo_page_escapes_malicious_document_id(server, http: AsyncClient) -> None:
    """A document whose `_id` carries a `</script>` payload must be escaped
    when the geo viewer injects feature data into its inline <script>, so the
    payload can't break out of the block (stored XSS, issue #115)."""
    client = MongoClient(server.uri)
    try:
        coll = client["xss_db"]["locs"]
        coll.create_index([("loc", "2dsphere")])
        coll.insert_one({"_id": _XSS_ID, "loc": {"type": "Point", "coordinates": [0.0, 0.0]}})
    finally:
        client.close()

    r = await http.get("/db/xss_db/locs/geo", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    # The raw injection must NOT appear in the page source...
    assert _XSS_ID not in r.text
    assert "<script>alert(document.cookie)" not in r.text
    # ...and the value must be present in its escaped \uXXXX form instead.
    assert "\\u003c/script\\u003e" in r.text


async def test_geo_page_without_token_rejected(http: AsyncClient) -> None:
    """The geo route is token-gated like every other data page."""
    r = await http.get("/db/xss_db/locs/geo")
    assert r.status_code == 401


async def test_spoofed_host_cannot_bypass_token(http: AsyncClient) -> None:
    """A Host header carrying path separators (`Host: x/healthz?t=`) must not
    let the token middleware's `/healthz` + `/static/` bypass allowlist match
    a protected route — CVE-2026-48710 "BadHost" (issue #114). The middleware
    reads the ASGI scope path, which is immune to Host spoofing, and
    starlette>=1.0.1 fixes the URL build too. Either way: still 401."""
    for spoof in (
        "127.0.0.1/healthz",
        "127.0.0.1/healthz?t=",
        "evil/static/x",
        "x/healthz#",
    ):
        r = await http.get("/", headers={"host": spoof})
        assert r.status_code == 401, f"Host {spoof!r} bypassed the token check"


async def test_healthz_still_reachable_without_token(http: AsyncClient) -> None:
    """The legitimate bypass for the real `/healthz` path still works."""
    r = await http.get("/healthz")
    assert r.status_code == 200
