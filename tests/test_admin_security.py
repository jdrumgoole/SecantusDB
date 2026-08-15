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
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
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


async def test_collections_page_alpine_directives_use_row_indices(
    server, http: AsyncClient
) -> None:
    """A collection name with a quote must not reach the Alpine.js expression
    context (issue #835): Jinja's HTML-entity escaping is undone by the
    browser's attribute decoding before Alpine compiles the string with
    ``new Function``, so `'` in a name broke out of the toggle-key literal
    and executed script on page render. The toggle keys are row indices now —
    no attacker-controlled string in any directive."""
    evil = "x') ? alert(document.cookie) : (open === 'y"
    client = MongoClient(server.uri)
    try:
        client["xss_db"][evil].insert_one({"_id": 1})
    finally:
        client.close()

    r = await http.get("/db/xss_db", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    import re

    # The security property: no Alpine directive (@click / x-show) may carry
    # the collection name — toggle keys are integer row indices, so the name
    # never reaches the `new Function` expression context.
    directives = re.findall(r'(?:@click|x-show)="[^"]*"', r.text)
    assert directives, "expected some Alpine directives on the page"
    for d in directives:
        assert "alert" not in d, f"collection name leaked into directive: {d}"
    # The toggle keys are the row index, not the name.
    assert "'mod-1'" in r.text and "'ren-1'" in r.text
    # The name still renders (escaped) in the row's link/text so the page is
    # usable — just never in a directive.
    assert "&#39;) ? alert(document.cookie)" in r.text


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
