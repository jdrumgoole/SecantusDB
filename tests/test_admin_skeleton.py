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


# ---- databases + collections pages -----------------------------------------


async def test_databases_page_lists_dbs(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["alpha"]["c"].insert_one({"_id": 1})
        mc["beta"]["c"].insert_one({"_id": 1})
    finally:
        mc.close()

    r = await http.get("/db", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "alpha" in r.text
    assert "beta" in r.text
    assert 'class="nav-current"' in r.text


async def test_collections_page_lists_with_stats(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc["zoo"]["animals"]
        coll.insert_many([{"name": f"a-{i}"} for i in range(5)])
        mc["zoo"].create_collection("logs", capped=True, size=4096)
    finally:
        mc.close()

    r = await http.get("/db/zoo", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "animals" in r.text
    assert "logs" in r.text
    assert "capped" in r.text


async def test_collections_page_for_empty_db_renders_empty_message(
    http: AsyncClient,
) -> None:
    r = await http.get("/db/no-such-db", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "No collections" in r.text


# ---- collection viewer (Slice 2.2) -----------------------------------------


async def test_collection_viewer_renders_first_page(
    server, http: AsyncClient
) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["viewer_db"]["c"].insert_many([{"_id": i, "name": f"row-{i}"} for i in range(7)])
    finally:
        mc.close()

    r = await http.get(
        "/db/viewer_db/c?page_size=5", headers={HEADER_NAME: "testtoken"}
    )
    assert r.status_code == 200
    # First five docs visible.
    assert "row-0" in r.text
    assert "row-4" in r.text
    # Sixth not on this page.
    assert "row-5" not in r.text
    # Next-page link present.
    assert "Next page" in r.text


async def test_collection_viewer_paginates_to_completion(
    server, http: AsyncClient
) -> None:
    from urllib.parse import parse_qs, urlparse

    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["page_db"]["c"].insert_many([{"_id": i} for i in range(12)])
    finally:
        mc.close()

    seen: list[int] = []
    url = "/db/page_db/c?page_size=5"
    for _ in range(10):  # safety bound
        r = await http.get(url, headers={HEADER_NAME: "testtoken"})
        assert r.status_code == 200
        # Pull the next-page href if present.
        text = r.text
        if "Next page" not in text:
            break
        # Extract the cursor query parameter from the next-page anchor.
        start = text.find('href="/db/page_db/c?')
        end = text.find('"', start + 6)
        href = text[start + 6 : end].replace("&amp;", "&")
        params = parse_qs(urlparse(href).query)
        url = "/db/page_db/c?" + "&".join(
            f"{k}={v[0]}" for k, v in params.items()
        )
    # The last page contains _id 10 and 11 (12 docs / page_size 5 = 3 pages).
    r = await http.get(url, headers={HEADER_NAME: "testtoken"})
    assert "Next page" not in r.text


async def test_collection_viewer_filter_invalid_json_shows_error(
    http: AsyncClient,
) -> None:
    r = await http.get(
        "/db/x/c?filter=not-json", headers={HEADER_NAME: "testtoken"}
    )
    assert r.status_code == 200
    assert "Filter is not valid JSON" in r.text


async def test_collection_viewer_filter_with_id_rejected(
    server, http: AsyncClient
) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["fid_db"]["c"].insert_many([{"_id": i} for i in range(3)])
    finally:
        mc.close()

    r = await http.get(
        '/db/fid_db/c?filter={"_id": 1}',
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "_id" in r.text and "skip-ID pagination" in r.text


async def test_collection_viewer_descending_sort(
    server, http: AsyncClient
) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["desc_db"]["c"].insert_many([{"_id": i, "tag": f"t-{i}"} for i in range(3)])
    finally:
        mc.close()

    r = await http.get(
        "/db/desc_db/c?sort=desc",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    pos2 = r.text.index("t-2")
    pos0 = r.text.index("t-0")
    assert pos2 < pos0


# ---- edit + delete (Slice 2.3) ---------------------------------------------


def _id_token(value):
    from secantus.admin.pagination import encode_doc_id

    return encode_doc_id(value)


async def test_edit_modal_pre_populates_doc(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["edit_db"]["c"].insert_one({"_id": 1, "name": "alice", "age": 30})
    finally:
        mc.close()

    token = _id_token(1)
    r = await http.get(
        f"/db/edit_db/c/docs/{token}/edit",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "alice" in r.text
    assert "Save" in r.text


async def test_replace_doc_updates_and_returns_row(
    server, http: AsyncClient
) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["replace_db"]["c"].insert_one({"_id": 1, "x": 1})
    finally:
        mc.close()

    token = _id_token(1)
    r = await http.post(
        f"/db/replace_db/c/docs/{token}",
        data={"body": '{"_id": 1, "x": 99, "y": "new"}'},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    # Updated row partial is returned with the new field rendered.
    assert "row-" in r.text
    assert "new" in r.text
    # And the actual collection got the update.
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        doc = mc["replace_db"]["c"].find_one({"_id": 1})
        assert doc == {"_id": 1, "x": 99, "y": "new"}
    finally:
        mc.close()


async def test_replace_doc_invalid_json_returns_modal_error(
    server, http: AsyncClient
) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["bad_db"]["c"].insert_one({"_id": 1})
    finally:
        mc.close()

    token = _id_token(1)
    r = await http.post(
        f"/db/bad_db/c/docs/{token}",
        data={"body": "{not-json"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "Extended JSON" in r.text


async def test_replace_doc_immutable_id_enforced(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["imm_db"]["c"].insert_one({"_id": 1})
    finally:
        mc.close()

    token = _id_token(1)
    r = await http.post(
        f"/db/imm_db/c/docs/{token}",
        data={"body": '{"_id": 99, "x": 1}'},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "_id is immutable" in r.text


async def test_edit_on_bogus_token_404s(http: AsyncClient) -> None:
    r = await http.get(
        "/db/x/c/docs/not-a-valid-token-😀/edit",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 404


async def test_delete_confirm_modal_includes_typed_check(
    server, http: AsyncClient
) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["confirm_db"]["things"].insert_one({"_id": 1, "name": "alice"})
    finally:
        mc.close()

    token = _id_token(1)
    r = await http.get(
        f"/db/confirm_db/things/docs/{token}/delete-confirm",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    # The user types the collection name to confirm; UI gates the button
    # on it via Alpine. Server-side test: just verify the contract.
    assert "things" in r.text  # collection name shown
    assert "Type the collection name" in r.text
    assert "hx-delete" in r.text
    assert 'confirm !== \'things\'' in r.text  # Alpine guard wired up


async def test_delete_doc_removes_and_returns_empty(
    server, http: AsyncClient
) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["del_db"]["c"].insert_many([{"_id": 1}, {"_id": 2}])
    finally:
        mc.close()

    token = _id_token(1)
    r = await http.delete(
        f"/db/del_db/c/docs/{token}",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert r.text == ""
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        remaining = [d["_id"] for d in mc["del_db"]["c"].find()]
        assert remaining == [2]
    finally:
        mc.close()
